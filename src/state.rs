use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use webrtc::peer_connection::RTCPeerConnection;

#[derive(Clone, Debug)]
pub struct PeerState {
    pub nickname: String,
    pub connected: bool,
    pub speaking: bool,
}

#[derive(Clone, Debug)]
pub struct SharedFile {
    pub from: String,
    pub name: String,
    pub size: usize,
    pub path: PathBuf,
}

pub struct AppState {
    pub peers: RwLock<HashMap<String, Arc<RTCPeerConnection>>>,
    pub peer_states: RwLock<HashMap<String, PeerState>>,
    pub messages: RwLock<HashMap<String, Vec<String>>>,
    pub last_audio: RwLock<HashMap<String, Instant>>,
    pub received_files: RwLock<Vec<SharedFile>>,
}

impl AppState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            peers: RwLock::new(HashMap::new()),
            peer_states: RwLock::new(HashMap::new()),
            messages: RwLock::new(HashMap::new()),
            last_audio: RwLock::new(HashMap::new()),
            received_files: RwLock::new(Vec::new()),
        })
    }

    pub async fn add_message(&self, channel: &str, msg: String) {
        let mut messages = self.messages.write().await;
        let list = messages.entry(channel.to_string()).or_default();
        list.push(msg);
        if list.len() > 1000 {
            list.drain(0..100);
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

    pub async fn mark_speaking(&self, nick: &str) {
        self.last_audio.write().await.insert(nick.to_string(), Instant::now());
    }

    pub async fn refresh_speaking(&self) {
        let last = self.last_audio.read().await;
        let mut states = self.peer_states.write().await;
        for (nick, ps) in states.iter_mut() {
            ps.speaking = last
                .get(nick)
                .map(|t| t.elapsed() < Duration::from_millis(400))
                .unwrap_or(false);
        }
    }

    pub async fn remove_peer(&self, nick: &str) {
        self.peers.write().await.remove(nick);
        self.peer_states.write().await.remove(nick);
        self.last_audio.write().await.remove(nick);
    }

    /// Drop all peer state (used when switching channels)
    pub async fn clear_peers(&self) {
        self.peers.write().await.clear();
        self.peer_states.write().await.clear();
        self.last_audio.write().await.clear();
    }

    pub async fn add_received_file(&self, from: String, name: String, size: usize, path: PathBuf) {
        self.received_files.write().await.push(SharedFile { from, name, size, path });
    }
}
