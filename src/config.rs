use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConnState {
    Connecting,
    Connected,
    NatIssue,
    Relayed,
    Failed,
}

impl fmt::Display for ConnState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnState::Connecting => write!(f, "connecting..."),
            ConnState::Connected => write!(f, "connected"),
            ConnState::NatIssue => write!(f, "NAT issue - try relay"),
            ConnState::Relayed => write!(f, "relayed"),
            ConnState::Failed => write!(f, "failed"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct NetDiagnostics {
    pub local_ip: Option<String>,
    pub external_ip: Option<String>,
    pub upnp_status: Option<String>,
    pub turn_configured: usize,
    pub relay_enabled: bool,
    pub relay_port: Option<u16>,
    pub port_open: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UserConfig {
    pub user_id: String,
    pub display_name: String,
    pub recent_servers: Vec<RecentServer>,
    #[serde(default)]
    pub turn_servers: Vec<TurnServer>,
    #[serde(default)]
    pub banned_users: HashSet<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RecentServer {
    pub name: String,
    pub connection_string: String,
    pub last_connected: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TurnServer {
    pub url: String,
    pub username: String,
    pub credential: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Role {
    Host,
    Mod,
    Peer,
}

impl Role {
    pub fn is_superpeer(self) -> bool {
        matches!(self, Role::Host | Role::Mod)
    }

    pub fn can_moderate(self) -> bool {
        matches!(self, Role::Host | Role::Mod)
    }

    pub fn can_promote(self) -> bool {
        matches!(self, Role::Host)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Host => "host",
            Role::Mod => "mod",
            Role::Peer => "peer",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "host" => Role::Host,
            "mod" => Role::Mod,
            _ => Role::Peer,
        }
    }
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            user_id: Uuid::new_v4().to_string(),
            display_name: "User".to_string(),
            recent_servers: Vec::new(),
            turn_servers: Vec::new(),
            banned_users: HashSet::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_is_superpeer() {
        assert!(Role::Host.is_superpeer());
        assert!(Role::Mod.is_superpeer());
        assert!(!Role::Peer.is_superpeer());
    }

    #[test]
    fn test_role_can_moderate() {
        assert!(Role::Host.can_moderate());
        assert!(Role::Mod.can_moderate());
        assert!(!Role::Peer.can_moderate());
    }

    #[test]
    fn test_role_can_promote() {
        assert!(Role::Host.can_promote());
        assert!(!Role::Mod.can_promote());
        assert!(!Role::Peer.can_promote());
    }

    #[test]
    fn test_role_as_str() {
        assert_eq!(Role::Host.as_str(), "host");
        assert_eq!(Role::Mod.as_str(), "mod");
        assert_eq!(Role::Peer.as_str(), "peer");
    }

    #[test]
    fn test_role_from_str() {
        assert_eq!(Role::from_str("host"), Role::Host);
        assert_eq!(Role::from_str("mod"), Role::Mod);
        assert_eq!(Role::from_str("peer"), Role::Peer);
        assert_eq!(Role::from_str("unknown"), Role::Peer);
        assert_eq!(Role::from_str(""), Role::Peer);
    }

    #[test]
    fn test_conn_state_display() {
        assert_eq!(ConnState::Connecting.to_string(), "connecting...");
        assert_eq!(ConnState::Connected.to_string(), "connected");
        assert_eq!(ConnState::NatIssue.to_string(), "NAT issue - try relay");
        assert_eq!(ConnState::Relayed.to_string(), "relayed");
        assert_eq!(ConnState::Failed.to_string(), "failed");
    }

    #[test]
    fn test_user_config_default() {
        let config = UserConfig::default();
        assert!(!config.user_id.is_empty());
        assert_eq!(config.display_name, "User");
        assert!(config.recent_servers.is_empty());
        assert!(config.turn_servers.is_empty());
        assert!(config.banned_users.is_empty());
    }

    #[test]
    fn test_turn_server_serialization() {
        let turn = TurnServer {
            url: "turn:example.com:3478".to_string(),
            username: "user".to_string(),
            credential: "pass".to_string(),
        };
        let json = serde_json::to_string(&turn).unwrap();
        assert!(json.contains("turn:example.com:3478"));
        assert!(json.contains("user"));
        assert!(json.contains("pass"));
    }

    #[test]
    fn test_recent_server_serialization() {
        let server = RecentServer {
            name: "Test Server".to_string(),
            connection_string: "localhost:6667".to_string(),
            last_connected: "2024-01-01 12:00".to_string(),
        };
        let json = serde_json::to_string(&server).unwrap();
        assert!(json.contains("Test Server"));
        assert!(json.contains("localhost:6667"));
    }
}

impl UserConfig {
    fn config_path() -> PathBuf {
        let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        path.push("voirc");
        std::fs::create_dir_all(&path).ok();
        path.push("config.toml");
        path
    }

    pub fn tls_cert_dir() -> PathBuf {
        let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        path.push("voirc");
        path.push("tls");
        std::fs::create_dir_all(&path).ok();
        path
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if path.exists() {
            let content = std::fs::read_to_string(path)?;
            Ok(toml::from_str(&content)?)
        } else {
            let config = Self::default();
            config.save()?;
            Ok(config)
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn add_recent_server(&mut self, name: String, connection_string: String) {
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        self.recent_servers.retain(|s| s.connection_string != connection_string);
        self.recent_servers.insert(0, RecentServer {
            name,
            connection_string,
            last_connected: now,
        });
        self.recent_servers.truncate(10);
        let _ = self.save();
    }
}
