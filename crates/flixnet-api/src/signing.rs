//! Signed, expiring URLs for media that is loaded by `<img>`/`<video>` tags, so
//! device tokens never appear in URLs, browser history or proxy logs.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Signatures are valid for at least this long.
const VALIDITY_SECS: i64 = 7 * 24 * 60 * 60;
const DAY_SECS: i64 = 24 * 60 * 60;

#[derive(Clone)]
pub struct UrlSigner {
    secret: Vec<u8>,
}

impl UrlSigner {
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self {
            secret: secret.into(),
        }
    }

    fn mac(&self, scope: &str, id: i64, expires: i64) -> HmacSha256 {
        let mut mac =
            HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts any key length");
        mac.update(format!("{scope}:{id}:{expires}").as_bytes());
        mac
    }

    /// Expiry rounded to whole days so URLs (and browser caches) stay stable for a day.
    fn expiry(now_secs: i64) -> i64 {
        (now_secs + VALIDITY_SECS) / DAY_SECS * DAY_SECS + DAY_SECS
    }

    pub fn sign(&self, scope: &str, id: i64) -> (i64, String) {
        let expires = Self::expiry(flixnet_db::now_ms() / 1000);
        let tag = self.mac(scope, id, expires).finalize().into_bytes();
        (expires, hex::encode(&tag[..16]))
    }

    pub fn verify(&self, scope: &str, id: i64, expires: i64, signature: &str) -> bool {
        if expires < flixnet_db::now_ms() / 1000 {
            return false;
        }
        let Ok(bytes) = hex::decode(signature) else {
            return false;
        };
        // Constant-time comparison of the truncated tag.
        self.mac(scope, id, expires)
            .verify_truncated_left(&bytes)
            .is_ok()
            && bytes.len() == 16
    }

    /// `/api/v1/images/{id}?w=…&exp=…&sig=…`
    pub fn image_url(&self, image_id: i64, width: Option<u32>) -> String {
        let (expires, sig) = self.sign("img", image_id);
        match width {
            Some(w) => format!("/api/v1/images/{image_id}?w={w}&exp={expires}&sig={sig}"),
            None => format!("/api/v1/images/{image_id}?exp={expires}&sig={sig}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_verify_and_reject_tampering() {
        let signer = UrlSigner::new(b"secret".to_vec());
        let (expires, sig) = signer.sign("img", 42);
        assert!(signer.verify("img", 42, expires, &sig));
        assert!(!signer.verify("img", 43, expires, &sig), "other id");
        assert!(!signer.verify("img", 42, expires + 1, &sig), "other expiry");
        assert!(!signer.verify("vid", 42, expires, &sig), "other scope");
        assert!(!signer.verify("img", 42, 1, &sig), "expired");
        assert!(!signer.verify("img", 42, expires, "zz"), "garbage");
        assert!(!UrlSigner::new(b"other".to_vec()).verify("img", 42, expires, &sig));
    }

    #[test]
    fn urls_are_stable_within_a_day() {
        let signer = UrlSigner::new(b"secret".to_vec());
        assert_eq!(
            signer.image_url(1, Some(342)),
            signer.image_url(1, Some(342))
        );
    }
}
