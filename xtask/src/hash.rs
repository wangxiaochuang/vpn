use argon2::password_hash::SaltString;
use argon2::password_hash::rand_core::OsRng;
use argon2::{Argon2, PasswordHasher};

pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hashing cannot fail")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::PasswordHash;
    use argon2::PasswordVerifier;

    #[test]
    fn test_hash_password_produces_argon2id_phc_prefix() {
        let hash = hash_password("s3cret");
        assert!(hash.starts_with("$argon2id$"), "hash: {hash}");
    }

    #[test]
    fn test_hash_password_uses_random_salt() {
        let a = hash_password("same-password");
        let b = hash_password("same-password");
        assert_ne!(a, b);
    }

    #[test]
    fn test_hash_password_verifies_with_argon2() {
        let hash = hash_password("s3cret");
        let parsed = PasswordHash::new(&hash).unwrap();
        assert!(
            Argon2::default()
                .verify_password(b"s3cret", &parsed)
                .is_ok()
        );
    }

    #[test]
    fn test_hash_password_verifier_rejects_wrong_password() {
        let hash = hash_password("s3cret");
        let parsed = PasswordHash::new(&hash).unwrap();
        assert!(
            Argon2::default()
                .verify_password(b"wrong", &parsed)
                .is_err()
        );
    }
}
