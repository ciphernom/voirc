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
