use std::sync::Arc;

use crate::db::UserStore;
use argon2::{Argon2, PasswordHash, PasswordVerifier};
use async_trait::async_trait;
use thiserror::Error;
use tracing::error;
use tracing::warn;
use vpn_core::vpn::AuthChallenge;
use vpn_core::vpn::AuthInit;
use vpn_core::vpn::AuthResponse;
use vpn_core::vpn::auth_init;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AuthError {
    #[error("invalid credentials")]
    InvalidCredentials,
}

fn verify_dummy(password: &str) {
    let _ = PasswordHash::new(DUMMY_HASH)
        .ok()
        .map(|h| Argon2::default().verify_password(password.as_bytes(), &h));
}

const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$j3xYVqWV0EE+AG6htXRGTA$g446kNT7dmrxnDjw/DZYHbCWrO83sNJtAdIqmWjcknE";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity(pub String);

pub enum AuthOutcome {
    Completed(Identity),
    Challenge(Box<dyn AuthChallengeHandler>),
    Denied(AuthError),
}

impl std::fmt::Debug for AuthOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed(id) => f.debug_tuple("Completed").field(id).finish(),
            Self::Challenge(_) => f.debug_tuple("Challenge").finish_non_exhaustive(),
            Self::Denied(e) => f.debug_tuple("Denied").field(e).finish(),
        }
    }
}

#[async_trait]
pub trait Authenticator: Send + Sync {
    async fn begin(&self, init: AuthInit) -> AuthOutcome;
}

#[async_trait]
pub trait AuthChallengeHandler: Send {
    fn describe(&self) -> AuthChallenge;
    async fn respond(&mut self, response: AuthResponse) -> AuthOutcome;
}

pub struct PasswordAuthenticator {
    store: Arc<dyn UserStore>,
}

impl PasswordAuthenticator {
    pub fn new(store: Arc<dyn UserStore>) -> Self {
        Self { store }
    }

    async fn authenticate(&self, username: &str, password: &str) -> AuthOutcome {
        let stored = match self.store.password_hash(username).await {
            Ok(stored) => stored,
            Err(e) => {
                error!("user store query failed: {e}");
                return AuthOutcome::Denied(AuthError::InvalidCredentials);
            }
        };
        match stored {
            None => {
                verify_dummy(password);
                AuthOutcome::Denied(AuthError::InvalidCredentials)
            }
            Some(phc) => verify_stored(username, password, &phc),
        }
    }
}

fn verify_stored(username: &str, password: &str, phc: &str) -> AuthOutcome {
    match PasswordHash::new(phc) {
        Err(_) => {
            warn!("malformed password hash for user {username}");
            verify_dummy(password);
            AuthOutcome::Denied(AuthError::InvalidCredentials)
        }
        Ok(hash) => verify_password(username, password, &hash),
    }
}

fn verify_password(username: &str, password: &str, hash: &PasswordHash<'_>) -> AuthOutcome {
    match Argon2::default().verify_password(password.as_bytes(), hash) {
        Ok(()) => AuthOutcome::Completed(Identity(username.to_string())),
        Err(_) => AuthOutcome::Denied(AuthError::InvalidCredentials),
    }
}

#[async_trait]
impl Authenticator for PasswordAuthenticator {
    async fn begin(&self, init: AuthInit) -> AuthOutcome {
        let password = extract_password(&init);
        self.authenticate(&init.username, &password).await
    }
}

fn extract_password(init: &AuthInit) -> String {
    match init.method.as_ref() {
        Some(auth_init::Method::Password(pw)) => pw.password.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::db::DbError;
    use crate::db::open_user_store;
    use argon2::PasswordHasher;
    use argon2::password_hash::SaltString;
    use argon2::password_hash::rand_core::OsRng;

    struct FailingStore;

    #[async_trait]
    impl UserStore for FailingStore {
        async fn password_hash(&self, _username: &str) -> Result<Option<String>, DbError> {
            Err(DbError::Io(Box::new(std::io::Error::other("db down"))))
        }
        async fn upsert(&self, _username: &str, _phc: &str) -> Result<(), DbError> {
            Ok(())
        }
        async fn delete(&self, _username: &str) -> Result<bool, DbError> {
            Ok(false)
        }
        async fn list(&self) -> Result<Vec<String>, DbError> {
            Ok(Vec::new())
        }
    }

    struct MalformedHashStore;

    #[async_trait]
    impl UserStore for MalformedHashStore {
        async fn password_hash(&self, _username: &str) -> Result<Option<String>, DbError> {
            Ok(Some("not-a-valid-hash".to_string()))
        }
        async fn upsert(&self, _username: &str, _phc: &str) -> Result<(), DbError> {
            Ok(())
        }
        async fn delete(&self, _username: &str) -> Result<bool, DbError> {
            Ok(false)
        }
        async fn list(&self) -> Result<Vec<String>, DbError> {
            Ok(Vec::new())
        }
    }

    fn hash_password(pw: &str) -> String {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(pw.as_bytes(), &salt)
            .unwrap()
            .to_string()
    }

    type TestStores = (PasswordAuthenticator, Arc<dyn UserStore>, tempfile::TempDir);

    async fn make_authenticators() -> TestStores {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("users.db").display());
        let store = open_user_store(&url).await.unwrap();
        store
            .upsert("alice", &hash_password("s3cret"))
            .await
            .unwrap();
        (PasswordAuthenticator::new(store.clone()), store, dir)
    }

    fn password_init(username: &str, password: &str) -> AuthInit {
        use vpn_core::vpn::auth_init::Method;
        AuthInit {
            username: username.to_string(),
            method: Some(Method::Password(vpn_core::vpn::PasswordAuth {
                password: password.to_string(),
            })),
        }
    }

    fn assert_invalid_credentials(outcome: &AuthOutcome) {
        assert!(
            matches!(outcome, AuthOutcome::Denied(e) if *e == AuthError::InvalidCredentials),
            "expected Denied(InvalidCredentials), got {outcome:?}"
        );
    }

    #[test]
    fn test_auth_error_display() {
        assert_eq!(
            AuthError::InvalidCredentials.to_string(),
            "invalid credentials"
        );
    }

    #[tokio::test]
    async fn test_begin_when_correct_credentials_returns_completed() {
        let (auth, _, _) = make_authenticators().await;
        let outcome = auth.begin(password_init("alice", "s3cret")).await;
        assert!(matches!(
            outcome,
            AuthOutcome::Completed(Identity(ref id)) if id == "alice"
        ));
    }

    #[tokio::test]
    async fn test_begin_when_wrong_password_returns_denied() {
        let (auth, _, _) = make_authenticators().await;
        assert_invalid_credentials(&auth.begin(password_init("alice", "wrong")).await);
    }

    #[tokio::test]
    async fn test_begin_when_unknown_user_returns_denied() {
        let (auth, _, _) = make_authenticators().await;
        assert_invalid_credentials(&auth.begin(password_init("eve", "anything")).await);
    }

    #[tokio::test]
    async fn test_begin_when_empty_username_returns_denied() {
        let (auth, _, _) = make_authenticators().await;
        assert_invalid_credentials(&auth.begin(password_init("", "s3cret")).await);
    }

    #[tokio::test]
    async fn test_begin_when_different_case_returns_denied() {
        let (auth, _, _) = make_authenticators().await;
        assert_invalid_credentials(&auth.begin(password_init("Alice", "s3cret")).await);
    }

    #[tokio::test]
    async fn test_begin_when_leading_space_returns_denied() {
        let (auth, _, _) = make_authenticators().await;
        assert_invalid_credentials(&auth.begin(password_init(" alice", "s3cret")).await);
    }

    #[tokio::test]
    async fn test_begin_when_user_upserted_takes_effect_immediately() {
        let (auth, store, _) = make_authenticators().await;
        store.upsert("bob", &hash_password("pw2")).await.unwrap();
        assert!(matches!(
            auth.begin(password_init("bob", "pw2")).await,
            AuthOutcome::Completed(Identity(ref id)) if id == "bob"
        ));
    }

    #[tokio::test]
    async fn test_begin_when_password_rotated_old_password_denied() {
        let (auth, store, _) = make_authenticators().await;
        store
            .upsert("alice", &hash_password("new-pw"))
            .await
            .unwrap();
        assert_invalid_credentials(&auth.begin(password_init("alice", "s3cret")).await);
        assert!(matches!(
            auth.begin(password_init("alice", "new-pw")).await,
            AuthOutcome::Completed(_)
        ));
    }

    #[tokio::test]
    async fn test_begin_when_user_deleted_denies_with_dummy_path() {
        let (auth, store, _) = make_authenticators().await;
        assert!(store.delete("alice").await.unwrap());
        assert_invalid_credentials(&auth.begin(password_init("alice", "s3cret")).await);
    }

    #[tokio::test]
    async fn test_begin_when_store_fails_returns_denied_fail_closed() {
        let auth = PasswordAuthenticator::new(Arc::new(FailingStore));
        assert_invalid_credentials(&auth.begin(password_init("alice", "s3cret")).await);
    }

    #[tokio::test]
    async fn test_begin_when_malformed_hash_returns_denied() {
        let auth = PasswordAuthenticator::new(Arc::new(MalformedHashStore));
        assert_invalid_credentials(&auth.begin(password_init("alice", "s3cret")).await);
    }

    #[tokio::test]
    async fn test_begin_returns_identity_equal_to_username() {
        let (auth, _, _) = make_authenticators().await;
        let outcome = auth.begin(password_init("alice", "s3cret")).await;
        let AuthOutcome::Completed(identity) = outcome else {
            panic!("expected Completed, got {outcome:?}");
        };
        assert_eq!(identity.0, "alice");
    }
}
