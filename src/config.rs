use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use ipnet::Ipv4Net;
use serde::Deserialize;
use thiserror::Error;

use crate::auth::AuthError;
use crate::auth::UserStore;
use crate::ipam::IpPool;
const MIN_MTU: u16 = 1280;

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

        if server.mtu < MIN_MTU {
            return Err(ConfigError::MtuTooSmall(server.mtu));
        }

        let tun_subnet = server.tun_subnet;
        IpPool::new(tun_subnet).map_err(|_| ConfigError::InvalidSubnet)?;

        let users: Vec<UserConfig> = raw
            .users
            .into_iter()
            .map(|u| UserConfig {
                username: u.username,
                password_hash: u.password_hash,
            })
            .collect();

        let user_pairs: Vec<(String, String)> = users
            .iter()
            .map(|u| (u.username.clone(), u.password_hash.clone()))
            .collect();
        map_user_error(UserStore::from_users(user_pairs))?;

        Ok(Self {
            listen: server.listen,
            tun_subnet,
            mtu: server.mtu,
            cert: server.cert,
            key: server.key,
            users,
        })
    }
}

fn map_user_error(res: Result<UserStore, AuthError>) -> Result<(), ConfigError> {
    match res {
        Ok(_) | Err(AuthError::InvalidCredentials) => Ok(()),
        Err(AuthError::EmptyUsername) => Err(ConfigError::EmptyUsername),
        Err(AuthError::DuplicateUser(name)) => Err(ConfigError::DuplicateUser(name)),
        Err(AuthError::InvalidHash) => Err(ConfigError::InvalidHash),
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
}

fn deserialize_ipv4_net<'de, D>(deserializer: D) -> Result<Ipv4Net, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let s = String::deserialize(deserializer)?;
    s.parse().map_err(serde::de::Error::custom)
}

#[derive(Debug, Deserialize)]
struct RawUser {
    username: String,
    password_hash: String,
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

    #[test]
    fn test_config_error_display_variants_are_distinct() {
        let io = ConfigError::Io(io_stub()).to_string();
        let parse = ConfigError::Parse(toml::from_str::<serde::de::IgnoredAny>("x =").unwrap_err())
            .to_string();
        let mtu = ConfigError::MtuTooSmall(1000).to_string();
        let subnet = ConfigError::InvalidSubnet.to_string();
        let empty = ConfigError::EmptyUsername.to_string();
        let dup = ConfigError::DuplicateUser("alice".into()).to_string();
        let hash = ConfigError::InvalidHash.to_string();

        let all = [&io, &parse, &mtu, &subnet, &empty, &dup, &hash];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j]);
            }
        }
        assert!(io.contains("read config file"));
        assert!(parse.contains("parse"));
        assert!(mtu.contains("1000") && mtu.contains("1280"));
        assert!(subnet.contains("subnet"));
        assert!(empty.contains("empty"));
        assert!(dup.contains("alice"));
        assert!(hash.contains("hash"));
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
}
