use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, anyhow};
use rustls::RootCertStore;
use rustls::client::ClientConfig as RustlsClientConfig;
use rustls::crypto::aws_lc_rs::default_provider;
use rustls::server::ServerConfig as RustlsServerConfig;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::PrivateKeyDer;
use rustls_pki_types::ServerName;
use rustls_pki_types::pem::PemObject;

pub fn build_quinn_server_config(
    cert_path: &Path,
    key_path: &Path,
) -> anyhow::Result<quinn::ServerConfig> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert_path)
        .map_err(|e| anyhow!("failed to open cert file {}: {e}", cert_path.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow!("failed to parse certificates: {e}"))?;
    if certs.is_empty() {
        anyhow::bail!("no certificates found in {}", cert_path.display());
    }
    let key = PrivateKeyDer::from_pem_file(key_path)
        .with_context(|| format!("failed to load key from {}", key_path.display()))?;

    let rustls_cfg = RustlsServerConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| anyhow!("failed to select TLS protocol versions: {e}"))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| anyhow!("failed to build rustls ServerConfig: {e}"))?;

    let quic_cfg = quinn::crypto::rustls::QuicServerConfig::try_from(Arc::new(rustls_cfg))
        .map_err(|e| anyhow!("failed to build QuicServerConfig: {e}"))?;

    Ok(quinn::ServerConfig::with_crypto(Arc::new(quic_cfg)))
}

pub fn build_quinn_client_config(
    ca_cert: &Path,
    server_name: &str,
) -> anyhow::Result<quinn::ClientConfig> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(ca_cert)
        .map_err(|e| anyhow!("failed to open CA file {}: {e}", ca_cert.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow!("failed to parse CA certificates: {e}"))?;
    if certs.is_empty() {
        anyhow::bail!("no CA certificates found in {}", ca_cert.display());
    }

    let mut root_store = RootCertStore::empty();
    root_store.add_parsable_certificates(certs);

    ServerName::try_from(server_name)
        .map_err(|e| anyhow!("invalid server_name {server_name:?}: {e}"))?;

    let rustls_cfg = RustlsClientConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| anyhow!("failed to select TLS protocol versions: {e}"))?
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let quic_cfg = quinn::crypto::rustls::QuicClientConfig::try_from(Arc::new(rustls_cfg))
        .map_err(|e| anyhow!("failed to build QuicClientConfig: {e}"))?;

    Ok(quinn::ClientConfig::new(Arc::new(quic_cfg)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("quic-link crate nested under repo root")
            .to_path_buf()
    }

    #[test]
    fn test_build_quinn_server_config_loads_self_signed_repo_certs() {
        let cert = repo_root().join("cert.pem");
        let key = repo_root().join("key.pem");
        let cfg = build_quinn_server_config(&cert, &key);
        assert!(
            cfg.is_ok(),
            "should load repo self-signed certs: {:?}",
            cfg.err()
        );
    }

    #[test]
    fn test_build_quinn_server_config_missing_cert_returns_err() {
        let cert = repo_root().join("nonexistent-cert.pem");
        let key = repo_root().join("key.pem");
        assert!(build_quinn_server_config(&cert, &key).is_err());
    }

    #[test]
    fn test_build_quinn_server_config_missing_key_returns_err() {
        let cert = repo_root().join("cert.pem");
        let key = repo_root().join("nonexistent-key.pem");
        assert!(build_quinn_server_config(&cert, &key).is_err());
    }

    #[test]
    fn test_build_quinn_server_config_empty_cert_list_returns_err() {
        let empty = tempfile::NamedTempFile::new().expect("temp file");
        let cert = empty.path();
        let key = repo_root().join("key.pem");
        assert!(build_quinn_server_config(cert, &key).is_err());
    }

    #[test]
    fn test_build_quinn_client_config_ca_exists_returns_ok() {
        let ca = repo_root().join("cert.pem");
        let cfg = build_quinn_client_config(&ca, "localhost");
        assert!(
            cfg.is_ok(),
            "should load repo self-signed cert as CA: {:?}",
            cfg.err()
        );
    }

    #[test]
    fn test_build_quinn_client_config_missing_ca_returns_err() {
        let ca = repo_root().join("nonexistent-ca.pem");
        assert!(build_quinn_client_config(&ca, "localhost").is_err());
    }

    #[test]
    fn test_build_quinn_client_config_invalid_server_name_returns_err() {
        let ca = repo_root().join("cert.pem");
        assert!(build_quinn_client_config(&ca, "").is_err());
    }
}
