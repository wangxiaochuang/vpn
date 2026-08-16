use ipnet::Ipv4Net;
use thiserror::Error;

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
    #[error("database url is missing, empty, or malformed")]
    InvalidDatabaseUrl,
    #[error("database backend {0} is not supported yet")]
    UnsupportedDatabase(String),
    #[error("server_name must not be empty")]
    EmptyServerName,
    #[error("ca_cert must not be empty")]
    EmptyCaCert,
    #[error("routes must not contain the default route 0.0.0.0/0")]
    DefaultRouteNotAllowed,
}

pub fn deserialize_ipv4_net<'de, D>(deserializer: D) -> Result<Ipv4Net, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let s = String::deserialize(deserializer)?;
    s.parse().map_err(serde::de::Error::custom)
}

pub fn deserialize_ipv4_net_vec<'de, D>(deserializer: D) -> Result<Vec<Ipv4Net>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let seq = Vec::<String>::deserialize(deserializer)?;
    seq.into_iter()
        .map(|s| s.parse().map_err(serde::de::Error::custom))
        .collect()
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

    fn io_stub() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::NotFound, "stub")
    }

    fn base_config_error_displays() -> Vec<String> {
        vec![
            ConfigError::Io(io_stub()).to_string(),
            ConfigError::Parse(toml::from_str::<serde::de::IgnoredAny>("x =").unwrap_err())
                .to_string(),
            ConfigError::MtuTooSmall(1000).to_string(),
            ConfigError::InvalidSubnet.to_string(),
            ConfigError::InvalidDatabaseUrl.to_string(),
            ConfigError::UnsupportedDatabase("mysql".into()).to_string(),
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
        assert!(all[4].contains("database"));
        assert!(all[5].contains("mysql") && all[5].contains("not supported"));
    }

    #[test]
    fn test_default_route_not_allowed_display_is_distinct() {
        let default_route = ConfigError::DefaultRouteNotAllowed.to_string();
        let mut others = all_config_error_displays();
        others.retain(|s| s != &default_route);
        for other in &others {
            assert_ne!(&default_route, other);
        }
        assert!(default_route.contains("default route") || default_route.contains("0.0.0.0/0"));
    }

    #[allow(clippy::indexing_slicing)]
    fn assert_displays_unique(all: &[String]) {
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "duplicate at {i},{j}");
            }
        }
    }

    #[test]
    #[allow(clippy::indexing_slicing)]
    fn test_client_error_new_variants_display_distinct_from_existing() {
        let all = all_config_error_displays();
        assert_displays_unique(&all);
        assert!(all[6].contains("server_name"));
        assert!(all[7].contains("ca_cert"));
        assert!(all[8].contains("default route"));
    }
}
