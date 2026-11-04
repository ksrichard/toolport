//! Open MCP sessions, so a tool change can reach the clients that are connected now.

use std::{collections::HashMap, sync::Mutex};

use rmcp::{
    RoleServer,
    service::Peer,
    transport::streamable_http_server::{SessionManager, session::local::LocalSessionManager},
};

/// The peers of the open sessions, grouped by server id and keyed by session id.
/// A closed session stays here until the next notification to it fails.
#[derive(Default)]
pub struct SessionPeers {
    peers: Mutex<HashMap<i64, HashMap<String, Peer<RoleServer>>>>,
}

impl SessionPeers {
    pub fn register(&self, server_id: i64, session_id: String, peer: Peer<RoleServer>) {
        if let Ok(mut peers) = self.peers.lock() {
            peers.entry(server_id).or_default().insert(session_id, peer);
        }
    }

    /// Sends `notifications/tools/list_changed` to every open session of the server.
    pub async fn notify_tools_changed(&self, server_id: i64) {
        let open: Vec<(String, Peer<RoleServer>)> = match self.peers.lock() {
            Ok(peers) => peers
                .get(&server_id)
                .map(|sessions| {
                    sessions
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            Err(_) => return,
        };
        for (session_id, peer) in open {
            if peer.notify_tool_list_changed().await.is_err()
                && let Ok(mut peers) = self.peers.lock()
                && let Some(sessions) = peers.get_mut(&server_id)
            {
                sessions.remove(&session_id);
            }
        }
    }

    /// Closes the open sessions of a server and forgets them. A rename or a delete calls it,
    /// because the clients must connect again with the new URL or not at all.
    pub async fn close_server(&self, server_id: i64, manager: &LocalSessionManager) {
        let sessions = match self.peers.lock() {
            Ok(mut peers) => peers.remove(&server_id).unwrap_or_default(),
            Err(_) => return,
        };
        for session_id in sessions.into_keys() {
            if let Err(error) = manager.close_session(&session_id.as_str().into()).await {
                tracing::warn!(%error, server_id, "cannot close an MCP session");
            }
        }
    }
}
