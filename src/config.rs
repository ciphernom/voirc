use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UserConfig {
    pub user_id: String,
    pub display_name: String,
    pub recent_servers: Vec<RecentServer>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RecentServer {
    pub name: String,
    pub connection_string: String,
    pub last_connected: String,
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            user_id: Uuid::new_v4().to_string(),
            display_name: "User".to_string(),
            recent_servers: Vec::new(),
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
        
        // Remove duplicates
        self.recent_servers.retain(|s| s.connection_string != connection_string);
        
        // Add to front
        self.recent_servers.insert(0, RecentServer {
            name,
            connection_string,
            last_connected: now,
        });
        
        // Keep only last 10
        self.recent_servers.truncate(10);
        
        let _ = self.save();
    }
}
