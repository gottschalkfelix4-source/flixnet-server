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

    /// A non-expiring tag for `scope` and `id` (32 hex characters). Used where clients
    /// cache URLs indefinitely, e.g. Jellyfin image tags; a tag only unlocks that one
    /// resource, and rotating the secret revokes all of them.
    pub fn tag(&self, scope: &str, id: i64) -> String {
        let tag = self.mac(scope, id, 0).finalize().into_bytes();
        hex::encode(&tag[..16])
    }

    pub fn verify_tag(&self, scope: &str, id: i64, tag: &str) -> bool {
        let Ok(bytes) = hex::decode(tag) else {
            return false;
        };
        bytes.len() == 16 && self.mac(scope, id, 0).verify_truncated_left(&bytes).is_ok()
    }

    /// `/api/v1/stream/{file_id}?u=…&exp=…&sig=…`, bound to the user it was issued to.
    pub fn stream_url(&self, file_id: i64, user_id: i64) -> String {
        let (expires, sig) = self.sign(&format!("stream:{user_id}"), file_id);
        format!("/api/v1/stream/{file_id}?u={user_id}&exp={expires}&sig={sig}")
    }

    pub fn verify_stream(&self, file_id: i64, user_id: i64, expires: i64, signature: &str) -> bool {
        self.verify(&format!("stream:{user_id}"), file_id, expires, signature)
    }

    /// `/api/v1/music/stream/{file_id}?u=…&exp=…&sig=…` (AAC conversion of a track),
    /// bound to the user like stream URLs.
    pub fn audio_url(&self, file_id: i64, user_id: i64) -> String {
        let (expires, sig) = self.sign(&format!("audio:{user_id}"), file_id);
        format!("/api/v1/music/stream/{file_id}?u={user_id}&exp={expires}&sig={sig}")
    }

    pub fn verify_audio(&self, file_id: i64, user_id: i64, expires: i64, signature: &str) -> bool {
        self.verify(&format!("audio:{user_id}"), file_id, expires, signature)
    }

    /// `/api/v1/subtitles/{file_id}/{stream_index}.vtt?u=…&exp=…&sig=…`, bound to the
    /// user and the file like stream URLs (`<track>` elements cannot send headers).
    pub fn subtitle_url(&self, file_id: i64, stream_index: i64, user_id: i64) -> String {
        let (expires, sig) = self.sign(&format!("subtitle:{user_id}"), file_id);
        format!(
            "/api/v1/subtitles/{file_id}/{stream_index}.vtt?u={user_id}&exp={expires}&sig={sig}"
        )
    }

    pub fn verify_subtitle(
        &self,
        file_id: i64,
        user_id: i64,
        expires: i64,
        signature: &str,
    ) -> bool {
        self.verify(&format!("subtitle:{user_id}"), file_id, expires, signature)
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
    fn subtitle_urls_are_bound_to_user_and_file() {
        let signer = UrlSigner::new(b"secret".to_vec());
        let url = signer.subtitle_url(7, 1000, 3);
        let query = |name: &str| {
            url.split(['?', '&'])
                .find_map(|p| p.strip_prefix(&format!("{name}=")))
                .unwrap()
                .to_owned()
        };
        assert!(url.starts_with("/api/v1/subtitles/7/1000.vtt?u=3&"));
        let (exp, sig) = (query("exp").parse().unwrap(), query("sig"));
        assert!(signer.verify_subtitle(7, 3, exp, &sig));
        assert!(!signer.verify_subtitle(7, 4, exp, &sig), "other user");
        assert!(!signer.verify_subtitle(8, 3, exp, &sig), "other file");
        assert!(
            !signer.verify_stream(7, 3, exp, &sig),
            "not a stream signature"
        );
    }

    #[test]
    fn tags_are_stable_and_scoped() {
        let signer = UrlSigner::new(b"secret".to_vec());
        let tag = signer.tag("jfimg", 5);
        assert_eq!(tag.len(), 32);
        assert_eq!(tag, signer.tag("jfimg", 5));
        assert!(signer.verify_tag("jfimg", 5, &tag));
        assert!(!signer.verify_tag("jfimg", 6, &tag), "other id");
        assert!(!signer.verify_tag("img", 5, &tag), "other scope");
        assert!(!signer.verify_tag("jfimg", 5, &tag[..30]), "truncated");
        assert!(!UrlSigner::new(b"other".to_vec()).verify_tag("jfimg", 5, &tag));
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
