use anyhow::Result;
use base64::{engine::general_purpose, Engine};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct RawConnectionInfo {
    #[serde(default)]
    host: String,
    #[serde(default)]
    port: u16,
    #[serde(default)]
    channels: Vec<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    cert_fingerprint: Option<String>,
    #[serde(default)]
    relay_port: Option<u16>,
    /// Minimum PoW difficulty (leading zero bits) required to register a nick
    /// on this server.  0 means disabled.
    #[serde(default)]
    pow_required_bits: u8,
}

#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub host: String,
    pub port: u16,
    pub channels: Vec<String>,
    pub cert_fingerprint: Option<String>,
    pub relay_port: Option<u16>,
    /// Minimum PoW difficulty required by the host.  Clients check their nick
    /// against this before even attempting to connect, and can mine a stronger
    /// nick offline if needed.  0 = no PoW required.
    pub pow_required_bits: u8,
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
    #[serde(skip_serializing_if = "is_zero")]
    pow_required_bits: u8,
}

fn is_zero(v: &u8) -> bool { *v == 0 }

impl ConnectionInfo {
    pub fn new(host: String, port: u16, channels: Vec<String>) -> Self {
        Self {
            host,
            port,
            channels,
            cert_fingerprint: None,
            relay_port: None,
            pow_required_bits: 0,
        }
    }

    pub fn with_tls(mut self, fingerprint: String) -> Self {
        self.cert_fingerprint = Some(fingerprint);
        self
    }

    pub fn with_relay(mut self, port: u16) -> Self {
        self.relay_port = Some(port);
        self
    }

    /// Set the minimum PoW difficulty encoded in this link.
    /// Joining clients whose nick was mined below this difficulty will need
    /// to re-mine before connecting.
    pub fn with_pow(mut self, bits: u8) -> Self {
        self.pow_required_bits = bits;
        self
    }

    pub fn to_magic_link(&self) -> Result<String> {
        let wire = WireFormat {
            host: &self.host,
            port: self.port,
            channels: &self.channels,
            cert_fingerprint: self.cert_fingerprint.as_deref(),
            relay_port: self.relay_port,
            pow_required_bits: self.pow_required_bits,
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
            pow_required_bits: raw.pow_required_bits,
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
    fn test_connection_info_new_defaults() {
        let info = ConnectionInfo::new("localhost".to_string(), 6667, vec!["#general".to_string()]);
        assert_eq!(info.pow_required_bits, 0);
    }

    #[test]
    fn test_with_pow_roundtrip() {
        let info = ConnectionInfo::new("example.com".to_string(), 6667, vec!["#general".to_string()])
            .with_pow(16);
        assert_eq!(info.pow_required_bits, 16);

        let link = info.to_magic_link().unwrap();
        let parsed = ConnectionInfo::from_magic_link(&link).unwrap();
        assert_eq!(parsed.pow_required_bits, 16);
    }

    #[test]
    fn test_zero_pow_omitted_from_wire() {
        // pow_required_bits = 0 should not appear in the JSON (skip_serializing_if)
        let info = ConnectionInfo::new("example.com".to_string(), 6667, vec![]);
        let link = info.to_magic_link().unwrap();
        let encoded = &link[8..];
        let decoded = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
        let json = String::from_utf8(decoded).unwrap();
        assert!(!json.contains("pow_required_bits"), "zero bits should be omitted: {}", json);
    }

    #[test]
    fn test_nonzero_pow_present_in_wire() {
        let info = ConnectionInfo::new("example.com".to_string(), 6667, vec![])
            .with_pow(20);
        let link = info.to_magic_link().unwrap();
        let encoded = &link[8..];
        let decoded = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
        let json = String::from_utf8(decoded).unwrap();
        assert!(json.contains("pow_required_bits"), "bits should appear: {}", json);
        assert!(json.contains("20"));
    }

    #[test]
    fn test_legacy_link_without_pow_parses_as_zero() {
        // Old links without pow_required_bits should default to 0
        let link = "voirc://eyJob3N0IjoiZXhhbXBsZS5jb20iLCJwb3J0Ijo2NjY3LCJjaGFubmVscyI6WyIjZ2VuZXJhbCJdfQ==";
        let parsed = ConnectionInfo::from_magic_link(link).unwrap();
        assert_eq!(parsed.pow_required_bits, 0);
    }

    #[test]
    fn test_full_roundtrip_with_all_fields() {
        let info = ConnectionInfo::new("192.168.1.1".to_string(), 6667, vec!["#general".to_string()])
            .with_tls("abc123fingerprint".to_string())
            .with_relay(6668)
            .with_pow(12);

        let link = info.to_magic_link().unwrap();
        let parsed = ConnectionInfo::from_magic_link(&link).unwrap();

        assert_eq!(parsed.host, "192.168.1.1");
        assert_eq!(parsed.port, 6667);
        assert_eq!(parsed.cert_fingerprint, Some("abc123fingerprint".to_string()));
        assert_eq!(parsed.relay_port, Some(6668));
        assert_eq!(parsed.pow_required_bits, 12);
    }
}
