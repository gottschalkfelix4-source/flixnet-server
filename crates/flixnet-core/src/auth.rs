//! Password hashing and access-token primitives.
//!
//! Password hashing is deliberately slow (argon2id); call these from
//! `tokio::task::spawn_blocking`, never directly on the async runtime.

use std::sync::OnceLock;

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use sha2::{Digest, Sha256};

/// Minimum and maximum accepted password length (characters).
pub const PASSWORD_MIN_LEN: usize = 8;
pub const PASSWORD_MAX_LEN: usize = 1024;

/// Hashes a password into an argon2id PHC string.
pub fn hash_password(password: &str) -> String {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).expect("OS random number generator unavailable");
    let hash: PasswordHash = Argon2::default()
        .hash_password_with_salt(password.as_bytes(), &salt)
        .expect("argon2 hashing with default params cannot fail");
    hash.to_string()
}

/// Checks a password against a stored PHC string. Malformed hashes never verify.
pub fn verify_password(password: &str, phc: &str) -> bool {
    Argon2::default()
        .verify_password(password.as_bytes(), phc)
        .is_ok()
}

/// Burns the same time as a real verification. Used when the user does not exist,
/// so response timing does not reveal which account names are valid.
pub fn verify_dummy(password: &str) {
    static DUMMY: OnceLock<String> = OnceLock::new();
    let phc = DUMMY.get_or_init(|| hash_password("flixnet-dummy-password"));
    let _ = verify_password(password, phc);
}

/// A freshly generated access token and the hash that is stored in the database.
pub struct NewToken {
    pub token: String,
    pub hash: String,
}

/// 256 random bits, hex encoded (64 chars).
pub fn new_token() -> NewToken {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS random number generator unavailable");
    let token = hex::encode(bytes);
    let hash = hash_token(&token);
    NewToken { token, hash }
}

/// SHA-256 of a token, hex encoded. Tokens are high-entropy, so a fast hash is enough.
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let phc = hash_password("korrekt pferd batterie");
        assert!(phc.starts_with("$argon2id$"));
        assert!(verify_password("korrekt pferd batterie", &phc));
        assert!(!verify_password("falsch", &phc));
    }

    #[test]
    fn same_password_gets_different_salts() {
        assert_ne!(hash_password("geheim123"), hash_password("geheim123"));
    }

    #[test]
    fn malformed_hash_never_verifies() {
        assert!(!verify_password("x", "not-a-phc-string"));
        assert!(!verify_password("x", ""));
    }

    #[test]
    fn tokens_are_random_and_hash_matches() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.token.len(), 64);
        assert_ne!(a.token, b.token);
        assert_eq!(a.hash, hash_token(&a.token));
        assert_ne!(a.hash, a.token);
    }
}
