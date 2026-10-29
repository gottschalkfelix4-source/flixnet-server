use flixnet_db::sessions::{self, NewSession};
use flixnet_db::{Db, users};
use tempfile::TempDir;

async fn open_temp() -> (TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    (dir, db)
}

fn session<'a>(user_id: i64, device: &'a str, hash: &'a str) -> NewSession<'a> {
    NewSession {
        user_id,
        device_uid: device,
        client: "Flixnet Web",
        device_name: "Chrome · Windows",
        token_hash: hash,
    }
}

#[tokio::test]
async fn first_admin_can_only_be_created_once() {
    let (_dir, db) = open_temp().await;
    assert_eq!(users::count(&db).await.unwrap(), 0);

    let admin = users::create_first_admin(&db, "Felix", "phc")
        .await
        .unwrap()
        .unwrap();
    assert!(admin.is_admin);
    assert_eq!(users::count(&db).await.unwrap(), 1);

    assert_eq!(
        users::create_first_admin(&db, "Mallory", "phc")
            .await
            .unwrap(),
        None
    );
    assert_eq!(users::count(&db).await.unwrap(), 1);
}

#[tokio::test]
async fn names_are_case_insensitive() {
    let (_dir, db) = open_temp().await;
    users::create_first_admin(&db, "Felix", "phc")
        .await
        .unwrap();
    let found = users::find_by_name(&db, "fElIx").await.unwrap().unwrap();
    assert_eq!(found.name, "Felix");
    assert!(users::find_by_name(&db, "nobody").await.unwrap().is_none());
}

#[tokio::test]
async fn relogin_on_same_device_revokes_the_old_token() {
    let (_dir, db) = open_temp().await;
    let user = users::create_first_admin(&db, "Felix", "phc")
        .await
        .unwrap()
        .unwrap();

    sessions::create(&db, &session(user.id, "browser-1", "hash-a"))
        .await
        .unwrap();
    let first = sessions::find_by_token_hash(&db, "hash-a")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.user_name, "Felix");
    assert!(first.is_admin);

    sessions::create(&db, &session(user.id, "browser-1", "hash-b"))
        .await
        .unwrap();
    assert!(
        sessions::find_by_token_hash(&db, "hash-a")
            .await
            .unwrap()
            .is_none()
    );
    let second = sessions::find_by_token_hash(&db, "hash-b")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        second.device_id, first.device_id,
        "same device row is reused"
    );

    // A different device keeps its own token.
    sessions::create(&db, &session(user.id, "tv-1", "hash-c"))
        .await
        .unwrap();
    assert!(
        sessions::find_by_token_hash(&db, "hash-b")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        sessions::find_by_token_hash(&db, "hash-c")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn revoked_tokens_no_longer_resolve() {
    let (_dir, db) = open_temp().await;
    let user = users::create_first_admin(&db, "Felix", "phc")
        .await
        .unwrap()
        .unwrap();
    let token_id = sessions::create(&db, &session(user.id, "d", "h"))
        .await
        .unwrap();

    let s = sessions::find_by_token_hash(&db, "h")
        .await
        .unwrap()
        .unwrap();
    sessions::touch(&db, s.token_id, s.device_id).await.unwrap();

    sessions::revoke(&db, token_id).await.unwrap();
    assert!(
        sessions::find_by_token_hash(&db, "h")
            .await
            .unwrap()
            .is_none()
    );
}
