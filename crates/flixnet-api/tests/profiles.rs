mod common;

use axum::http::{Method, StatusCode};
use common::{PASSWORD, TestApp, app, credentials};
use serde_json::{Value, json};

fn device(id: &str) -> Value {
    json!({ "id": id, "name": "Wohnzimmer-TV", "client": "Flixnet Tests" })
}

async fn create_user(app: &TestApp, admin: &str, body: Value) -> Value {
    let response = app.post("/api/v1/users", body, Some(admin)).await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.json);
    response.json
}

async fn login(app: &TestApp, name: &str, password: &str, device_id: &str) -> String {
    let response = app
        .post(
            "/api/v1/auth/login",
            credentials(name, password, device_id),
            None,
        )
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.json);
    response.json["token"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn admin_manages_users_and_members_cannot() {
    let app = app().await;
    let admin = app.setup_admin().await;

    let anna = create_user(
        &app,
        &admin,
        json!({ "name": "Anna", "password": PASSWORD, "max_age_rating": 16 }),
    )
    .await;
    assert_eq!(anna["is_admin"], false);
    assert_eq!(anna["has_password"], true);
    assert_eq!(anna["max_age_rating"], 16);

    let taken = app
        .post(
            "/api/v1/users",
            json!({ "name": "anna", "password": PASSWORD }),
            Some(&admin),
        )
        .await;
    assert_eq!(taken.status, StatusCode::CONFLICT);
    assert_eq!(taken.json["code"], "name_taken");

    let admin_without_password = app
        .post(
            "/api/v1/users",
            json!({ "name": "Root", "is_admin": true }),
            Some(&admin),
        )
        .await;
    assert_eq!(
        admin_without_password.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    // Members get 403 on admin endpoints.
    let anna_token = login(&app, "Anna", PASSWORD, "anna-phone").await;
    assert_eq!(
        app.get("/api/v1/users", Some(&anna_token)).await.status,
        StatusCode::FORBIDDEN
    );
    let forbidden = app
        .post(
            "/api/v1/users",
            json!({ "name": "X", "password": PASSWORD }),
            Some(&anna_token),
        )
        .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);

    // Update: clear the age limit, rename.
    let id = anna["id"].as_i64().unwrap();
    let updated = app
        .request(
            Method::PATCH,
            &format!("/api/v1/users/{id}"),
            Some(json!({ "name": "Anna B.", "max_age_rating": null })),
            Some(&admin),
        )
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.json);
    assert_eq!(updated.json["name"], "Anna B.");
    assert_eq!(updated.json["max_age_rating"], Value::Null);

    // An absent field is left alone (not cleared).
    let untouched = app
        .request(
            Method::PATCH,
            &format!("/api/v1/users/{id}"),
            Some(json!({ "is_hidden": true })),
            Some(&admin),
        )
        .await;
    assert_eq!(untouched.json["name"], "Anna B.");
    assert_eq!(untouched.json["is_hidden"], true);

    let list = app.get("/api/v1/users", Some(&admin)).await;
    assert_eq!(list.json.as_array().unwrap().len(), 2);

    let deleted = app
        .request(
            Method::DELETE,
            &format!("/api/v1/users/{id}"),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get("/api/v1/users/me", Some(&anna_token)).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn the_last_admin_is_protected() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let me = app.get("/api/v1/users/me", Some(&admin)).await;
    let id = me.json["id"].as_i64().unwrap();

    let demote = app
        .request(
            Method::PATCH,
            &format!("/api/v1/users/{id}"),
            Some(json!({ "is_admin": false })),
            Some(&admin),
        )
        .await;
    assert_eq!(demote.status, StatusCode::CONFLICT);
    assert_eq!(demote.json["code"], "last_admin");

    let delete_self = app
        .request(
            Method::DELETE,
            &format!("/api/v1/users/{id}"),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(delete_self.status, StatusCode::CONFLICT);
    assert_eq!(delete_self.json["code"], "cannot_delete_self");
}

#[tokio::test]
async fn profile_switching_respects_pins_and_admin_passwords() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let kid = create_user(&app, &admin, json!({ "name": "Kind", "max_age_rating": 6 })).await;
    let teen = create_user(&app, &admin, json!({ "name": "Teen", "pin": "1234" })).await;
    create_user(
        &app,
        &admin,
        json!({ "name": "Gast", "password": PASSWORD, "is_hidden": true }),
    )
    .await;

    let profiles = app.get("/api/v1/auth/profiles", Some(&admin)).await;
    let names: Vec<&str> = profiles
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["Felix", "Kind", "Teen"],
        "hidden profiles are not listed"
    );
    let unlocks: Vec<&str> = profiles
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["unlock"].as_str().unwrap())
        .collect();
    assert_eq!(unlocks, ["password", "none", "pin"]);

    // Kid profile: no PIN needed. The old token is revoked.
    let switched = app
        .post(
            "/api/v1/auth/switch",
            json!({ "user_id": kid["id"], "device": device("tv") }),
            Some(&admin),
        )
        .await;
    assert_eq!(switched.status, StatusCode::OK, "{}", switched.json);
    assert_eq!(switched.json["user"]["name"], "Kind");
    let kid_token = switched.json["token"].as_str().unwrap().to_owned();
    assert_eq!(
        app.get("/api/v1/users/me", Some(&admin)).await.status,
        StatusCode::UNAUTHORIZED
    );

    // Teen needs the PIN.
    let no_pin = app
        .post(
            "/api/v1/auth/switch",
            json!({ "user_id": teen["id"], "device": device("tv") }),
            Some(&kid_token),
        )
        .await;
    assert_eq!(no_pin.status, StatusCode::FORBIDDEN);
    let wrong_pin = app
        .post(
            "/api/v1/auth/switch",
            json!({ "user_id": teen["id"], "pin": "0000", "device": device("tv") }),
            Some(&kid_token),
        )
        .await;
    assert_eq!(wrong_pin.status, StatusCode::FORBIDDEN);
    let teen_session = app
        .post(
            "/api/v1/auth/switch",
            json!({ "user_id": teen["id"], "pin": "1234", "device": device("tv") }),
            Some(&kid_token),
        )
        .await;
    assert_eq!(teen_session.status, StatusCode::OK);
    let teen_token = teen_session.json["token"].as_str().unwrap().to_owned();

    // Back to the admin requires the admin password.
    let admin_id = profiles.json[0]["id"].clone();
    let no_password = app
        .post(
            "/api/v1/auth/switch",
            json!({ "user_id": admin_id, "device": device("tv") }),
            Some(&teen_token),
        )
        .await;
    assert_eq!(no_password.status, StatusCode::FORBIDDEN);
    let back = app
        .post(
            "/api/v1/auth/switch",
            json!({ "user_id": admin_id, "password": PASSWORD, "device": device("tv") }),
            Some(&teen_token),
        )
        .await;
    assert_eq!(back.status, StatusCode::OK);
    assert_eq!(back.json["user"]["is_admin"], true);
}

#[tokio::test]
async fn pin_guessing_is_rate_limited() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let teen = create_user(&app, &admin, json!({ "name": "Teen", "pin": "4711" })).await;
    let kid = create_user(&app, &admin, json!({ "name": "Kind" })).await;
    let kid_token = app
        .post(
            "/api/v1/auth/switch",
            json!({ "user_id": kid["id"], "device": device("tv") }),
            Some(&admin),
        )
        .await
        .json["token"]
        .as_str()
        .unwrap()
        .to_owned();

    for guess in ["0000", "1111", "2222", "3333", "4444"] {
        let r = app
            .post(
                "/api/v1/auth/switch",
                json!({ "user_id": teen["id"], "pin": guess, "device": device("tv") }),
                Some(&kid_token),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN);
    }
    // Even the right PIN is refused while locked.
    let locked = app
        .post(
            "/api/v1/auth/switch",
            json!({ "user_id": teen["id"], "pin": "4711", "device": device("tv") }),
            Some(&kid_token),
        )
        .await;
    assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(locked.json["code"], "too_many_attempts");
}

#[tokio::test]
async fn password_guessing_is_rate_limited() {
    let app = app().await;
    app.setup_admin().await;
    for _ in 0..10 {
        let r = app
            .post(
                "/api/v1/auth/login",
                credentials("Felix", "falsch-falsch", "d"),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let locked = app
        .post(
            "/api/v1/auth/login",
            credentials("felix", PASSWORD, "d"),
            None,
        )
        .await;
    assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn own_password_pin_and_devices() {
    let app = app().await;
    let admin = app.setup_admin().await;
    let phone = login(&app, "Felix", PASSWORD, "phone").await;

    let devices = app.get("/api/v1/users/me/devices", Some(&admin)).await;
    let list = devices.json.as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list.iter().filter(|d| d["is_current"] == true).count(), 1);

    let wrong = app
        .request(
            Method::PUT,
            "/api/v1/users/me/password",
            Some(json!({ "current_password": "nope-nope", "new_password": "neues-passwort" })),
            Some(&admin),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);

    let changed = app
        .request(
            Method::PUT,
            "/api/v1/users/me/password",
            Some(json!({ "current_password": PASSWORD, "new_password": "neues-passwort" })),
            Some(&admin),
        )
        .await;
    assert_eq!(changed.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get("/api/v1/users/me", Some(&admin)).await.status,
        StatusCode::OK,
        "own session kept"
    );
    assert_eq!(
        app.get("/api/v1/users/me", Some(&phone)).await.status,
        StatusCode::UNAUTHORIZED,
        "others signed out"
    );
    login(&app, "Felix", "neues-passwort", "phone").await;

    let bad_pin = app
        .request(
            Method::PUT,
            "/api/v1/users/me/pin",
            Some(json!({ "pin": "12a4" })),
            Some(&admin),
        )
        .await;
    assert_eq!(bad_pin.status, StatusCode::UNPROCESSABLE_ENTITY);
    let pin = app
        .request(
            Method::PUT,
            "/api/v1/users/me/pin",
            Some(json!({ "pin": "2468" })),
            Some(&admin),
        )
        .await;
    assert_eq!(pin.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get("/api/v1/users/me", Some(&admin)).await.json["has_pin"],
        true
    );

    // Sign out the phone remotely.
    let devices = app.get("/api/v1/users/me/devices", Some(&admin)).await;
    let phone_id = devices
        .json
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["is_current"] == false)
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let out = app
        .request(
            Method::DELETE,
            &format!("/api/v1/users/me/devices/{phone_id}"),
            None,
            Some(&admin),
        )
        .await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    let devices = app.get("/api/v1/users/me/devices", Some(&admin)).await;
    assert_eq!(devices.json.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn invitations_are_single_use() {
    let app = app().await;
    let admin = app.setup_admin().await;

    let created = app
        .post(
            "/api/v1/invitations",
            json!({ "note": "Für Oma", "max_age_rating": 12, "valid_days": 3 }),
            Some(&admin),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    let code = created.json["code"].as_str().unwrap().to_owned();
    assert_eq!(created.json["invitation"]["note"], "Für Oma");

    let status = app
        .get(&format!("/api/v1/invitations/redeem/{code}"), None)
        .await;
    assert_eq!(status.json["valid"], true);
    let bogus = app.get("/api/v1/invitations/redeem/abc", None).await;
    assert_eq!(bogus.json["valid"], false);

    // A taken name keeps the invitation usable.
    let taken = app
        .post(
            &format!("/api/v1/invitations/redeem/{code}"),
            credentials("Felix", PASSWORD, "oma"),
            None,
        )
        .await;
    assert_eq!(taken.status, StatusCode::CONFLICT);

    let redeemed = app
        .post(
            &format!("/api/v1/invitations/redeem/{code}"),
            credentials("Oma", PASSWORD, "oma"),
            None,
        )
        .await;
    assert_eq!(redeemed.status, StatusCode::OK, "{}", redeemed.json);
    assert_eq!(redeemed.json["user"]["name"], "Oma");
    assert_eq!(redeemed.json["user"]["max_age_rating"], 12);
    assert_eq!(redeemed.json["user"]["is_admin"], false);

    let again = app
        .post(
            &format!("/api/v1/invitations/redeem/{code}"),
            credentials("Opa", PASSWORD, "opa"),
            None,
        )
        .await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    assert_eq!(again.json["code"], "invitation_invalid");

    let list = app.get("/api/v1/invitations", Some(&admin)).await;
    assert!(list.json[0]["used_at"].is_i64());

    let invalid_days = app
        .post(
            "/api/v1/invitations",
            json!({ "valid_days": 90 }),
            Some(&admin),
        )
        .await;
    assert_eq!(invalid_days.status, StatusCode::UNPROCESSABLE_ENTITY);
}
