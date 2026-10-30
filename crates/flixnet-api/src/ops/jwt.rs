//! Verification of signed JWTs (OIDC ID tokens) against a JSON Web Key Set, with
//! `ring`: RS256/384/512, PS256/384/512, ES256/384 and EdDSA (Ed25519).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{self, RsaPublicKeyComponents, UnparsedPublicKey};
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Debug, Clone, Deserialize)]
pub struct Jwk {
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    #[serde(rename = "use", default)]
    pub use_: Option<String>,
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub e: Option<String>,
    #[serde(default)]
    pub crv: Option<String>,
    #[serde(default)]
    pub x: Option<String>,
    #[serde(default)]
    pub y: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Jwks {
    pub keys: Vec<Jwk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwtError {
    Malformed,
    UnsupportedAlgorithm(String),
    /// No key in the set fits the token (`kid`/key type).
    UnknownKey,
    BadSignature,
}

impl std::fmt::Display for JwtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JwtError::Malformed => write!(f, "malformed token"),
            JwtError::UnsupportedAlgorithm(a) => write!(f, "unsupported algorithm {a}"),
            JwtError::UnknownKey => write!(f, "no matching signing key"),
            JwtError::BadSignature => write!(f, "invalid signature"),
        }
    }
}

fn b64(value: &str) -> Result<Vec<u8>, JwtError> {
    URL_SAFE_NO_PAD
        .decode(value.trim_end_matches('='))
        .map_err(|_| JwtError::Malformed)
}

/// The key type an algorithm needs.
fn key_type(alg: &str) -> Option<&'static str> {
    Some(match alg {
        "RS256" | "RS384" | "RS512" | "PS256" | "PS384" | "PS512" => "RSA",
        "ES256" | "ES384" => "EC",
        "EdDSA" => "OKP",
        _ => return None,
    })
}

fn verify_with(alg: &str, key: &Jwk, message: &[u8], sig: &[u8]) -> Result<(), JwtError> {
    let bad = |_| JwtError::BadSignature;
    match alg {
        "RS256" | "RS384" | "RS512" | "PS256" | "PS384" | "PS512" => {
            let n = b64(key.n.as_deref().ok_or(JwtError::UnknownKey)?)?;
            let e = b64(key.e.as_deref().ok_or(JwtError::UnknownKey)?)?;
            let params: &signature::RsaParameters = match alg {
                "RS256" => &signature::RSA_PKCS1_2048_8192_SHA256,
                "RS384" => &signature::RSA_PKCS1_2048_8192_SHA384,
                "RS512" => &signature::RSA_PKCS1_2048_8192_SHA512,
                "PS256" => &signature::RSA_PSS_2048_8192_SHA256,
                "PS384" => &signature::RSA_PSS_2048_8192_SHA384,
                _ => &signature::RSA_PSS_2048_8192_SHA512,
            };
            RsaPublicKeyComponents { n: &n, e: &e }
                .verify(params, message, sig)
                .map_err(bad)
        }
        "ES256" | "ES384" => {
            let (curve, algorithm): (&str, &'static signature::EcdsaVerificationAlgorithm) =
                if alg == "ES256" {
                    ("P-256", &signature::ECDSA_P256_SHA256_FIXED)
                } else {
                    ("P-384", &signature::ECDSA_P384_SHA384_FIXED)
                };
            if key.crv.as_deref() != Some(curve) {
                return Err(JwtError::UnknownKey);
            }
            let mut point = vec![0x04];
            point.extend(b64(key.x.as_deref().ok_or(JwtError::UnknownKey)?)?);
            point.extend(b64(key.y.as_deref().ok_or(JwtError::UnknownKey)?)?);
            UnparsedPublicKey::new(algorithm, point)
                .verify(message, sig)
                .map_err(bad)
        }
        "EdDSA" => {
            if key.crv.as_deref() != Some("Ed25519") {
                return Err(JwtError::UnknownKey);
            }
            let x = b64(key.x.as_deref().ok_or(JwtError::UnknownKey)?)?;
            UnparsedPublicKey::new(&signature::ED25519, x)
                .verify(message, sig)
                .map_err(bad)
        }
        other => Err(JwtError::UnsupportedAlgorithm(other.to_owned())),
    }
}

/// Checks the signature of `token` and returns its claims. Claims (issuer,
/// audience, expiry, nonce) are the caller's to check.
pub fn verify(token: &str, keys: &[Jwk]) -> Result<Map<String, Value>, JwtError> {
    let mut parts = token.split('.');
    let (Some(header), Some(payload), Some(sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(JwtError::Malformed);
    };
    let header: Map<String, Value> =
        serde_json::from_slice(&b64(header)?).map_err(|_| JwtError::Malformed)?;
    let alg = header
        .get("alg")
        .and_then(Value::as_str)
        .ok_or(JwtError::Malformed)?;
    // "none" and HMAC algorithms are never accepted for ID tokens here.
    let kty = key_type(alg).ok_or_else(|| JwtError::UnsupportedAlgorithm(alg.to_owned()))?;
    let kid = header.get("kid").and_then(Value::as_str);
    let signature = b64(sig)?;
    let message = format!("{}.{}", token.split('.').next().unwrap_or(""), payload);

    let candidates = keys.iter().filter(|k| {
        k.kty == kty
            && k.use_.as_deref().is_none_or(|u| u == "sig")
            && k.alg.as_deref().is_none_or(|a| a == alg)
            && kid.is_none_or(|kid| k.kid.as_deref() == Some(kid))
    });
    let mut tried = false;
    for key in candidates {
        tried = true;
        if verify_with(alg, key, message.as_bytes(), &signature).is_ok() {
            return serde_json::from_slice(&b64(payload)?).map_err(|_| JwtError::Malformed);
        }
    }
    Err(if tried {
        JwtError::BadSignature
    } else {
        JwtError::UnknownKey
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};

    fn es256() -> (EcdsaKeyPair, Jwk) {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let public = pair.public_key().as_ref();
        let jwk = Jwk {
            kty: "EC".into(),
            kid: Some("k1".into()),
            alg: Some("ES256".into()),
            use_: Some("sig".into()),
            n: None,
            e: None,
            crv: Some("P-256".into()),
            x: Some(URL_SAFE_NO_PAD.encode(&public[1..33])),
            y: Some(URL_SAFE_NO_PAD.encode(&public[33..65])),
        };
        (pair, jwk)
    }

    fn sign(pair: &EcdsaKeyPair, header: &str, claims: &str) -> String {
        let message = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header),
            URL_SAFE_NO_PAD.encode(claims)
        );
        let sig = pair.sign(&SystemRandom::new(), message.as_bytes()).unwrap();
        format!("{message}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref()))
    }

    #[test]
    fn es256_tokens_verify_and_tampering_fails() {
        let (pair, jwk) = es256();
        let token = sign(&pair, r#"{"alg":"ES256","kid":"k1"}"#, r#"{"sub":"42"}"#);
        let claims = verify(&token, std::slice::from_ref(&jwk)).unwrap();
        assert_eq!(claims["sub"], "42");

        let mut parts: Vec<&str> = token.split('.').collect();
        let forged = URL_SAFE_NO_PAD.encode(r#"{"sub":"1"}"#);
        parts[1] = &forged;
        assert_eq!(
            verify(&parts.join("."), std::slice::from_ref(&jwk)),
            Err(JwtError::BadSignature)
        );

        let (_, other) = es256();
        assert_eq!(verify(&token, &[other]), Err(JwtError::BadSignature));
        let wrong_kid = sign(&pair, r#"{"alg":"ES256","kid":"k2"}"#, "{}");
        assert_eq!(
            verify(&wrong_kid, std::slice::from_ref(&jwk)),
            Err(JwtError::UnknownKey)
        );
    }

    #[test]
    fn none_and_hmac_are_refused() {
        let (_, jwk) = es256();
        let none = format!(
            "{}.{}.",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#),
            URL_SAFE_NO_PAD.encode("{}")
        );
        assert!(matches!(
            verify(&none, std::slice::from_ref(&jwk)),
            Err(JwtError::UnsupportedAlgorithm(_))
        ));
        let hs = format!(
            "{}.{}.AAAA",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256"}"#),
            URL_SAFE_NO_PAD.encode("{}")
        );
        assert!(matches!(
            verify(&hs, &[jwk]),
            Err(JwtError::UnsupportedAlgorithm(_))
        ));
        assert_eq!(verify("a.b", &[]), Err(JwtError::Malformed));
    }
}
