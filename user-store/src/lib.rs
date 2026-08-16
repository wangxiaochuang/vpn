#![warn(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::dbg_macro,
    clippy::print_stdout,
    clippy::print_stderr
)]
#![allow(
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

pub mod memory;
pub mod sqlite;

pub use memory::InMemoryUserStore;
pub use sqlite::SqliteUserStore;

use argon2::password_hash::PasswordHashString;
use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("store io error: {0}")]
    Io(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

#[async_trait]
pub trait UserStore: Send + Sync {
    async fn password_hash(&self, username: &str) -> Result<Option<String>, StoreError>;
    async fn upsert(&self, username: &str, phc: &str) -> Result<(), StoreError>;
    async fn delete(&self, username: &str) -> Result<bool, StoreError>;
    async fn list(&self) -> Result<Vec<String>, StoreError>;
}

pub(crate) fn validate_upsert(username: &str, phc: &str) -> Result<(), StoreError> {
    if username.is_empty() {
        return Err(StoreError::InvalidInput("empty username".into()));
    }
    PasswordHashString::new(phc)
        .map_err(|e| StoreError::InvalidInput(format!("invalid phc: {e}")))?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod contract {
    use super::*;

    fn hash_password(pw: &str) -> String {
        use argon2::PasswordHasher;
        use argon2::password_hash::SaltString;
        use argon2::password_hash::rand_core::OsRng;
        let salt = SaltString::generate(&mut OsRng);
        argon2::Argon2::default()
            .hash_password(pw.as_bytes(), &salt)
            .unwrap()
            .to_string()
    }

    pub(crate) async fn lookup_on_empty_store_returns_none(store: &dyn UserStore) {
        assert_eq!(store.password_hash("alice").await.unwrap(), None);
    }

    pub(crate) async fn upsert_then_lookup_round_trips(store: &dyn UserStore) {
        let phc = hash_password("s3cret");
        store.upsert("alice", &phc).await.unwrap();
        assert_eq!(store.password_hash("alice").await.unwrap(), Some(phc));
    }

    pub(crate) async fn upsert_empty_username_rejected_without_write(store: &dyn UserStore) {
        let err = store
            .upsert("", &hash_password("s3cret"))
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::InvalidInput(_)));
        assert!(store.list().await.unwrap().is_empty());
    }

    pub(crate) async fn upsert_malformed_phc_rejected_without_write(store: &dyn UserStore) {
        let err = store.upsert("alice", "not-a-valid-hash").await.unwrap_err();
        assert!(matches!(err, StoreError::InvalidInput(_)));
        assert_eq!(store.password_hash("alice").await.unwrap(), None);
    }

    pub(crate) async fn upsert_same_user_updates_in_place(store: &dyn UserStore) {
        let first = hash_password("one");
        let second = hash_password("two");
        store.upsert("alice", &first).await.unwrap();
        store.upsert("alice", &second).await.unwrap();
        assert_eq!(store.list().await.unwrap(), vec!["alice".to_string()]);
        assert_eq!(store.password_hash("alice").await.unwrap(), Some(second));
    }

    pub(crate) async fn delete_existing_user_returns_true_and_clears(store: &dyn UserStore) {
        store
            .upsert("alice", &hash_password("s3cret"))
            .await
            .unwrap();
        assert!(store.delete("alice").await.unwrap());
        assert_eq!(store.password_hash("alice").await.unwrap(), None);
        assert!(store.list().await.unwrap().is_empty());
    }

    pub(crate) async fn delete_missing_user_returns_false(store: &dyn UserStore) {
        assert!(!store.delete("alice").await.unwrap());
    }

    pub(crate) async fn list_is_sorted_and_stable(store: &dyn UserStore) {
        for name in ["carol", "alice", "bob"] {
            store.upsert(name, &hash_password("s3cret")).await.unwrap();
        }
        let expected = vec!["alice", "bob", "carol"];
        for _ in 0..2 {
            let got: Vec<String> = store.list().await.unwrap();
            let got: Vec<&str> = got.iter().map(String::as_str).collect();
            assert_eq!(got, expected);
        }
    }
}
