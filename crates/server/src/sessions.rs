//! Open MCP sessions, so a tool change can reach the clients that are connected now.

use std::{collections::HashMap, sync::Mutex};

use rmcp::{RoleServer, service::Peer};

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
}
