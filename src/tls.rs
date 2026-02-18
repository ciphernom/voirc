use anyhow::Result;
use rcgen::{CertificateParams, KeyPair};
use ring::digest;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::path::Path;
use std::sync::Arc;

#[derive(Clone)]
pub struct CertInfo {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub fingerprint: String,
}

pub fn load_or_generate(dir: &Path) -> Result<CertInfo> {
    let cert_path = dir.join("cert.der");
    let key_path = dir.join("key.der");

    if cert_path.exists() && key_path.exists() {
        let cert_der = std::fs::read(&cert_path)?;
        let key_der = std::fs::read(&key_path)?;
        let fingerprint = sha256_fingerprint(&cert_der);
        return Ok(CertInfo { cert_der, key_der, fingerprint });
    }

    let key_pair = KeyPair::generate()?;
    let params = CertificateParams::new(vec!["voirc.local".to_string()])?;

    let cert = params.self_signed(&key_pair)?;

    let cert_der = cert.der().to_vec();
    let key_der = key_pair.serialize_der();
    let fingerprint = sha256_fingerprint(&cert_der);

    std::fs::write(&cert_path, &cert_der)?;
    std::fs::write(&key_path, &key_der)?;

    Ok(CertInfo { cert_der, key_der, fingerprint })
}

pub fn sha256_fingerprint(der: &[u8]) -> String {
    let d = digest::digest(&digest::SHA256, der);
    hex::encode(d.as_ref())
}

pub fn server_config(info: &CertInfo) -> Result<Arc<rustls::ServerConfig>> {
    let cert = CertificateDer::from(info.cert_der.clone());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(info.key_der.clone()));

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;

    Ok(Arc::new(config))
}

pub fn client_config_pinned(expected_fingerprint: &str) -> Arc<rustls::ClientConfig> {
    let verifier = PinnedCertVerifier {
        expected: expected_fingerprint.to_string(),
    };

    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();

    Arc::new(config)
}

pub fn client_config_insecure() -> Arc<rustls::ClientConfig> {
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAllVerifier))
        .with_no_client_auth();

    Arc::new(config)
}

#[derive(Debug)]
struct PinnedCertVerifier {
    expected: String,
}

impl rustls::client::danger::ServerCertVerifier for PinnedCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let fp = sha256_fingerprint(end_entity.as_ref());
        if fp == self.expected {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "cert fingerprint mismatch: got {} expected {}",
                fp, self.expected
            )))
        }
    }

    fn verify_tls12_signature(
        &self, _message: &[u8], _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self, _message: &[u8], _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Debug)]
struct AcceptAllVerifier;

impl rustls::client::danger::ServerCertVerifier for AcceptAllVerifier {
    fn verify_server_cert(
        &self, _end_entity: &CertificateDer<'_>, _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>, _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self, _message: &[u8], _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self, _message: &[u8], _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_fingerprint() {
        let test_data = b"test certificate data";
        let fp = sha256_fingerprint(test_data);
        // SHA256 produces 64 hex characters
        assert_eq!(fp.len(), 64);
        // Should be all hex characters
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_sha256_fingerprint_deterministic() {
        let test_data = b"same data";
        let fp1 = sha256_fingerprint(test_data);
        let fp2 = sha256_fingerprint(test_data);
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn test_sha256_fingerprint_different_inputs() {
        let data1 = b"data1";
        let data2 = b"data2";
        let fp1 = sha256_fingerprint(data1);
        let fp2 = sha256_fingerprint(data2);
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_cert_info_fields() {
        let info = CertInfo {
            cert_der: vec![1, 2, 3, 4],
            key_der: vec![5, 6, 7, 8],
            fingerprint: "abc123".to_string(),
        };
        assert_eq!(info.cert_der, vec![1, 2, 3, 4]);
        assert_eq!(info.key_der, vec![5, 6, 7, 8]);
        assert_eq!(info.fingerprint, "abc123");
    }

    #[test]
    fn test_cert_info_clone() {
        let info = CertInfo {
            cert_der: vec![1, 2, 3],
            key_der: vec![4, 5, 6],
            fingerprint: "test".to_string(),
        };
        let cloned = info.clone();
        assert_eq!(info.cert_der, cloned.cert_der);
        assert_eq!(info.key_der, cloned.key_der);
        assert_eq!(info.fingerprint, cloned.fingerprint);
    }
}
