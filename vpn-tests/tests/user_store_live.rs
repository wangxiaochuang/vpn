#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::sync::Arc;

use ipnet::Ipv4Net;
use user_store::SqliteUserStore;
use user_store::UserStore as _;
use vpn_core::ctrl::control_message::Msg;

fn subnet() -> Ipv4Net {
    Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap()
}

fn temp_db(dir: &tempfile::TempDir) -> String {
    format!("sqlite://{}", dir.path().join("users.db").display())
}

async fn auth_succeeds(addr: SocketAddr, username: &str, password: &str) -> bool {
    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;
    common::send_auth_request(&mut framed, username, password).await;
    let msg = common::recv_control(&mut framed).await.expect("auth reply");
    matches!(msg.msg, Some(Msg::AuthOk(_)))
}

#[tokio::test]
async fn test_boot_creates_db_and_authenticates_seeded_user() {
    let dir = tempfile::tempdir().unwrap();
    let db = temp_db(&dir);
    let seeding = SqliteUserStore::connect(&db).await.unwrap();
    seeding
        .upsert("alice", &common::hash_password(common::ALICE_PASSWORD))
        .await
        .unwrap();
    drop(seeding);

    let store = Arc::new(SqliteUserStore::connect(&db).await.unwrap());
    let (endpoint, _state, _sd) = common::start_test_server_with_store(store, subnet()).await;
    let addr = endpoint.local_addr().unwrap();
    assert!(
        auth_succeeds(addr, "alice", common::ALICE_PASSWORD).await,
        "seeded user should authenticate via sqlite-backed store"
    );
}

async fn start_running_server(dir: &tempfile::TempDir) -> (SocketAddr, String) {
    let db = temp_db(dir);
    let store = Arc::new(SqliteUserStore::connect(&db).await.unwrap());
    store
        .upsert("alice", &common::hash_password(common::ALICE_PASSWORD))
        .await
        .unwrap();
    let (endpoint, _state, _sd) = common::start_test_server_with_store(store, subnet()).await;
    (endpoint.local_addr().unwrap(), db)
}

#[tokio::test]
async fn test_new_user_takes_effect_without_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, db) = start_running_server(&dir).await;
    let admin = SqliteUserStore::connect(&db).await.unwrap();
    admin
        .upsert("bob", &common::hash_password("pw2"))
        .await
        .unwrap();
    assert!(auth_succeeds(addr, "bob", "pw2").await);
}

#[tokio::test]
async fn test_password_rotation_takes_effect_without_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, db) = start_running_server(&dir).await;
    let admin = SqliteUserStore::connect(&db).await.unwrap();
    admin
        .upsert("alice", &common::hash_password("new-pw"))
        .await
        .unwrap();
    assert!(
        !auth_succeeds(addr, "alice", common::ALICE_PASSWORD).await,
        "old password must be rejected after rotation"
    );
    assert!(auth_succeeds(addr, "alice", "new-pw").await);
}
