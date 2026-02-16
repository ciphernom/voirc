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
