use anyhow::Result;
use base64::{engine::general_purpose, Engine};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ConnectionInfo {
    pub host: String,
    pub port: u16,
    pub channel: String,
}

impl ConnectionInfo {
    pub fn new(host: String, port: u16, channel: String) -> Self {
        Self { host, port, channel }
    }

    /// Generate a voirc:// magic link
    pub fn to_magic_link(&self) -> Result<String> {
        let json = serde_json::to_string(self)?;
        let encoded = general_purpose::STANDARD.encode(json.as_bytes());
        Ok(format!("voirc://{}", encoded))
    }

    /// Parse a voirc:// magic link
    pub fn from_magic_link(link: &str) -> Result<Self> {
        let link = link.trim();
        
        // Handle both "voirc://..." and just the base64 part
        let encoded = if link.starts_with("voirc://") {
            &link[8..]
        } else {
            link
        };

        let decoded = general_purpose::STANDARD.decode(encoded)?;
        let json = String::from_utf8(decoded)?;
        let info: ConnectionInfo = serde_json::from_str(&json)?;
        Ok(info)
    }

    pub fn server_address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_magic_link_roundtrip() {
        let info = ConnectionInfo::new("192.168.1.100".to_string(), 6667, "#general".to_string());
        let link = info.to_magic_link().unwrap();
        assert!(link.starts_with("voirc://"));
        
        let decoded = ConnectionInfo::from_magic_link(&link).unwrap();
        assert_eq!(decoded.host, info.host);
        assert_eq!(decoded.port, info.port);
        assert_eq!(decoded.channel, info.channel);
    }
}
