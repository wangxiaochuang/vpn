use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::RwLockReadGuard;
use std::sync::RwLockWriteGuard;

use async_trait::async_trait;

use crate::StoreError;
use crate::UserStore;
use crate::validate_upsert;

#[derive(Debug, Default)]
pub struct InMemoryUserStore {
    users: RwLock<HashMap<String, String>>,
}

fn poisoned() -> StoreError {
    StoreError::Io(Box::new(std::io::Error::other("lock poisoned")))
}

impl InMemoryUserStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_pairs<I>(pairs: I) -> Result<Self, StoreError>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let mut map = HashMap::new();
        for (username, phc) in pairs {
            validate_upsert(&username, &phc)?;
            map.insert(username, phc);
        }
        Ok(Self {
            users: RwLock::new(map),
        })
    }

    fn read(&self) -> Result<RwLockReadGuard<'_, HashMap<String, String>>, StoreError> {
        self.users.read().map_err(|_| poisoned())
    }

    fn write(&self) -> Result<RwLockWriteGuard<'_, HashMap<String, String>>, StoreError> {
        self.users.write().map_err(|_| poisoned())
    }
}

#[async_trait]
impl UserStore for InMemoryUserStore {
    async fn password_hash(&self, username: &str) -> Result<Option<String>, StoreError> {
        Ok(self.read()?.get(username).cloned())
    }

    async fn upsert(&self, username: &str, phc: &str) -> Result<(), StoreError> {
        validate_upsert(username, phc)?;
        self.write()?.insert(username.to_string(), phc.to_string());
        Ok(())
    }

    async fn delete(&self, username: &str) -> Result<bool, StoreError> {
        Ok(self.write()?.remove(username).is_some())
    }

    async fn list(&self) -> Result<Vec<String>, StoreError> {
        let mut names: Vec<String> = self.read()?.keys().cloned().collect();
        names.sort();
        Ok(names)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::contract;

    #[tokio::test]
    async fn test_memory_lookup_on_empty_store_returns_none() {
        contract::lookup_on_empty_store_returns_none(&InMemoryUserStore::new()).await;
    }

    #[tokio::test]
    async fn test_memory_upsert_then_lookup_round_trips() {
        contract::upsert_then_lookup_round_trips(&InMemoryUserStore::new()).await;
    }

    #[tokio::test]
    async fn test_memory_upsert_empty_username_rejected_without_write() {
        contract::upsert_empty_username_rejected_without_write(&InMemoryUserStore::new()).await;
    }

    #[tokio::test]
    async fn test_memory_upsert_malformed_phc_rejected_without_write() {
        contract::upsert_malformed_phc_rejected_without_write(&InMemoryUserStore::new()).await;
    }

    #[tokio::test]
    async fn test_memory_upsert_same_user_updates_in_place() {
        contract::upsert_same_user_updates_in_place(&InMemoryUserStore::new()).await;
    }

    #[tokio::test]
    async fn test_memory_delete_existing_user_returns_true_and_clears() {
        contract::delete_existing_user_returns_true_and_clears(&InMemoryUserStore::new()).await;
    }

    #[tokio::test]
    async fn test_memory_delete_missing_user_returns_false() {
        contract::delete_missing_user_returns_false(&InMemoryUserStore::new()).await;
    }

    #[tokio::test]
    async fn test_memory_list_is_sorted_and_stable() {
        contract::list_is_sorted_and_stable(&InMemoryUserStore::new()).await;
    }

    #[tokio::test]
    async fn test_from_pairs_populates_store_and_lookup() {
        let phc = "$argon2id$v=19$m=19456,t=2,p=1$j3xYVqWV0EE+AG6htXRGTA$g446kNT7dmrxnDjw/DZYHbCWrO83sNJtAdIqmWjcknE";
        let store = InMemoryUserStore::from_pairs([("alice".to_string(), phc.to_string())])
            .expect("valid pairs");
        assert_eq!(
            store.password_hash("alice").await.unwrap(),
            Some(phc.to_string())
        );
        assert_eq!(store.list().await.unwrap(), vec!["alice".to_string()]);
    }

    #[tokio::test]
    async fn test_from_pairs_malformed_phc_returns_invalid_input() {
        let err =
            InMemoryUserStore::from_pairs([("alice".to_string(), "bad".to_string())]).unwrap_err();
        assert!(matches!(err, StoreError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn test_dropped_future_releases_lock() {
        let store = InMemoryUserStore::new();
        let fut = store.password_hash("alice");
        drop(fut);
        assert_eq!(store.password_hash("alice").await.unwrap(), None);
    }
}
