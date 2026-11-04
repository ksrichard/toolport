//! Live data for the dashboard: new calls, metrics, and connected clients, sent over SSE.

use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{Mutex, PoisonError},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    extract::{Path, State},
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use ts_rs::TS;

use crate::{
    auth::AuthUser,
    calls::{self, Bucket, CallRecord, Metrics},
    error::{ApiError, error_chain},
    mcp_tokens::McpCaller,
    servers::owned_server_id,
    state::AppState,
};

/// A client counts as connected while it sent an MCP request in this time.
const ACTIVE_MS: i64 = 2 * 60 * 1000;
const METRICS_INTERVAL: Duration = Duration::from_secs(5);
const SNAPSHOT_CALLS: i64 = 50;
/// A Live page that misses more than this many calls gets a new snapshot instead of the calls.
const CALL_BUFFER: usize = 256;

#[derive(Debug, Clone)]
pub struct CallEvent {
    pub server_id: i64,
    pub record: CallRecord,
}

/// What the broadcast channel carries. Each Live stream keeps the events of its own server.
#[derive(Debug, Clone)]
pub enum LiveEvent {
    Call(CallEvent),
    Client { server_id: i64, first: FirstRequest },
}

/// The first MCP request of a new client. The onboarding page shows it as "connected".
#[derive(Debug, Clone, Serialize, PartialEq, TS)]
#[ts(export)]
pub struct FirstRequest {
    pub client_name: Option<String>,
    /// The onboarding page matches its own token by id, because token names are not unique.
    #[ts(type = "number")]
    pub token_id: i64,
    pub token_name: String,
    /// The MCP method, for example `tools/list`.
    pub method: String,
    /// The number of tools in the answer. Only `tools/list` sets it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tools_sent: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ClientKey {
    server_id: i64,
    token_id: i64,
    client_name: Option<String>,
}

struct ClientSeen {
    token_name: String,
    since_ms: i64,
    last_seen_ms: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, TS)]
#[ts(export)]
pub struct ConnectedClient {
    /// The name from the MCP `initialize` request, for example `claude-ai`.
    pub client_name: Option<String>,
    #[ts(type = "number")]
    pub token_id: i64,
    pub token_name: String,
    #[ts(type = "number")]
    pub since_ms: i64,
    #[ts(type = "number")]
    pub last_seen_ms: i64,
}

/// Shared by the MCP handler, which writes, and the SSE streams, which read.
/// It is in memory only, so a server restart empties the client list.
pub struct LiveHub {
    calls: broadcast::Sender<LiveEvent>,
    clients: Mutex<HashMap<ClientKey, ClientSeen>>,
}

impl Default for LiveHub {
    fn default() -> Self {
        Self {
            calls: broadcast::channel(CALL_BUFFER).0,
            clients: Mutex::new(HashMap::new()),
        }
    }
}

impl LiveHub {
    pub fn publish(&self, event: CallEvent) {
        self.send(LiveEvent::Call(event));
    }

    pub fn publish_client(&self, server_id: i64, first: FirstRequest) {
        self.send(LiveEvent::Client { server_id, first });
    }

    fn send(&self, event: LiveEvent) {
        // An error means that no Live page is open. Nobody needs the event then.
        let _ = self.calls.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LiveEvent> {
        self.calls.subscribe()
    }

    /// Returns true when this server, token, and client name were not active before.
    pub fn seen(&self, caller: &McpCaller, client_name: Option<&str>) -> bool {
        let now = now_ms();
        let key = ClientKey {
            server_id: caller.server_id,
            token_id: caller.token_id,
            client_name: client_name.map(str::to_owned),
        };
        let mut clients = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
        clients.retain(|_, seen| now - seen.last_seen_ms < ACTIVE_MS);
        let mut is_new = false;
        let seen = clients.entry(key).or_insert_with(|| {
            is_new = true;
            ClientSeen {
                token_name: caller.token_name.clone(),
                since_ms: now,
                last_seen_ms: now,
            }
        });
        seen.last_seen_ms = now;
        is_new
    }

    /// The connected clients of one server, the oldest first.
    pub fn clients(&self, server_id: i64) -> Vec<ConnectedClient> {
        let now = now_ms();
        let clients = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
        let mut list: Vec<_> = clients
            .iter()
            .filter(|(key, seen)| key.server_id == server_id && now - seen.last_seen_ms < ACTIVE_MS)
            .map(|(key, seen)| ConnectedClient {
                client_name: key.client_name.clone(),
                token_id: key.token_id,
                token_name: seen.token_name.clone(),
                since_ms: seen.since_ms,
                last_seen_ms: seen.last_seen_ms,
            })
            .collect();
        list.sort_by_key(|client| client.since_ms);
        list
    }

    /// The number of connected clients per server id, for the server list.
    pub fn client_counts(&self) -> HashMap<i64, i64> {
        let now = now_ms();
        let clients = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
        let mut counts = HashMap::new();
        for (key, seen) in clients.iter() {
            if now - seen.last_seen_ms < ACTIVE_MS {
                *counts.entry(key.server_id).or_insert(0) += 1;
            }
        }
        counts
    }

    /// Removes the clients of a deleted server.
    pub fn forget_server(&self, server_id: i64) {
        let mut clients = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
        clients.retain(|key, _| key.server_id != server_id);
    }
}

fn now_ms() -> i64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
}

/// The first event of a stream. It is also sent again when the stream misses calls.
#[derive(Serialize, TS)]
#[ts(export)]
struct Snapshot {
    metrics: Metrics,
    clients: Vec<ConnectedClient>,
    buckets: Vec<Bucket>,
    /// The newest calls first. A call can also come again as a `call` event, so the
    /// dashboard must drop duplicates by `id`.
    calls: Vec<CallRecord>,
}

#[derive(Serialize, TS)]
#[ts(export)]
struct MetricsUpdate {
    metrics: Metrics,
    clients: Vec<ConnectedClient>,
}

#[derive(Debug, thiserror::Error)]
enum LiveError {
    #[error("the database query failed")]
    Database(#[from] sqlx::Error),
    #[error("cannot encode the event as JSON")]
    Encode(#[from] axum::Error),
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/servers/{name}/live", get(live))
}

/// The SSE stream of one server. The events are `snapshot`, `call`, `client`, and `metrics`.
/// The browser cannot send a Bearer header with `EventSource`, so the dashboard reads
/// this stream with `fetch`.
async fn live(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;

    // Subscribe before the snapshot query, so no call is lost between the two.
    let calls = state.live.subscribe();
    let first = snapshot(&state, server_id)
        .await
        .map_err(|error| match error {
            LiveError::Database(source) => ApiError::Database(source),
            LiveError::Encode(source) => ApiError::Encode(source),
        })?;

    let (tx, rx) = mpsc::channel(32);
    // The receiver exists, and the buffer is empty, so this send cannot fail.
    let _ = tx.send(Ok(first)).await;
    tokio::spawn(stream_events(state, server_id, calls, tx));
    Ok(Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default()))
}

/// Runs while the dashboard keeps the stream open. It stops when the receiver is gone.
async fn stream_events(
    state: AppState,
    server_id: i64,
    mut calls: broadcast::Receiver<LiveEvent>,
    tx: mpsc::Sender<Result<Event, Infallible>>,
) {
    let mut tick = tokio::time::interval(METRICS_INTERVAL);
    // The first tick is at once, and the snapshot already has the metrics.
    tick.tick().await;
    loop {
        let event = tokio::select! {
            () = tx.closed() => return,
            _ = tick.tick() => metrics_update(&state, server_id).await,
            received = calls.recv() => match received {
                Ok(LiveEvent::Call(call)) if call.server_id == server_id => {
                    Event::default().event("call").json_data(&call.record).map_err(LiveError::from)
                }
                Ok(LiveEvent::Client { server_id: id, first }) if id == server_id => {
                    Event::default().event("client").json_data(&first).map_err(LiveError::from)
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => snapshot(&state, server_id).await,
                Err(broadcast::error::RecvError::Closed) => return,
            },
        };
        match event {
            Ok(event) => {
                if tx.send(Ok(event)).await.is_err() {
                    return;
                }
            }
            Err(error) => {
                tracing::warn!(error = %error_chain(&error), server_id, "live event failed")
            }
        }
    }
}

async fn snapshot(state: &AppState, server_id: i64) -> Result<Event, LiveError> {
    let data = Snapshot {
        metrics: calls::metrics(&state.pool, server_id).await?,
        clients: state.live.clients(server_id),
        buckets: calls::buckets(&state.pool, server_id).await?,
        calls: calls::recent(&state.pool, server_id, SNAPSHOT_CALLS).await?,
    };
    Ok(Event::default().event("snapshot").json_data(&data)?)
}

async fn metrics_update(state: &AppState, server_id: i64) -> Result<Event, LiveError> {
    let data = MetricsUpdate {
        metrics: calls::metrics(&state.pool, server_id).await?,
        clients: state.live.clients(server_id),
    };
    Ok(Event::default().event("metrics").json_data(&data)?)
}
