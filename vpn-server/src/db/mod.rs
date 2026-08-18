use std::sync::Arc;

use argon2::password_hash::PasswordHashString;
use async_trait::async_trait;
use sysprobe::proto::InfoKind;
use sysprobe::proto::TelemetryReport;
use sysprobe::sink::SinkSource;
use thiserror::Error;

pub mod sqlite;

#[cfg(test)]
pub(crate) mod contract;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("database io error: {0}")]
    Io(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

#[async_trait]
pub trait UserStore: Send + Sync {
    async fn password_hash(&self, username: &str) -> Result<Option<String>, DbError>;
    async fn upsert(&self, username: &str, phc: &str) -> Result<(), DbError>;
    async fn delete(&self, username: &str) -> Result<bool, DbError>;
    async fn list(&self) -> Result<Vec<String>, DbError>;
}

pub(crate) fn validate_upsert(username: &str, phc: &str) -> Result<(), DbError> {
    if username.is_empty() {
        return Err(DbError::InvalidInput("empty username".into()));
    }
    PasswordHashString::new(phc).map_err(|e| DbError::InvalidInput(format!("invalid phc: {e}")))?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryRow {
    pub id: String,
    pub session_id: i64,
    pub username: String,
    pub virtual_ip: Option<String>,
    pub report_ts_ms: i64,
    pub received_at_ms: i64,
    pub kind: i32,
    pub payload: Vec<u8>,
}

const DEFAULT_QUERY_LIMIT: u32 = 50;

#[derive(Debug, Clone)]
pub struct TelemetryFilter {
    pub username: Option<String>,
    pub kind: Option<InfoKind>,
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
    pub limit: u32,
}

impl Default for TelemetryFilter {
    fn default() -> Self {
        Self {
            username: None,
            kind: None,
            since_ms: None,
            until_ms: None,
            limit: DEFAULT_QUERY_LIMIT,
        }
    }
}

#[async_trait]
pub trait TelemetryStore: Send + Sync {
    async fn append(&self, source: &SinkSource, report: &TelemetryReport) -> Result<(), DbError>;
    async fn query(&self, filter: &TelemetryFilter) -> Result<Vec<TelemetryRow>, DbError>;
}

const SUPPORTED_SCHEMES: &str = "supported scheme: sqlite";

fn unsupported_scheme(url: &str) -> DbError {
    DbError::InvalidInput(format!(
        "unsupported database url {url}, {SUPPORTED_SCHEMES}"
    ))
}

pub async fn open_user_store(url: &str) -> Result<Arc<dyn UserStore>, DbError> {
    match url.split_once("://") {
        Some(("sqlite", _)) => {
            let store = sqlite::SqliteUserStore::connect(url).await?;
            Ok(Arc::new(store))
        }
        _ => Err(unsupported_scheme(url)),
    }
}

pub async fn open_telemetry_store(url: &str) -> Result<Arc<dyn TelemetryStore>, DbError> {
    match url.split_once("://") {
        Some(("sqlite", _)) => {
            let store = sqlite::SqliteTelemetryStore::connect(url).await?;
            Ok(Arc::new(store))
        }
        _ => Err(unsupported_scheme(url)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn valid_phc() -> String {
        "$argon2id$v=19$m=19456,t=2,p=1$j3xYVqWV0EE+AG6htXRGTA$g446kNT7dmrxnDjw/DZYHbCWrO83sNJtAdIqmWjcknE".to_string()
    }

    #[tokio::test]
    async fn test_open_user_store_sqlite_url_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("users.db").display());
        let store = open_user_store(&url).await.unwrap();
        store.upsert("alice", &valid_phc()).await.unwrap();
        assert_eq!(
            store.password_hash("alice").await.unwrap(),
            Some(valid_phc())
        );
    }

    #[tokio::test]
    async fn test_open_user_store_unknown_scheme_rejected() {
        let err = open_user_store("mongodb://host/db")
            .await
            .map(|_| ())
            .unwrap_err();
        let DbError::InvalidInput(msg) = err else {
            panic!("expected InvalidInput, got {err:?}");
        };
        assert!(msg.contains("mongodb"), "msg should mention scheme: {msg}");
    }

    #[tokio::test]
    async fn test_open_user_store_non_url_rejected() {
        let err = open_user_store("not-a-url").await.map(|_| ()).unwrap_err();
        assert!(matches!(err, DbError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn test_open_telemetry_store_sqlite_url_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("telemetry.db").display());
        let store = open_telemetry_store(&url).await.unwrap();
        let report = TelemetryReport {
            ts_ms: 42,
            items: vec![],
        };
        let source = SinkSource {
            session_id: 7,
            username: "alice".into(),
            virtual_ip: None,
        };
        store.append(&source, &report).await.unwrap();
        assert!(
            store
                .query(&TelemetryFilter::default())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_open_telemetry_store_unknown_scheme_rejected() {
        let err = open_telemetry_store("postgres://host/db")
            .await
            .map(|_| ())
            .unwrap_err();
        let DbError::InvalidInput(msg) = err else {
            panic!("expected InvalidInput, got {err:?}");
        };
        assert!(msg.contains("postgres"), "msg should mention scheme: {msg}");
    }
}
