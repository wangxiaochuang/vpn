use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use ipnet::Ipv4Net;
use serde::Deserialize;

use crate::ipam::IpPool;
pub use vpn_core::config::ConfigError;
pub use vpn_core::config::MIN_MTU;
pub use vpn_core::config::{deserialize_ipv4_net, deserialize_ipv4_net_vec};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    pub tun_subnet: Ipv4Net,
    pub mtu: u16,
    pub cert: PathBuf,
    pub key: PathBuf,
    pub routes: Vec<Ipv4Net>,
    pub users_db: String,
    pub telemetry_db: String,
}

impl ServerConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
        let raw: RawConfig = toml::from_str(&content)?;
        Self::from_raw(raw)
    }

    fn from_raw(raw: RawConfig) -> Result<Self, ConfigError> {
        let server = raw.server;
        validate_server_fields(server.mtu, server.tun_subnet, &server.routes)?;
        validate_db_url(&server.users_db)?;
        validate_db_url(&server.telemetry_db)?;
        Ok(Self {
            listen: server.listen,
            tun_subnet: server.tun_subnet,
            mtu: server.mtu,
            cert: server.cert,
            key: server.key,
            routes: server.routes,
            users_db: server.users_db,
            telemetry_db: server.telemetry_db,
        })
    }
}

fn validate_server_fields(
    mtu: u16,
    tun_subnet: Ipv4Net,
    routes: &[Ipv4Net],
) -> Result<(), ConfigError> {
    if mtu < MIN_MTU {
        return Err(ConfigError::MtuTooSmall(mtu));
    }
    IpPool::new(tun_subnet).map_err(|_| ConfigError::InvalidSubnet)?;
    if is_default_route(routes) {
        return Err(ConfigError::DefaultRouteNotAllowed);
    }
    Ok(())
}

fn is_default_route(routes: &[Ipv4Net]) -> bool {
    routes
        .iter()
        .any(|r| r.network() == Ipv4Addr::UNSPECIFIED && r.prefix_len() == 0)
}

fn validate_db_url(url: &str) -> Result<(), ConfigError> {
    if url.is_empty() {
        return Err(ConfigError::InvalidDatabaseUrl);
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    server: RawServer,
}

#[derive(Debug, Deserialize)]
struct RawServer {
    listen: SocketAddr,
    #[serde(deserialize_with = "deserialize_ipv4_net", alias = "tun_subnet")]
    tun_subnet: Ipv4Net,
    mtu: u16,
    cert: PathBuf,
    key: PathBuf,
    #[serde(default, deserialize_with = "deserialize_ipv4_net_vec")]
    routes: Vec<Ipv4Net>,
    #[serde(default)]
    users_db: String,
    #[serde(default)]
    telemetry_db: String,
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::format_push_string,
    clippy::no_effect_replace
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

    fn minimal_config_body() -> String {
        r#"[server]
listen = "127.0.0.1:4433"
tun_subnet = "10.0.0.0/24"
mtu = 1280
cert = "server.crt"
key = "server.key"
users_db = "sqlite://users.db"
telemetry_db = "sqlite://telemetry.db"
"#
        .to_string()
    }

    #[test]
    fn test_load_when_valid_minimal_returns_ok_with_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "ok.toml", &minimal_config_body());
        let cfg = ServerConfig::load(&path).unwrap();
        assert_eq!(cfg.listen, "127.0.0.1:4433".parse().unwrap());
        assert_eq!(cfg.tun_subnet, "10.0.0.0/24".parse::<Ipv4Net>().unwrap());
        assert_eq!(cfg.mtu, 1280);
        assert_eq!(cfg.cert, PathBuf::from("server.crt"));
        assert_eq!(cfg.key, PathBuf::from("server.key"));
        assert_eq!(cfg.users_db, "sqlite://users.db");
        assert_eq!(cfg.telemetry_db, "sqlite://telemetry.db");
        assert!(cfg.routes.is_empty());
    }

    fn config_body_with_routes(routes: &str) -> String {
        minimal_config_body().replacen('\n', &format!("\nroutes = {routes}\n"), 1)
    }

    #[test]
    fn test_load_when_routes_present_returns_ok_with_routes() {
        let dir = tempfile::tempdir().unwrap();
        let body = config_body_with_routes(r#"["192.168.100.0/24", "10.88.0.0/16"]"#);
        let path = write_config(&dir, "routes.toml", &body);
        let cfg = ServerConfig::load(&path).unwrap();
        assert_eq!(cfg.routes.len(), 2);
        assert_eq!(
            cfg.routes[0],
            "192.168.100.0/24".parse::<Ipv4Net>().unwrap()
        );
        assert_eq!(cfg.routes[1], "10.88.0.0/16".parse::<Ipv4Net>().unwrap());
    }

    #[test]
    fn test_load_when_routes_absent_defaults_to_empty_vec() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "no_routes.toml", &minimal_config_body());
        let cfg = ServerConfig::load(&path).unwrap();
        assert_eq!(cfg.routes, Vec::<Ipv4Net>::new());
    }

    #[test]
    fn test_load_when_routes_contains_default_route_returns_default_route_not_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let body = config_body_with_routes(r#"["0.0.0.0/0"]"#);
        let path = write_config(&dir, "default_route.toml", &body);
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::DefaultRouteNotAllowed));
    }

    #[test]
    fn test_load_when_routes_overlap_tun_subnet_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let body = config_body_with_routes(r#"["10.0.0.0/16"]"#);
        let path = write_config(&dir, "overlap.toml", &body);
        assert!(ServerConfig::load(&path).is_ok());
    }

    #[test]
    fn test_load_when_file_missing_returns_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Io(_)));
    }

    #[test]
    fn test_load_when_toml_syntax_error_returns_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "bad.toml", "listen = ");
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn test_load_when_mtu_too_small_returns_mtu_too_small() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_config_body().replace("mtu = 1280", "mtu = 1000");
        let path = write_config(&dir, "mtu_bad.toml", &body);
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::MtuTooSmall(1000)));
    }

    #[test]
    fn test_load_when_subnet_31_returns_invalid_subnet() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_config_body().replace("10.0.0.0/24", "10.0.0.0/31");
        let path = write_config(&dir, "net31.toml", &body);
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidSubnet));
    }

    #[test]
    fn test_load_when_subnet_33_returns_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_config_body().replace("10.0.0.0/24", "10.0.0.0/33");
        let path = write_config(&dir, "net33.toml", &body);
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn test_load_when_users_db_missing_returns_invalid_database_url() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_config_body().replace("users_db = \"sqlite://users.db\"\n", "");
        let path = write_config(&dir, "no_users_db.toml", &body);
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidDatabaseUrl));
    }

    #[test]
    fn test_load_when_telemetry_db_empty_returns_invalid_database_url() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_config_body().replace("sqlite://telemetry.db", "");
        let path = write_config(&dir, "empty_telemetry.toml", &body);
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidDatabaseUrl));
    }

    #[test]
    fn test_load_when_db_unknown_scheme_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_config_body().replace("sqlite://users.db", "mongodb://host/db");
        let path = write_config(&dir, "mongo.toml", &body);
        let cfg = ServerConfig::load(&path).unwrap();
        assert_eq!(cfg.users_db, "mongodb://host/db");
    }

    #[test]
    fn test_load_when_syntax_error_takes_precedence_over_validation() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_config_body().replace("mtu = 1280", "mtu = 1\nlisten = ");
        let path = write_config(&dir, "precedence.toml", &body);
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn test_load_when_users_section_present_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut body = minimal_config_body();
        body.push_str("\n[[users]]\nusername = \"alice\"\npassword_hash = \"x\"\n");
        let path = write_config(&dir, "legacy_users.toml", &body);
        let cfg = ServerConfig::load(&path).unwrap();
        assert_eq!(cfg.users_db, "sqlite://users.db");
    }
}
