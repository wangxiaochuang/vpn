use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use ipnet::Ipv4Net;
use serde::Deserialize;
use thiserror::Error;

use crate::auth::AuthError;
use crate::auth::UserStore;
use crate::ipam::IpPool;
pub const MIN_MTU: u16 = 1280;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file: {0}")]
    Io(#[source] std::io::Error),
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("mtu {0} is smaller than minimum {MIN_MTU}")]
    MtuTooSmall(u16),
    #[error("tun subnet is invalid or has no allocatable addresses")]
    InvalidSubnet,
    #[error("user list contains an empty username")]
    EmptyUsername,
    #[error("duplicate user: {0}")]
    DuplicateUser(String),
    #[error("password hash is not a valid argon2 PHC string")]
    InvalidHash,
    #[error("server_name must not be empty")]
    EmptyServerName,
    #[error("ca_cert must not be empty")]
    EmptyCaCert,
    #[error("routes must not contain the default route 0.0.0.0/0")]
    DefaultRouteNotAllowed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserConfig {
    pub username: String,
    pub password_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    pub tun_subnet: Ipv4Net,
    pub mtu: u16,
    pub cert: PathBuf,
    pub key: PathBuf,
    pub routes: Vec<Ipv4Net>,
    pub users: Vec<UserConfig>,
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
        let users = build_user_configs(raw.users);
        let user_pairs: Vec<(String, String)> = users
            .iter()
            .map(|u| (u.username.clone(), u.password_hash.clone()))
            .collect();
        map_user_error(UserStore::from_users(user_pairs))?;
        Ok(Self {
            listen: server.listen,
            tun_subnet: server.tun_subnet,
            mtu: server.mtu,
            cert: server.cert,
            key: server.key,
            routes: server.routes,
            users,
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

fn build_user_configs(raw: Vec<RawUser>) -> Vec<UserConfig> {
    raw.into_iter()
        .map(|u| UserConfig {
            username: u.username,
            password_hash: u.password_hash,
        })
        .collect()
}

fn map_user_error(res: Result<UserStore, AuthError>) -> Result<(), ConfigError> {
    match res {
        Ok(_) | Err(AuthError::InvalidCredentials) => Ok(()),
        Err(AuthError::EmptyUsername) => Err(ConfigError::EmptyUsername),
        Err(AuthError::DuplicateUser(name)) => Err(ConfigError::DuplicateUser(name)),
        Err(AuthError::InvalidHash) => Err(ConfigError::InvalidHash),
    }
}

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
struct RawConfig {
    server: RawServer,
    #[serde(default)]
    users: Vec<RawUser>,
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
}

fn deserialize_ipv4_net<'de, D>(deserializer: D) -> Result<Ipv4Net, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let s = String::deserialize(deserializer)?;
    s.parse().map_err(serde::de::Error::custom)
}

fn deserialize_ipv4_net_vec<'de, D>(deserializer: D) -> Result<Vec<Ipv4Net>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let seq = Vec::<String>::deserialize(deserializer)?;
    seq.into_iter()
        .map(|s| s.parse().map_err(serde::de::Error::custom))
        .collect()
}

#[derive(Debug, Deserialize)]
struct RawUser {
    username: String,
    password_hash: String,
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
    clippy::format_push_string,
    clippy::no_effect_replace
)]
mod tests {
    use super::*;
    use argon2::Argon2;
    use argon2::PasswordHasher;
    use argon2::password_hash::SaltString;
    use argon2::password_hash::rand_core::OsRng;
    use std::io::Write;

    fn hash_password(pw: &str) -> String {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(pw.as_bytes(), &salt)
            .unwrap()
            .to_string()
    }

    const VALID_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$j3xYVqWV0EE+AG6htXRGTA$g446kNT7dmrxnDjw/DZYHbCWrO83sNJtAdIqmWjcknE";

    fn write_config(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    fn minimal_config_body(hash: &str) -> String {
        format!(
            r#"[server]
listen = "127.0.0.1:4433"
tun_subnet = "10.0.0.0/24"
mtu = 1280
cert = "server.crt"
key = "server.key"

[[users]]
username = "alice"
password_hash = "{hash}"
"#
        )
    }

    #[allow(clippy::indexing_slicing)]
    fn assert_displays_unique(all: &[String]) {
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "duplicate at {i},{j}");
            }
        }
    }

    fn base_config_error_displays() -> Vec<String> {
        vec![
            ConfigError::Io(io_stub()).to_string(),
            ConfigError::Parse(toml::from_str::<serde::de::IgnoredAny>("x =").unwrap_err())
                .to_string(),
            ConfigError::MtuTooSmall(1000).to_string(),
            ConfigError::InvalidSubnet.to_string(),
            ConfigError::EmptyUsername.to_string(),
            ConfigError::DuplicateUser("alice".into()).to_string(),
            ConfigError::InvalidHash.to_string(),
        ]
    }

    fn all_config_error_displays() -> Vec<String> {
        let mut all = base_config_error_displays();
        all.push(ConfigError::EmptyServerName.to_string());
        all.push(ConfigError::EmptyCaCert.to_string());
        all.push(ConfigError::DefaultRouteNotAllowed.to_string());
        all
    }

    #[test]
    #[allow(clippy::indexing_slicing)]
    fn test_config_error_display_variants_are_distinct() {
        let all = base_config_error_displays();
        assert_displays_unique(&all);
        assert!(all[0].contains("read config file"));
        assert!(all[1].contains("parse"));
        assert!(all[2].contains("1000") && all[2].contains("1280"));
        assert!(all[3].contains("subnet"));
        assert!(all[4].contains("empty"));
        assert!(all[5].contains("alice"));
        assert!(all[6].contains("hash"));
    }

    fn io_stub() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::NotFound, "stub")
    }

    #[test]
    fn test_load_when_valid_minimal_returns_ok_with_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "ok.toml", &minimal_config_body(VALID_HASH));
        let cfg = ServerConfig::load(&path).unwrap();
        assert_eq!(cfg.listen, "127.0.0.1:4433".parse().unwrap());
        assert_eq!(cfg.tun_subnet, "10.0.0.0/24".parse::<Ipv4Net>().unwrap());
        assert_eq!(cfg.mtu, 1280);
        assert_eq!(cfg.cert, PathBuf::from("server.crt"));
        assert_eq!(cfg.key, PathBuf::from("server.key"));
        assert_eq!(cfg.users.len(), 1);
        assert_eq!(cfg.users[0].username, "alice");
        assert!(cfg.routes.is_empty());
    }

    fn config_body_with_routes(hash: &str, routes: &str) -> String {
        let body = minimal_config_body(hash);
        body.replacen(
            "\n\n[[users]]",
            &format!("\nroutes = {routes}\n\n[[users]]"),
            1,
        )
    }

    #[test]
    fn test_load_when_routes_present_returns_ok_with_routes() {
        let dir = tempfile::tempdir().unwrap();
        let body = config_body_with_routes(VALID_HASH, r#"["192.168.100.0/24", "10.88.0.0/16"]"#);
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
        let path = write_config(&dir, "no_routes.toml", &minimal_config_body(VALID_HASH));
        let cfg = ServerConfig::load(&path).unwrap();
        assert_eq!(cfg.routes, Vec::<Ipv4Net>::new());
    }

    #[test]
    fn test_load_when_routes_contains_default_route_returns_default_route_not_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let body = config_body_with_routes(VALID_HASH, r#"["0.0.0.0/0"]"#);
        let path = write_config(&dir, "default_route.toml", &body);
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::DefaultRouteNotAllowed));
    }

    #[test]
    fn test_load_when_routes_overlap_tun_subnet_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let body = config_body_with_routes(VALID_HASH, r#"["10.0.0.0/16"]"#);
        let path = write_config(&dir, "overlap.toml", &body);
        assert!(ServerConfig::load(&path).is_ok());
    }

    #[test]
    fn test_default_route_not_allowed_display_is_distinct() {
        let default_route = ConfigError::DefaultRouteNotAllowed.to_string();
        let all_others = [
            ConfigError::Io(io_stub()).to_string(),
            ConfigError::Parse(toml::from_str::<serde::de::IgnoredAny>("x =").unwrap_err())
                .to_string(),
            ConfigError::MtuTooSmall(1000).to_string(),
            ConfigError::InvalidSubnet.to_string(),
            ConfigError::EmptyUsername.to_string(),
            ConfigError::DuplicateUser("alice".into()).to_string(),
            ConfigError::InvalidHash.to_string(),
            ConfigError::EmptyServerName.to_string(),
            ConfigError::EmptyCaCert.to_string(),
        ];
        for other in &all_others {
            assert_ne!(&default_route, other);
        }
        assert!(default_route.contains("default route") || default_route.contains("0.0.0.0/0"));
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
    fn test_load_when_mtu_1280_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_config_body(VALID_HASH).replace("mtu = 1280", "mtu = 1280");
        let path = write_config(&dir, "mtu_ok.toml", &body);
        assert!(ServerConfig::load(&path).is_ok());
    }

    #[test]
    fn test_load_when_mtu_too_small_returns_mtu_too_small() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_config_body(VALID_HASH).replace("mtu = 1280", "mtu = 1000");
        let path = write_config(&dir, "mtu_bad.toml", &body);
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::MtuTooSmall(1000)));
    }

    #[test]
    fn test_load_when_subnet_24_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "net24.toml", &minimal_config_body(VALID_HASH));
        assert!(ServerConfig::load(&path).is_ok());
    }

    #[test]
    fn test_load_when_subnet_31_returns_invalid_subnet() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_config_body(VALID_HASH).replace("10.0.0.0/24", "10.0.0.0/31");
        let path = write_config(&dir, "net31.toml", &body);
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidSubnet));
    }

    #[test]
    fn test_load_when_subnet_33_returns_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_config_body(VALID_HASH).replace("10.0.0.0/24", "10.0.0.0/33");
        let path = write_config(&dir, "net33.toml", &body);
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn test_load_when_valid_single_user_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "user_ok.toml", &minimal_config_body(VALID_HASH));
        assert!(ServerConfig::load(&path).is_ok());
    }

    #[test]
    fn test_load_when_empty_username_returns_empty_username() {
        let dir = tempfile::tempdir().unwrap();
        let body =
            minimal_config_body(VALID_HASH).replace(r#"username = "alice""#, r#"username = """#);
        let path = write_config(&dir, "empty_user.toml", &body);
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::EmptyUsername));
    }

    #[test]
    fn test_load_when_duplicate_user_returns_duplicate_user() {
        let dir = tempfile::tempdir().unwrap();
        let mut body = minimal_config_body(VALID_HASH);
        body.push_str(&format!(
            "\n[[users]]\nusername = \"alice\"\npassword_hash = \"{VALID_HASH}\"\n"
        ));
        let path = write_config(&dir, "dup.toml", &body);
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateUser(ref n) if n == "alice"));
    }

    #[test]
    fn test_load_when_invalid_hash_returns_invalid_hash() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_config_body("not-a-valid-hash");
        let path = write_config(&dir, "bad_hash.toml", &body);
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidHash));
    }

    #[test]
    fn test_load_when_syntax_error_takes_precedence_over_validation() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_config_body(VALID_HASH).replace("mtu = 1280", "mtu = 1\nlisten = ");
        let path = write_config(&dir, "precedence.toml", &body);
        let err = ServerConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn test_generated_hash_round_trips_through_load() {
        let dir = tempfile::tempdir().unwrap();
        let body = minimal_config_body(&hash_password("s3cret"));
        let path = write_config(&dir, "gen_hash.toml", &body);
        assert!(ServerConfig::load(&path).is_ok());
    }

    #[test]
    fn test_map_user_error_invalid_credentials_is_tolerated() {
        let res: Result<(), ConfigError> = map_user_error(Err(AuthError::InvalidCredentials));
        assert!(res.is_ok());
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
    #[allow(clippy::indexing_slicing)]
    fn test_client_error_new_variants_display_distinct_from_existing() {
        let all = all_config_error_displays();
        assert_displays_unique(&all);
        assert!(all[7].contains("server_name"));
        assert!(all[8].contains("ca_cert"));
        assert!(all[9].contains("default route"));
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
