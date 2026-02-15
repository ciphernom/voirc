use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use uuid::Uuid;

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

/// TURN server credentials for NAT traversal behind symmetric NATs.
/// Without this, users on corporate/university networks simply cannot connect.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TurnServer {
    pub url: String,
    pub username: String,
    pub credential: String,
}

/// Peer role in a room. Host is the original server creator.
/// Mods are promoted by the host and act as audio relay superpeers.
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
