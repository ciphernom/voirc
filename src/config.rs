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
    #[serde(default)]
    pub pubkey_hex: Option<String>,

    /// Default PoW difficulty (leading zero bits) to require when hosting.
    /// 0 = disabled. Stored so the host doesn't have to re-enter it each time.
    /// Can also be changed at runtime by mods via /powset.
    #[serde(default)]
    pub pow_required_bits: u8,
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
            pubkey_hex: None,
            pow_required_bits: 0,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pow_required_bits_default_zero() {
        let config = UserConfig::default();
        assert_eq!(config.pow_required_bits, 0);
    }

    #[test]
    fn test_pow_required_bits_serialization() {
        let mut config = UserConfig::default();
        config.pow_required_bits = 16;
        let toml = toml::to_string_pretty(&config).unwrap();
        assert!(toml.contains("pow_required_bits = 16"));
        let reloaded: UserConfig = toml::from_str(&toml).unwrap();
        assert_eq!(reloaded.pow_required_bits, 16);
    }

    #[test]
    fn test_pow_required_bits_zero_omitted_old_configs() {
        // Old configs without the field should default to 0
        let toml = r#"
            user_id = "test-uuid"
            display_name = "Test"
            recent_servers = []
        "#;
        let config: UserConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.pow_required_bits, 0);
    }

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
    fn test_conn_state_display() {
        assert_eq!(ConnState::Connecting.to_string(), "connecting...");
        assert_eq!(ConnState::Connected.to_string(), "connected");
        assert_eq!(ConnState::NatIssue.to_string(), "NAT issue - try relay");
        assert_eq!(ConnState::Relayed.to_string(), "relayed");
        assert_eq!(ConnState::Failed.to_string(), "failed");
    }
}
