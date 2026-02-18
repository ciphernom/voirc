use anyhow::Result;
use base64::{engine::general_purpose, Engine};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct RawConnectionInfo {
    #[serde(default)] // <--- ADDED THIS
    host: String,
    #[serde(default)] // <--- ADDED THIS
    port: u16,
    #[serde(default)]
    channels: Vec<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    cert_fingerprint: Option<String>,
    #[serde(default)]
    relay_port: Option<u16>,
}

#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub host: String,
    pub port: u16,
    pub channels: Vec<String>,
    pub cert_fingerprint: Option<String>,
    pub relay_port: Option<u16>,
}

#[derive(Serialize)]
struct WireFormat<'a> {
    host: &'a str,
    port: u16,
    channels: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    cert_fingerprint: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    relay_port: Option<u16>,
}

impl ConnectionInfo {
    pub fn new(host: String, port: u16, channels: Vec<String>) -> Self {
        Self { host, port, channels, cert_fingerprint: None, relay_port: None }
    }

    pub fn with_tls(mut self, fingerprint: String) -> Self {
        self.cert_fingerprint = Some(fingerprint);
        self
    }

    pub fn with_relay(mut self, port: u16) -> Self {
        self.relay_port = Some(port);
        self
    }

    pub fn to_magic_link(&self) -> Result<String> {
        let wire = WireFormat {
            host: &self.host,
            port: self.port,
            channels: &self.channels,
            cert_fingerprint: self.cert_fingerprint.as_deref(),
            relay_port: self.relay_port,
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
            cert_fingerprint: raw.cert_fingerprint,
            relay_port: raw.relay_port,
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
    fn test_connection_info_new() {
        let info = ConnectionInfo::new("localhost".to_string(), 6667, vec!["#general".to_string()]);
        assert_eq!(info.host, "localhost");
        assert_eq!(info.port, 6667);
        assert_eq!(info.channels, vec!["#general"]);
        assert!(info.cert_fingerprint.is_none());
        assert!(info.relay_port.is_none());
    }

    #[test]
    fn test_connection_info_with_tls() {
        let info = ConnectionInfo::new("localhost".to_string(), 6667, vec![])
            .with_tls("abc123".to_string());
        assert_eq!(info.cert_fingerprint, Some("abc123".to_string()));
    }

    #[test]
    fn test_connection_info_with_relay() {
        let info = ConnectionInfo::new("localhost".to_string(), 6667, vec![])
            .with_relay(6668);
        assert_eq!(info.relay_port, Some(6668));
    }

    #[test]
    fn test_to_magic_link() {
        let info = ConnectionInfo::new("192.168.1.1".to_string(), 6667, vec!["#general".to_string()]);
        let link = info.to_magic_link().unwrap();
        assert!(link.starts_with("voirc://"));
        let decoded = &link[8..];
        let decoded_bytes = base64::engine::general_purpose::STANDARD.decode(decoded).unwrap();
        let json = String::from_utf8(decoded_bytes).unwrap();
        assert!(json.contains("192.168.1.1"));
        assert!(json.contains("6667"));
        assert!(json.contains("#general"));
    }

    #[test]
    fn test_from_magic_link() {
        let info = ConnectionInfo::new("example.com".to_string(), 6667, vec!["#general".to_string()]);
        let link = info.to_magic_link().unwrap();
        let parsed = ConnectionInfo::from_magic_link(&link).unwrap();
        assert_eq!(parsed.host, "example.com");
        assert_eq!(parsed.port, 6667);
        assert_eq!(parsed.channels, vec!["#general"]);
    }

    #[test]
    fn test_from_magic_link_with_tls() {
        let info = ConnectionInfo::new("example.com".to_string(), 6667, vec!["#general".to_string()])
            .with_tls("fingerprint123".to_string());
        let link = info.to_magic_link().unwrap();
        let parsed = ConnectionInfo::from_magic_link(&link).unwrap();
        assert_eq!(parsed.cert_fingerprint, Some("fingerprint123".to_string()));
    }

    #[test]
    fn test_from_magic_link_with_relay() {
        let info = ConnectionInfo::new("example.com".to_string(), 6667, vec!["#general".to_string()])
            .with_relay(6668);
        let link = info.to_magic_link().unwrap();
        let parsed = ConnectionInfo::from_magic_link(&link).unwrap();
        assert_eq!(parsed.relay_port, Some(6668));
    }

    #[test]
    fn test_from_magic_link_with_channel_field() {
        // We ensure legacy compatibility with the singular "channel" field.
        // Rust raw strings `r#""#` handle # characters fine, no need to escape or add /.
        let json = r##"{"host":"localhost","port":6667,"channel":"#general"}"##;
        let encoded = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        let link = format!("voirc://{}", encoded);

        let parsed = ConnectionInfo::from_magic_link(&link).unwrap();
        assert_eq!(parsed.host, "localhost");
        assert_eq!(parsed.port, 6667);
        assert_eq!(parsed.channels, vec!["#general"]);
    }

    #[test]
    fn test_from_magic_link_default_channel() {
        let link = "voirc://eyJob3N0IjoibG9jYWxob3N0IiwicG9ydCI6NjY2N30=";
        let parsed = ConnectionInfo::from_magic_link(link).unwrap();
        assert_eq!(parsed.default_channel(), "#general");
    }

    #[test]
    fn test_server_address() {
        let info = ConnectionInfo::new("localhost".to_string(), 6667, vec![]);
        assert_eq!(info.server_address(), "localhost:6667");
    }

    #[test]
    fn test_empty_link() {
        let link = "voirc://e30="; // {}
        let parsed = ConnectionInfo::from_magic_link(link).unwrap();
        assert_eq!(parsed.host, "");
        assert_eq!(parsed.port, 0);
    }
}
