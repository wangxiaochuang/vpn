use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub use vpn_core::config::ConfigError;
pub use vpn_core::config::MIN_MTU;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConfig {
    pub server: SocketAddr,
    pub server_name: String,
    pub ca_cert: PathBuf,
}

impl ClientConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
        let raw: RawClientConfig = toml::from_str(&content)?;
        Self::from_raw(raw)
    }

    fn from_raw(raw: RawClientConfig) -> Result<Self, ConfigError> {
        if raw.client.server_name.is_empty() {
            return Err(ConfigError::EmptyServerName);
        }
        if raw.client.ca_cert.as_os_str().is_empty() {
            return Err(ConfigError::EmptyCaCert);
        }
        Ok(Self {
            server: raw.client.server,
            server_name: raw.client.server_name,
            ca_cert: raw.client.ca_cert,
        })
    }
}

#[derive(Debug, Deserialize)]
struct RawClientConfig {
    client: RawClient,
}

#[derive(Debug, Deserialize)]
struct RawClient {
    server: SocketAddr,
    server_name: String,
    ca_cert: PathBuf,
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::format_push_string
)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_config(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    fn minimal_client_config_body() -> String {
        r#"[client]
server = "127.0.0.1:4433"
server_name = "vpn.example.com"
ca_cert = "ca.crt"
"#
        .to_string()
    }

    #[test]
    fn test_client_load_when_valid_minimal_returns_ok_with_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "client_ok.toml", &minimal_client_config_body());
        let cfg = ClientConfig::load(&path).unwrap();
        assert_eq!(cfg.server, "127.0.0.1:4433".parse().unwrap());
        assert_eq!(cfg.server_name, "vpn.example.com");
        assert_eq!(cfg.ca_cert, PathBuf::from("ca.crt"));
    }

    #[test]
    fn test_client_load_when_legacy_username_present_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let body = format!("{}username = \"alice\"\n", minimal_client_config_body());
        let path = write_config(&dir, "client_legacy.toml", &body);
        let cfg = ClientConfig::load(&path).expect("legacy username row must be ignored");
        assert_eq!(cfg.server, "127.0.0.1:4433".parse().unwrap());
        assert_eq!(cfg.server_name, "vpn.example.com");
        assert_eq!(cfg.ca_cert, PathBuf::from("ca.crt"));
    }

    #[test]
    fn test_client_load_when_file_missing_returns_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        let err = ClientConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Io(_)));
    }

    #[test]
    fn test_client_load_when_toml_syntax_error_returns_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "client_bad.toml", "server = ");
        let err = ClientConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn test_client_load_when_empty_server_name_returns_empty_server_name() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_client_config_body()
            .replace(r#"server_name = "vpn.example.com""#, r#"server_name = """#);
        let path = write_config(&dir, "client_sn.toml", &body);
        let err = ClientConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::EmptyServerName));
    }

    #[test]
    fn test_client_load_when_empty_ca_cert_returns_empty_ca_cert() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_client_config_body().replace(r#"ca_cert = "ca.crt""#, r#"ca_cert = """#);
        let path = write_config(&dir, "client_ca.toml", &body);
        let err = ClientConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::EmptyCaCert));
    }

    #[test]
    fn test_client_load_when_syntax_error_takes_precedence_over_validation() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_client_config_body().replace(
            "server = \"127.0.0.1:4433\"",
            "server = \"127.0.0.1:4433\"\nbroken = ",
        );
        let path = write_config(&dir, "client_precedence.toml", &body);
        let err = ClientConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
    }
}
