use std::str::FromStr;

use async_trait::async_trait;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqliteJournalMode;
use sqlx::sqlite::SqlitePoolOptions;

use crate::StoreError;
use crate::UserStore;
use crate::validate_upsert;

const CREATE_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS users (\
    username TEXT PRIMARY KEY, password_hash TEXT NOT NULL)";

#[derive(Debug)]
pub struct SqliteUserStore {
    pool: SqlitePool,
}

fn io_err(e: sqlx::Error) -> StoreError {
    StoreError::Io(Box::new(e))
}

impl SqliteUserStore {
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        let options = connect_options(url)?;
        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .map_err(io_err)?;
        sqlx::query(CREATE_TABLE_SQL)
            .execute(&pool)
            .await
            .map_err(io_err)?;
        Ok(Self { pool })
    }
}

fn connect_options(url: &str) -> Result<SqliteConnectOptions, StoreError> {
    if !url.starts_with("sqlite:") {
        return Err(StoreError::InvalidInput(format!(
            "invalid sqlite url: {url}"
        )));
    }
    let options = SqliteConnectOptions::from_str(url)
        .map_err(|e| StoreError::InvalidInput(format!("invalid sqlite url: {e}")))?;
    Ok(options
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal))
}

#[async_trait]
impl UserStore for SqliteUserStore {
    async fn password_hash(&self, username: &str) -> Result<Option<String>, StoreError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT password_hash FROM users WHERE username = ?")
                .bind(username)
                .fetch_optional(&self.pool)
                .await
                .map_err(io_err)?;
        Ok(row.map(|r| r.0))
    }

    async fn upsert(&self, username: &str, phc: &str) -> Result<(), StoreError> {
        validate_upsert(username, phc)?;
        sqlx::query(
            "INSERT INTO users (username, password_hash) VALUES (?, ?) \
             ON CONFLICT(username) DO UPDATE SET password_hash = excluded.password_hash",
        )
        .bind(username)
        .bind(phc)
        .execute(&self.pool)
        .await
        .map_err(io_err)?;
        Ok(())
    }

    async fn delete(&self, username: &str) -> Result<bool, StoreError> {
        let result = sqlx::query("DELETE FROM users WHERE username = ?")
            .bind(username)
            .execute(&self.pool)
            .await
            .map_err(io_err)?;
        Ok(result.rows_affected() > 0)
    }

    async fn list(&self) -> Result<Vec<String>, StoreError> {
        let rows: Vec<(String,)> = sqlx::query_as("SELECT username FROM users ORDER BY username")
            .fetch_all(&self.pool)
            .await
            .map_err(io_err)?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::InMemoryUserStore;
    use crate::contract;
    use std::time::Duration;

    fn temp_url(dir: &tempfile::TempDir) -> String {
        format!("sqlite://{}", dir.path().join("users.db").display())
    }

    async fn temp_store() -> (tempfile::TempDir, SqliteUserStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteUserStore::connect(&temp_url(&dir)).await.unwrap();
        (dir, store)
    }

    fn valid_phc() -> String {
        "$argon2id$v=19$m=19456,t=2,p=1$j3xYVqWV0EE+AG6htXRGTA$g446kNT7dmrxnDjw/DZYHbCWrO83sNJtAdIqmWjcknE".to_string()
    }

    #[tokio::test]
    async fn test_sqlite_lookup_on_empty_store_returns_none() {
        let (_dir, store) = temp_store().await;
        contract::lookup_on_empty_store_returns_none(&store).await;
    }

    #[tokio::test]
    async fn test_sqlite_upsert_then_lookup_round_trips() {
        let (_dir, store) = temp_store().await;
        contract::upsert_then_lookup_round_trips(&store).await;
    }

    #[tokio::test]
    async fn test_sqlite_upsert_empty_username_rejected_without_write() {
        let (_dir, store) = temp_store().await;
        contract::upsert_empty_username_rejected_without_write(&store).await;
    }

    #[tokio::test]
    async fn test_sqlite_upsert_malformed_phc_rejected_without_write() {
        let (_dir, store) = temp_store().await;
        contract::upsert_malformed_phc_rejected_without_write(&store).await;
    }

    #[tokio::test]
    async fn test_sqlite_upsert_same_user_updates_in_place() {
        let (_dir, store) = temp_store().await;
        contract::upsert_same_user_updates_in_place(&store).await;
    }

    #[tokio::test]
    async fn test_sqlite_delete_existing_user_returns_true_and_clears() {
        let (_dir, store) = temp_store().await;
        contract::delete_existing_user_returns_true_and_clears(&store).await;
    }

    #[tokio::test]
    async fn test_sqlite_delete_missing_user_returns_false() {
        let (_dir, store) = temp_store().await;
        contract::delete_missing_user_returns_false(&store).await;
    }

    #[tokio::test]
    async fn test_sqlite_list_is_sorted_and_stable() {
        let (_dir, store) = temp_store().await;
        contract::list_is_sorted_and_stable(&store).await;
    }

    #[tokio::test]
    async fn test_connect_creates_database_file() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("created.db");
        let url = format!("sqlite://{}", db_path.display());
        let store = SqliteUserStore::connect(&url).await.unwrap();
        store.upsert("alice", &valid_phc()).await.unwrap();
        assert!(db_path.exists());
    }

    #[tokio::test]
    async fn test_connect_twice_on_existing_db_is_idempotent() {
        let (dir, store) = temp_store().await;
        store.upsert("alice", &valid_phc()).await.unwrap();
        drop(store);
        let reopened = SqliteUserStore::connect(&temp_url(&dir)).await.unwrap();
        assert_eq!(
            reopened.password_hash("alice").await.unwrap(),
            Some(valid_phc())
        );
    }

    #[tokio::test]
    async fn test_connect_invalid_url_returns_invalid_input() {
        let err = SqliteUserStore::connect("not-a-url").await.unwrap_err();
        assert!(matches!(err, StoreError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn test_connect_unwritable_path_returns_io_error() {
        let err = SqliteUserStore::connect("sqlite:///nonexistent-dir/users.db")
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Io(_)));
    }

    #[tokio::test]
    async fn test_sqlite_and_memory_agree_after_upserts() {
        let (_dir, sqlite) = temp_store().await;
        let memory = InMemoryUserStore::new();
        let phc1 = valid_phc();
        let phc2 = valid_phc();
        for phc in [&phc1, &phc2] {
            sqlite.upsert("alice", phc).await.unwrap();
            memory.upsert("alice", phc).await.unwrap();
            assert_eq!(
                sqlite.password_hash("alice").await.unwrap(),
                memory.password_hash("alice").await.unwrap()
            );
            assert_eq!(
                sqlite.password_hash("alice").await.unwrap(),
                Some(phc.clone())
            );
        }
        assert_eq!(sqlite.list().await.unwrap(), memory.list().await.unwrap());
    }

    #[tokio::test]
    async fn test_sqlite_and_memory_agree_after_delete() {
        let (_dir, sqlite) = temp_store().await;
        let memory = InMemoryUserStore::new();
        for store in [
            &sqlite as &dyn crate::UserStore,
            &memory as &dyn crate::UserStore,
        ] {
            store.upsert("alice", &valid_phc()).await.unwrap();
        }
        for _ in [0, 1] {
            assert_eq!(
                sqlite.delete("alice").await.unwrap(),
                memory.delete("alice").await.unwrap()
            );
        }
        assert_eq!(
            sqlite.password_hash("alice").await.unwrap(),
            memory.password_hash("alice").await.unwrap()
        );
        assert_eq!(sqlite.list().await.unwrap(), memory.list().await.unwrap());
    }

    async fn seed_users(store: &SqliteUserStore, count: usize) {
        for i in 0..count {
            store
                .upsert(&format!("user{i:02}"), &valid_phc())
                .await
                .unwrap();
        }
    }

    fn spawn_reader(store: std::sync::Arc<SqliteUserStore>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            for _ in 0..50 {
                assert!(store.password_hash("user05").await.unwrap().is_some());
            }
        })
    }

    fn spawn_writer(store: std::sync::Arc<SqliteUserStore>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            for _ in 0..50 {
                store.upsert("writer", &valid_phc()).await.unwrap();
            }
        })
    }

    #[tokio::test]
    async fn test_concurrent_read_and_write_complete_without_deadlock() {
        let (_dir, store) = temp_store().await;
        seed_users(&store, 10).await;
        let store = std::sync::Arc::new(store);
        let reader = spawn_reader(store.clone());
        let writer = spawn_writer(store);
        let timeout = Duration::from_secs(30);
        tokio::time::timeout(timeout, async {
            reader.await.unwrap();
            writer.await.unwrap();
        })
        .await
        .expect("no deadlock under concurrent read/write");
    }

    #[tokio::test]
    async fn test_dropped_query_future_keeps_store_usable() {
        let (_dir, store) = temp_store().await;
        store.upsert("alice", &valid_phc()).await.unwrap();
        let fut = store.password_hash("alice");
        drop(fut);
        assert_eq!(
            store.password_hash("alice").await.unwrap(),
            Some(valid_phc())
        );
    }
}
