use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use webrtc::peer_connection::RTCPeerConnection;

use crate::config::Role;

#[derive(Clone, Debug)]
pub struct PeerState {
    pub nickname: String,
    pub connected: bool,
    pub speaking: bool,
    pub role: Role,
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
    /// Our own role in the current room
    pub our_role: RwLock<Role>,
    /// Log directory for persistent chat
    log_dir: PathBuf,
}

impl AppState {
    pub fn new() -> Arc<Self> {
        let log_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("voirc")
            .join("logs");
        let _ = std::fs::create_dir_all(&log_dir);

        Arc::new(Self {
            peers: RwLock::new(HashMap::new()),
            peer_states: RwLock::new(HashMap::new()),
            messages: RwLock::new(HashMap::new()),
            last_audio: RwLock::new(HashMap::new()),
            received_files: RwLock::new(Vec::new()),
            our_role: RwLock::new(Role::Peer),
            log_dir,
        })
    }

    pub async fn set_our_role(&self, role: Role) {
        *self.our_role.write().await = role;
    }

    pub async fn our_role(&self) -> Role {
        *self.our_role.read().await
    }

    pub async fn add_message(&self, channel: &str, msg: String) {
        // Persist to disk (append-only, one file per channel)
        self.persist_message(channel, &msg);

        let mut messages = self.messages.write().await;
        let list = messages.entry(channel.to_string()).or_default();
        list.push(msg);
        if list.len() > 1000 {
            list.drain(0..100);
        }
    }

    fn persist_message(&self, channel: &str, msg: &str) {
        let safe_name = channel.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
        let path = self.log_dir.join(format!("{}.log", safe_name));
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            let _ = writeln!(f, "[{}] {}", ts, msg);
        }
    }

    /// Load recent chat history from disk for a channel
    pub async fn load_history(&self, channel: &str, max_lines: usize) {
        let safe_name = channel.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
        let path = self.log_dir.join(format!("{}.log", safe_name));

        if let Ok(content) = std::fs::read_to_string(&path) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(max_lines);
            let mut messages = self.messages.write().await;
            let list = messages.entry(channel.to_string()).or_default();

            if list.is_empty() {
                for line in &lines[start..] {
                    // Strip the [timestamp] prefix for display, keep the message
                    if let Some(end_bracket) = line.find("] ") {
                        list.push(line[end_bracket + 2..].to_string());
                    } else {
                        list.push(line.to_string());
                    }
                }
            }
        }
    }

    pub async fn update_peer_state(&self, nick: String, connected: bool, speaking: bool) {
        let mut states = self.peer_states.write().await;
        let existing_role = states.get(&nick).map(|s| s.role).unwrap_or(Role::Peer);
        states.insert(
            nick.clone(),
            PeerState {
                nickname: nick,
                connected,
                speaking,
                role: existing_role,
            },
        );
    }

    pub async fn set_peer_role(&self, nick: &str, role: Role) {
        let mut states = self.peer_states.write().await;
        if let Some(ps) = states.get_mut(nick) {
            ps.role = role;
        } else {
            states.insert(nick.to_string(), PeerState {
                nickname: nick.to_string(),
                connected: false,
                speaking: false,
                role,
            });
        }
    }

    pub async fn get_peer_role(&self, nick: &str) -> Role {
        self.peer_states.read().await
            .get(nick)
            .map(|ps| ps.role)
            .unwrap_or(Role::Peer)
    }

    /// Get all known superpeers (connected or not)
    pub async fn all_superpeers(&self) -> Vec<String> {
        self.peer_states.read().await
            .values()
            .filter(|ps| ps.role.is_superpeer())
            .map(|ps| ps.nickname.clone())
            .collect()
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

    pub async fn clear_peers(&self) {
        self.peers.write().await.clear();
        self.peer_states.write().await.clear();
        self.last_audio.write().await.clear();
    }

    pub async fn add_received_file(&self, from: String, name: String, size: usize, path: PathBuf) {
        self.received_files.write().await.push(SharedFile { from, name, size, path });
    }
}
