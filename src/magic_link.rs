use anyhow::Result;
use base64::{engine::general_purpose, Engine};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct RawConnectionInfo {
    host: String,
    port: u16,
    #[serde(default)]
    channels: Vec<String>,
    #[serde(default)]
    channel: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub host: String,
    pub port: u16,
    pub channels: Vec<String>,
}

#[derive(Serialize)]
struct WireFormat<'a> {
    host: &'a str,
    port: u16,
    channels: &'a [String],
}

impl ConnectionInfo {
    pub fn new(host: String, port: u16, channels: Vec<String>) -> Self {
        Self { host, port, channels }
    }

    pub fn to_magic_link(&self) -> Result<String> {
        let wire = WireFormat {
            host: &self.host,
            port: self.port,
            channels: &self.channels,
        };
        let json = serde_json::to_string(&wire)?;
        let encoded = general_purpose::STANDARD.encode(json.as_bytes());
        Ok(format!("voirc://{}", encoded))
    }

    pub fn from_magic_link(link: &str) -> Result<Self> {
        let link = link.trim();
        let encoded = if link.starts_with("voirc://") { &link[8..] } else { link };

        let decoded = general_purpose::STANDARD.decode(encoded)?;
        let json = String::from_utf8(decoded)?;
        let raw: RawConnectionInfo = serde_json::from_str(&json)?;

        // Backward compat: old links have "channel", new have "channels"
        let channels = if !raw.channels.is_empty() {
            raw.channels
        } else if let Some(ch) = raw.channel {
            vec![ch]
        } else {
            vec!["#general".to_string()]
        };

        Ok(Self {
            host: raw.host,
            port: raw.port,
            channels,
        })
    }

    pub fn server_address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn default_channel(&self) -> &str {
        self.channels.first().map(|s| s.as_str()).unwrap_or("#general")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_magic_link_roundtrip() {
        let info = ConnectionInfo::new(
            "192.168.1.100".to_string(),
            6667,
            vec!["#general".to_string(), "#gaming".to_string()],
        );
        let link = info.to_magic_link().unwrap();
        let decoded = ConnectionInfo::from_magic_link(&link).unwrap();
        assert_eq!(decoded.host, info.host);
        assert_eq!(decoded.port, info.port);
        assert_eq!(decoded.channels, info.channels);
    }

    #[test]
    fn test_backward_compat() {
        // Old format with single "channel" field
        let json = r##"{"host":"127.0.0.1","port":6667,"channel":"#test"}"##;
        let encoded = general_purpose::STANDARD.encode(json.as_bytes());
        let link = format!("voirc://{}", encoded);
        let decoded = ConnectionInfo::from_magic_link(&link).unwrap();
        assert_eq!(decoded.channels, vec!["#test".to_string()]);
    }
}
