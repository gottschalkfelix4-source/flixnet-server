//! Single sign-on against a mocked OpenID Connect provider: discovery, authorization
//! code + PKCE, ID tokens signed with locally generated keys (ES256 and RS256), user
//! mapping by `sub`, auto-creation, admin group, linking by name and the failure
//! cases (replayed state, wrong nonce, foreign key, no account).

mod common;

use axum::http::{Method, StatusCode};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use common::{TestApp, app};
use ring::rand::SystemRandom;
use ring::signature::{self, EcdsaKeyPair, KeyPair, RsaKeyPair};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 2048-bit RSA test key (PKCS#8), generated with OpenSSL for these tests only.
const RSA_PKCS8: &str = concat!(
    "MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQDBaeZ7HQW7SR7yo9hOPMhJ1JATC1t2HMyNHW5E0FmS",
    "geNAJluJHoPHN0kSQPCE49wMy6JfeAqO/taQSdpsaeytJvMizswkFM8vHgB5wJUwapErg2EG2kBAJ43Vg3b+mjABCRNW",
    "+hkJHN7zM7SjE3eEnMHTdTkT1J0AlKMi0t1iaDZ686EoP1Mmge9GiOEJWNGtVIsDEHRhfEaJ8s8U/EkC1dhNkfyfBEC6",
    "UPfYD/EXOgCOlnB4M4E4r7Dpk/Y70FGqW+HksDthlj+1jHY8zsof6c3FGfzaWHP6DKraOf7osv8Yq36yxjrNlRfCIKPE",
    "L8bAEVz0ZU6uJisWT25zNMWLAgMBAAECggEAI5Ls13wRD891J7/HCyzW0e2MylPucJz/an0dSP7hWrcVKzli4KeCKWQb",
    "xoW3N1oJoTjbFqAnoQFySXur9wDMjVR6p85Ncd159fqYr8wiJJOVO+GgOPhvlN0xkzHo8w89ARPAQOEjNa/fue+qq8vq",
    "0bNbWx9WbjVO40pzbvZFzdF22Y046TrSCD2f+uBwKBWe2TuiKpiitKTO08HZaEYZkqPg/+wH9qyY7y4Nd2WjXf9nnWiq",
    "kp8OBCe602lKQxT0X+mEklLwHoYfCE2DgvSnItVOKVHZLJX/u3HdDlFvVRqVwiyjqeHfV0nXmMLXU6lADwNKc7tei5NL",
    "+JW6Y8s1YQKBgQD2rDB+y3AcfFTjaIlsKKpbhv8wKImzBlixaKur6+eymqIISTY1NC5LlNSjTpTnO3E1qHLwCN5jmyuE",
    "aaFLRv/K/aiDCaz6Xxh1P0nRbUpUQQaOyjVJ2p/U0tXT1GwOHoCnj6o+OS91OiPRkJ7KsVixBnFqWQtGzo3lw/zZKoCv",
    "MwKBgQDIuij7ECWU+vjBGX9JM6QT4r6fO/2d3J1fARErL4wTDpc2g8NhFCx9EqXmaODg63vgq/D0yl2OBbbTiCSrHkmM",
    "5m1cT3LT5srO9JIpc6OtkSXpVguuZv8qsnUkEcWqT73QMSg0khtweQpAkuTxBOVc0tOb+j7s7IK+zdkKOrHwSQKBgCXy",
    "g1teLJcVQXHiiRGMh/igoe1krxpNZaOVOKOwHOrLAxZHPQGeT2jtMnEVC68u49GfYl8u3hdwrZcnMNsFxyARObU+7c9u",
    "hbKz08VWDNM9Vabt0JjV+zSgYqi9ywm5A43DeissXPKpyQzaNLN16PECzrvPiv6PlCC+r80wzq8lAoGAbizgOY4PUsq+",
    "px7bos5ZjSC0CKrg9jkVmAP57H5YoE0t6ppAL5TnJm+J6AJBDLO0dwlpZLN+zmmMhGcB2XryXAhOlpB1MqJHR3kukNoM",
    "SUSWTpEQMFYQUNtLONUYbfb+9LtGmjRoI0AMxIzcZP6O9ngx9jdROrJGwXwzWU4GRSECgYAC3ZPxGwyX9gPuaOZDLTae",
    "6ig5P/hpbnwGcFCFtJjiidXdbez2jnsBt7xtp56dcc+KSQyTDEqQvFYo7JQurw7Qbd+8BoTKQTKc+TQBa/OyxlGZbX6M",
    "HHCbS7A9cRx8pQ5Xu92GS9sKuB70+euFnLVKOiqXwUbnvH0HssHTDIwHmw==",
);

struct Provider {
    server: MockServer,
    ec: EcdsaKeyPair,
    rsa: RsaKeyPair,
}

#[derive(Clone, Copy)]
enum Key {
    Ec,
    Rsa,
    /// A key the provider does not publish.
    Foreign,
}

impl Provider {
    async fn start() -> Self {
        Self::start_with(json!({})).await
    }

    /// With extra discovery fields (e.g. `token_endpoint_auth_methods_supported`).
    async fn start_with(discovery: Value) -> Self {
        let server = MockServer::start().await;
        let rng = SystemRandom::new();
        let alg = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
        let ec = EcdsaKeyPair::from_pkcs8(alg, pkcs8.as_ref(), &rng).unwrap();
        let rsa = RsaKeyPair::from_pkcs8(&STANDARD.decode(RSA_PKCS8).unwrap()).unwrap();

        let point = ec.public_key().as_ref();
        let public: signature::RsaPublicKeyComponents<Vec<u8>> = rsa.public().into();
        let jwks = json!({ "keys": [
            { "kty": "EC", "kid": "ec-1", "use": "sig", "alg": "ES256", "crv": "P-256",
              "x": URL_SAFE_NO_PAD.encode(&point[1..33]), "y": URL_SAFE_NO_PAD.encode(&point[33..65]) },
            { "kty": "RSA", "kid": "rsa-1", "use": "sig", "alg": "RS256",
              "n": URL_SAFE_NO_PAD.encode(&public.n),
              "e": URL_SAFE_NO_PAD.encode(&public.e) }
        ]});
        let uri = server.uri();
        let mut config = json!({
            "issuer": uri,
            "authorization_endpoint": format!("{uri}/authorize"),
            "token_endpoint": format!("{uri}/token"),
            "jwks_uri": format!("{uri}/jwks"),
            "userinfo_endpoint": format!("{uri}/userinfo"),
            "response_types_supported": ["code"],
            "id_token_signing_alg_values_supported": ["RS256", "ES256"],
            "code_challenge_methods_supported": ["S256"]
        });
        config
            .as_object_mut()
            .unwrap()
            .extend(discovery.as_object().unwrap().clone());
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(config))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks))
            .mount(&server)
            .await;
        Self { server, ec, rsa }
    }

    fn issuer(&self) -> String {
        self.server.uri()
    }

    fn id_token(&self, key: Key, claims: &Value) -> String {
        let rng = SystemRandom::new();
        let header = match key {
            Key::Ec | Key::Foreign => json!({ "alg": "ES256", "typ": "JWT", "kid": "ec-1" }),
            Key::Rsa => json!({ "alg": "RS256", "typ": "JWT", "kid": "rsa-1" }),
        };
        let message = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let sig = match key {
            Key::Ec => self
                .ec
                .sign(&rng, message.as_bytes())
                .unwrap()
                .as_ref()
                .to_vec(),
            Key::Foreign => {
                let alg = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
                let pkcs8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
                let other = EcdsaKeyPair::from_pkcs8(alg, pkcs8.as_ref(), &rng).unwrap();
                other
                    .sign(&rng, message.as_bytes())
                    .unwrap()
                    .as_ref()
                    .to_vec()
            }
            Key::Rsa => {
                let mut sig = vec![0; self.rsa.public().modulus_len()];
                self.rsa
                    .sign(
                        &signature::RSA_PKCS1_SHA256,
                        &rng,
                        message.as_bytes(),
                        &mut sig,
                    )
                    .unwrap();
                sig
            }
        };
        format!("{message}.{}", URL_SAFE_NO_PAD.encode(sig))
    }

    /// Answers the token request for `code` with an ID token.
    async fn token_for(&self, code: &str, id_token: String) {
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains(format!("code={code}&")))
            .and(header(
                "authorization",
                format!("Basic {}", STANDARD.encode("flixnet:geheim")).as_str(),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": format!("at-{code}"), "token_type": "Bearer", "expires_in": 300,
                "id_token": id_token
            })))
            .mount(&self.server)
            .await;
    }
}

fn claims(issuer: &str, nonce: &str, sub: &str, extra: Value) -> Value {
    let now = flixnet_db::now_ms() / 1000;
    let mut c = json!({
        "iss": issuer, "aud": "flixnet", "exp": now + 300, "iat": now, "nonce": nonce, "sub": sub
    });
    c.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    c
}

struct Started {
    state: String,
    nonce: String,
    challenge: String,
}

async fn start(app: &TestApp) -> Started {
    let response = app
        .post(
            "/api/v1/auth/oidc/start",
            json!({
                "redirect_uri": "http://localhost:8097/login/oidc",
                "device": { "id": "browser-1", "name": "Firefox · Linux", "client": "Flixnet Web" }
            }),
            None,
        )
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.json);
    let url = reqwest::Url::parse(response.json["authorization_url"].as_str().unwrap()).unwrap();
    assert_eq!(url.path(), "/authorize");
    let query: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["client_id"], "flixnet");
    assert_eq!(query["redirect_uri"], "http://localhost:8097/login/oidc");
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(query["scope"].split(' ').any(|s| s == "openid"));
    Started {
        state: query["state"].clone(),
        nonce: query["nonce"].clone(),
        challenge: query["code_challenge"].clone(),
    }
}

async fn complete(app: &TestApp, code: &str, state: &str) -> common::Response {
    app.post(
        "/api/v1/auth/oidc/complete",
        json!({ "code": code, "state": state }),
        None,
    )
    .await
}

async fn configure(app: &TestApp, admin: &str, provider: &Provider, extra: Value) {
    let mut body = json!({
        "enabled": true, "provider_name": "Authentik", "issuer_url": provider.issuer(),
        "client_id": "flixnet", "client_secret": "geheim", "auto_create_users": true,
        "admin_group": "flixnet-admins"
    });
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let response = app
        .request(
            Method::PUT,
            "/api/v1/settings/oidc",
            Some(body),
            Some(admin),
        )
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.json);
}

#[tokio::test]
async fn sign_in_creates_and_reuses_the_user() {
    let provider = Provider::start().await;
    let app = app().await;
    let admin = app.setup_admin().await;

    let off = app.get("/api/v1/auth/oidc/config", None).await;
    assert_eq!(off.json["enabled"], false);

    configure(&app, &admin, &provider, json!({})).await;
    let settings = app.get("/api/v1/settings/oidc", Some(&admin)).await;
    assert_eq!(settings.json["has_client_secret"], true);
    assert!(!settings.json.to_string().contains("geheim"));
    let config = app.get("/api/v1/auth/oidc/config", None).await;
    assert_eq!(
        config.json,
        json!({ "enabled": true, "provider_name": "Authentik" })
    );

    // First sign-in (ES256): the groups only come from the userinfo endpoint.
    let started = start(&app).await;
    provider
        .token_for(
            "code-1",
            provider.id_token(
                Key::Ec,
                &claims(
                    &provider.issuer(),
                    &started.nonce,
                    "sub-anna",
                    json!({ "preferred_username": "anna", "email": "anna@example.org", "email_verified": true }),
                ),
            ),
        )
        .await;
    Mock::given(method("GET"))
        .and(path("/userinfo"))
        .and(header("authorization", "Bearer at-code-1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "sub": "sub-anna", "groups": ["flixnet-admins"] })),
        )
        .mount(&provider.server)
        .await;
    let session = complete(&app, "code-1", &started.state).await;
    assert_eq!(session.status, StatusCode::OK, "{}", session.json);
    assert_eq!(session.json["user"]["name"], "anna");
    assert_eq!(
        session.json["user"]["is_admin"], true,
        "member of the admin group"
    );
    assert_eq!(session.json["user"]["has_password"], false);
    let token = session.json["token"].as_str().unwrap();
    let me = app.get("/api/v1/users/me", Some(token)).await;
    assert_eq!(me.json["name"], "anna");
    let anna = me.json["id"].as_i64().unwrap();

    // The token request carried the PKCE verifier matching the challenge.
    let requests = provider.server.received_requests().await.unwrap();
    let token_request = requests.iter().find(|r| r.url.path() == "/token").unwrap();
    let form: std::collections::HashMap<String, String> = reqwest::Url::parse(&format!(
        "http://x/?{}",
        String::from_utf8_lossy(&token_request.body)
    ))
    .unwrap()
    .query_pairs()
    .into_owned()
    .collect();
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["redirect_uri"], "http://localhost:8097/login/oidc");
    assert_eq!(
        URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())),
        started.challenge
    );

    // The state is single-use.
    let replay = complete(&app, "code-1", &started.state).await;
    assert_eq!(replay.status, StatusCode::UNAUTHORIZED);
    assert_eq!(replay.json["code"], "oidc_expired");

    // Second sign-in (RS256), another name at the provider: same account by `sub`.
    let started = start(&app).await;
    provider
        .token_for(
            "code-2",
            provider.id_token(
                Key::Rsa,
                &claims(
                    &provider.issuer(),
                    &started.nonce,
                    "sub-anna",
                    json!({ "preferred_username": "anna.neu" }),
                ),
            ),
        )
        .await;
    let again = complete(&app, "code-2", &started.state).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.json);
    assert_eq!(again.json["user"]["id"], anna);
    let users = app.get("/api/v1/users", Some(&admin)).await;
    assert_eq!(
        users.json.as_array().unwrap().len(),
        2,
        "no duplicate account"
    );
}

#[tokio::test]
async fn invalid_tokens_and_missing_accounts_are_rejected() {
    let provider = Provider::start().await;
    let app = app().await;
    let admin = app.setup_admin().await;
    configure(
        &app,
        &admin,
        &provider,
        json!({ "auto_create_users": false }),
    )
    .await;

    let issuer = provider.issuer();
    for (n, (key, nonce_ok, aud, expected)) in [
        (Key::Ec, false, "flixnet", "oidc_invalid_token"),
        (Key::Foreign, true, "flixnet", "oidc_invalid_token"),
        (Key::Rsa, true, "someone-else", "oidc_invalid_token"),
    ]
    .into_iter()
    .enumerate()
    {
        let started = start(&app).await;
        let nonce = if nonce_ok {
            started.nonce.clone()
        } else {
            "falsch".to_owned()
        };
        let mut c = claims(&issuer, &nonce, "sub-x", json!({}));
        c["aud"] = json!(aud);
        let code = format!("bad-{n}");
        provider.token_for(&code, provider.id_token(key, &c)).await;
        let response = complete(&app, &code, &started.state).await;
        assert_eq!(
            response.status,
            StatusCode::UNAUTHORIZED,
            "case {n}: {}",
            response.json
        );
        assert_eq!(response.json["code"], expected, "case {n}");
    }

    // A valid token for an unknown account without auto-creation.
    let started = start(&app).await;
    provider
        .token_for(
            "unknown",
            provider.id_token(
                Key::Ec,
                &claims(
                    &issuer,
                    &started.nonce,
                    "sub-felix",
                    json!({ "preferred_username": "Felix" }),
                ),
            ),
        )
        .await;
    let response = complete(&app, "unknown", &started.state).await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
    assert_eq!(response.json["code"], "oidc_no_account");

    // Linking by name maps it to the existing "Felix".
    configure(
        &app,
        &admin,
        &provider,
        json!({ "auto_create_users": false, "link_by_name": true }),
    )
    .await;
    let started = start(&app).await;
    provider
        .token_for(
            "linked",
            provider.id_token(
                Key::Ec,
                &claims(
                    &issuer,
                    &started.nonce,
                    "sub-felix",
                    json!({ "preferred_username": "Felix" }),
                ),
            ),
        )
        .await;
    let response = complete(&app, "linked", &started.state).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.json);
    assert_eq!(response.json["user"]["name"], "Felix");
    assert_eq!(response.json["user"]["is_admin"], true);

    // Bad redirect URIs are refused; disabling hides the button and the flow.
    let bad = app
        .post(
            "/api/v1/auth/oidc/start",
            json!({ "redirect_uri": "https://evil.example/steal", "device": { "id": "d", "name": "n", "client": "c" } }),
            None,
        )
        .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    configure(&app, &admin, &provider, json!({ "enabled": false })).await;
    assert_eq!(
        app.get("/api/v1/auth/oidc/config", None).await.json["enabled"],
        false
    );
    let off = app
        .post(
            "/api/v1/auth/oidc/start",
            json!({ "redirect_uri": "http://localhost/login/oidc", "device": { "id": "d", "name": "n", "client": "c" } }),
            None,
        )
        .await;
    assert_eq!(off.status, StatusCode::NOT_FOUND);

    // Enabling against an unreachable issuer is refused.
    let unreachable = app
        .request(
            Method::PUT,
            "/api/v1/settings/oidc",
            Some(json!({ "enabled": true, "issuer_url": "http://127.0.0.1:9" })),
            Some(&admin),
        )
        .await;
    assert_eq!(unreachable.status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// Claims and client authentication as a real Dex v2.46 sends and expects them: an
/// opaque base64 `sub`, groups in the ID token (scope `groups`), no
/// `preferred_username` for static users without one, and a client secret with
/// `+`, `/` and `=` (as `openssl rand -base64` makes). Dex url-decodes Basic
/// credentials and ignores a body secret when Basic is present, so the old request
/// (raw secret as Basic *and* in the body) failed with `invalid_client`.
#[tokio::test]
async fn dex_shaped_provider_with_a_base64_secret() {
    const SECRET: &str = "fx+W0XKEUU/x=";
    const ENCODED: &str = "fx%2BW0XKEUU%2Fx%3D";
    let app = app().await;
    let admin = app.setup_admin().await;
    for (n, methods) in [
        json!(["client_secret_basic", "client_secret_post"]),
        json!(["client_secret_basic"]),
    ]
    .into_iter()
    .enumerate()
    {
        let provider = Provider::start_with(json!({
            "token_endpoint_auth_methods_supported": methods,
            "scopes_supported": ["openid", "email", "groups", "profile", "offline_access"]
        }))
        .await;
        configure(
            &app,
            &admin,
            &provider,
            json!({ "client_secret": SECRET, "scopes": "openid profile email groups" }),
        )
        .await;
        let started = start(&app).await;
        let sub = "CiQ0MTMzMTMyMy02ZjQ0LTQ1ZTYtYjNiOS0yYzRiNjBjMDJiZTUSBWxvY2Fs";
        let id_token = provider.id_token(
            Key::Rsa,
            &claims(
                &provider.issuer(),
                &started.nonce,
                sub,
                json!({
                    "at_hash": "x", "email": "bernd@example.org", "email_verified": true,
                    "groups": ["flixnet-admins", "family"], "name": "Bernd Benutzer"
                }),
            ),
        );
        // Both methods offered: the body (no encoding ambiguity); Basic only: encoded.
        let token_mock = Mock::given(method("POST")).and(path("/token"));
        let token_mock = if n == 0 {
            token_mock.and(body_string_contains(format!("client_secret={ENCODED}")))
        } else {
            token_mock.and(header(
                "authorization",
                format!("Basic {}", STANDARD.encode(format!("flixnet:{ENCODED}"))).as_str(),
            ))
        };
        token_mock
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at", "token_type": "bearer", "expires_in": 86399,
                "id_token": id_token
            })))
            .mount(&provider.server)
            .await;
        let session = complete(&app, "dex-code", &started.state).await;
        assert_eq!(session.status, StatusCode::OK, "case {n}: {}", session.json);
        assert_eq!(session.json["user"]["name"], "Bernd Benutzer");
        assert_eq!(session.json["user"]["is_admin"], true);

        let requests = provider.server.received_requests().await.unwrap();
        let token = requests.iter().find(|r| r.url.path() == "/token").unwrap();
        let body = String::from_utf8_lossy(&token.body);
        if n == 0 {
            assert!(
                !token.headers.contains_key("authorization"),
                "one authentication method only"
            );
        } else {
            assert!(
                !body.contains("client_secret="),
                "one authentication method only: {body}"
            );
        }
    }
}
