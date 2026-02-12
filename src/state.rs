use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use webrtc::peer_connection::RTCPeerConnection;

/// Represents a peer's connection state
#[derive(Clone, Debug)]
pub struct PeerState {
    pub nickname: String,
    pub connected: bool,
    pub speaking: bool,
}

/// Shared application state
pub struct AppState {
    /// Map of IRC nickname -> WebRTC peer connection
    pub peers: RwLock<HashMap<String, Arc<RTCPeerConnection>>>,
    
    /// Map of IRC nickname -> peer metadata
    pub peer_states: RwLock<HashMap<String, PeerState>>,
    
    /// Chat message history
    pub messages: RwLock<Vec<String>>,
}

impl AppState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            peers: RwLock::new(HashMap::new()),
            peer_states: RwLock::new(HashMap::new()),
            messages: RwLock::new(Vec::new()),
        })
    }

    pub async fn add_message(&self, msg: String) {
        let mut messages = self.messages.write().await;
        messages.push(msg);
        // Keep last 1000 messages
        if messages.len() > 1000 {
            messages.drain(0..100);
        }
    }

    pub async fn update_peer_state(&self, nick: String, connected: bool, speaking: bool) {
        let mut states = self.peer_states.write().await;
        states.insert(
            nick.clone(),
            PeerState {
                nickname: nick,
                connected,
                speaking,
            },
        );
    }

    pub async fn remove_peer(&self, nick: &str) {
        self.peers.write().await.remove(nick);
        self.peer_states.write().await.remove(nick);
    }
}
