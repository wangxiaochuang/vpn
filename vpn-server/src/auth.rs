use std::collections::HashMap;

use argon2::password_hash::PasswordHashString;
use argon2::{Argon2, PasswordHash, PasswordVerifier};
use async_trait::async_trait;
use thiserror::Error;
use vpn_core::vpn::AuthChallenge;
use vpn_core::vpn::AuthInit;
use vpn_core::vpn::AuthResponse;
use vpn_core::vpn::auth_init;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AuthError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("invalid password hash format")]
    InvalidHash,
    #[error("empty username is not allowed")]
    EmptyUsername,
    #[error("duplicate user: {0}")]
    DuplicateUser(String),
}

#[derive(Debug, Clone)]
pub struct UserStore {
    users: HashMap<String, PasswordHashString>,
}

impl UserStore {
    pub fn from_users<I>(users: I) -> Result<Self, AuthError>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let mut map = HashMap::new();
        for (username, phc) in users {
            if username.is_empty() {
                return Err(AuthError::EmptyUsername);
            }
            if map.contains_key(&username) {
                return Err(AuthError::DuplicateUser(username));
            }
            let hash = PasswordHashString::new(&phc).map_err(|_| AuthError::InvalidHash)?;
            map.insert(username, hash);
        }
        Ok(Self { users: map })
    }

    pub fn verify(&self, username: &str, password: &str) -> Result<(), AuthError> {
        if let Some(phc) = self.users.get(username) {
            if Argon2::default()
                .verify_password(password.as_bytes(), &phc.password_hash())
                .is_ok()
            {
                Ok(())
            } else {
                Err(AuthError::InvalidCredentials)
            }
        } else {
            verify_dummy(password);
            Err(AuthError::InvalidCredentials)
        }
    }
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
    store: UserStore,
}

impl PasswordAuthenticator {
    pub fn new(store: UserStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Authenticator for PasswordAuthenticator {
    async fn begin(&self, init: AuthInit) -> AuthOutcome {
        let password = extract_password(&init);
        match self.store.verify(&init.username, &password) {
            Ok(()) => AuthOutcome::Completed(Identity(init.username)),
            Err(e) => AuthOutcome::Denied(e),
        }
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
    use argon2::PasswordHasher;
    use argon2::password_hash::SaltString;
    use argon2::password_hash::rand_core::OsRng;

    fn hash_password(pw: &str) -> String {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(pw.as_bytes(), &salt)
            .unwrap()
            .to_string()
    }

    #[test]
    fn test_auth_error_display() {
        assert_eq!(
            AuthError::InvalidCredentials.to_string(),
            "invalid credentials"
        );
        assert_eq!(
            AuthError::InvalidHash.to_string(),
            "invalid password hash format"
        );
        assert_eq!(
            AuthError::EmptyUsername.to_string(),
            "empty username is not allowed"
        );
        assert_eq!(
            AuthError::DuplicateUser("bob".to_string()).to_string(),
            "duplicate user: bob"
        );
    }

    #[test]
    fn test_from_users_when_valid_hash_constructs_and_verifies_ok() {
        let store = UserStore::from_users([("alice".to_string(), hash_password("s3cret"))])
            .expect("valid users construct");
        assert_eq!(store.verify("alice", "s3cret"), Ok(()));
    }

    #[test]
    fn test_from_users_when_empty_list_constructs_empty_store() {
        let store = UserStore::from_users(Vec::<(String, String)>::new()).unwrap();
        assert_eq!(
            store.verify("anyone", "anything"),
            Err(AuthError::InvalidCredentials)
        );
    }

    #[test]
    fn test_from_users_when_invalid_hash_returns_invalid_hash() {
        let err = UserStore::from_users([("alice".to_string(), "not-a-valid-hash".to_string())])
            .unwrap_err();
        assert_eq!(err, AuthError::InvalidHash);
    }

    #[test]
    fn test_from_users_when_empty_username_returns_empty_username() {
        let err = UserStore::from_users([(String::new(), hash_password("s3cret"))]).unwrap_err();
        assert_eq!(err, AuthError::EmptyUsername);
    }

    #[test]
    fn test_from_users_when_duplicate_username_returns_duplicate_user() {
        let err = UserStore::from_users([
            ("alice".to_string(), hash_password("s3cret")),
            ("alice".to_string(), hash_password("other")),
        ])
        .unwrap_err();
        assert_eq!(err, AuthError::DuplicateUser("alice".to_string()));
    }

    #[test]
    fn test_verify_when_correct_credentials_returns_ok() {
        let store =
            UserStore::from_users([("alice".to_string(), hash_password("s3cret"))]).unwrap();
        assert_eq!(store.verify("alice", "s3cret"), Ok(()));
    }

    #[test]
    fn test_verify_when_wrong_password_returns_invalid_credentials() {
        let store =
            UserStore::from_users([("alice".to_string(), hash_password("s3cret"))]).unwrap();
        assert_eq!(
            store.verify("alice", "wrong"),
            Err(AuthError::InvalidCredentials)
        );
    }

    #[test]
    fn test_verify_when_unknown_user_returns_invalid_credentials() {
        let store =
            UserStore::from_users([("alice".to_string(), hash_password("s3cret"))]).unwrap();
        assert_eq!(
            store.verify("eve", "anything"),
            Err(AuthError::InvalidCredentials)
        );
    }

    #[test]
    fn test_verify_when_different_case_returns_invalid_credentials() {
        let store =
            UserStore::from_users([("alice".to_string(), hash_password("s3cret"))]).unwrap();
        assert_eq!(
            store.verify("Alice", "s3cret"),
            Err(AuthError::InvalidCredentials)
        );
    }

    #[test]
    fn test_verify_when_leading_space_returns_invalid_credentials() {
        let store =
            UserStore::from_users([("alice".to_string(), hash_password("s3cret"))]).unwrap();
        assert_eq!(
            store.verify(" alice", "s3cret"),
            Err(AuthError::InvalidCredentials)
        );
    }

    fn make_password_authenticator() -> PasswordAuthenticator {
        let store =
            UserStore::from_users([("alice".to_string(), hash_password("s3cret"))]).unwrap();
        PasswordAuthenticator::new(store)
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

    #[tokio::test]
    async fn test_password_authenticator_correct_credentials_returns_completed() {
        let auth = make_password_authenticator();
        let outcome = auth.begin(password_init("alice", "s3cret")).await;
        assert!(matches!(
            outcome,
            AuthOutcome::Completed(Identity(ref id)) if id == "alice"
        ));
    }

    #[tokio::test]
    async fn test_password_authenticator_wrong_password_returns_denied() {
        let auth = make_password_authenticator();
        let outcome = auth.begin(password_init("alice", "wrong")).await;
        assert!(
            matches!(outcome, AuthOutcome::Denied(ref e) if *e == AuthError::InvalidCredentials)
        );
    }

    #[tokio::test]
    async fn test_password_authenticator_unknown_user_returns_denied() {
        let auth = make_password_authenticator();
        let outcome = auth.begin(password_init("eve", "anything")).await;
        assert!(matches!(
            outcome,
            AuthOutcome::Denied(ref e) if *e == AuthError::InvalidCredentials
        ));
    }

    #[tokio::test]
    async fn test_password_authenticator_empty_username_returns_denied() {
        let auth = make_password_authenticator();
        let outcome = auth.begin(password_init("", "s3cret")).await;
        assert!(matches!(
            outcome,
            AuthOutcome::Denied(ref e) if *e == AuthError::InvalidCredentials
        ));
    }

    #[tokio::test]
    async fn test_password_authenticator_completed_identity_equals_username() {
        let auth = make_password_authenticator();
        let outcome = auth.begin(password_init("alice", "s3cret")).await;
        let AuthOutcome::Completed(identity) = outcome else {
            panic!("expected Completed, got {outcome:?}");
        };
        assert_eq!(identity.0, "alice");
    }
}
