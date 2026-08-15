#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use futures::StreamExt;
use ipnet::Ipv4Net;
use vpn_core::ctrl::control_message::Msg;

async fn auth_as(
    addr: std::net::SocketAddr,
    username: &str,
    password: &str,
) -> (quinn::Connection, common::ClientFramed, String) {
    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;
    common::send_auth_request(&mut framed, username, password).await;
    let ok = common::recv_control(&mut framed).await.expect("auth ok");
    let ip = match ok.msg {
        Some(Msg::AuthOk(ok)) => ok.assigned_ip,
        other => panic!("expected AuthOk, got {other:?}"),
    };
    (conn, framed, ip)
}

/// Spec scenario: 顶替后老 supervisor retire 后旧 IP 可被新分配。
/// alice 顶替 alice（旧 `.2` → Reserved），老 supervisor retire 后 bob 才可能拿到 `.2`。
/// "retire 前不被复用"的隔离由 Q1 ledger 单测覆盖（reserve 后 alloc 不可见）；
/// 此 Q2 场景验证完整生命周期：evict → reserve → retire → release → realloc。
#[tokio::test]
async fn test_evicted_ip_returned_to_pool_after_old_supervisor_retire() {
    let bob_hash = common::hash_password("b0bpw");
    let mut users = common::alice_users();
    users.push(("bob".to_string(), bob_hash));
    let subnet = Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 29).unwrap();
    let (endpoint, state) = common::start_test_server(users, subnet).await;
    let addr = endpoint.local_addr().unwrap();

    let (_conn1, mut framed1, ip1) = auth_as(addr, "alice", common::ALICE_PASSWORD).await;
    assert_eq!(ip1, "10.0.0.2");
    let available_at_peak = state.ledger.available_count();

    let (_conn2, _framed2, ip2) = auth_as(addr, "alice", common::ALICE_PASSWORD).await;
    assert_eq!(ip2, "10.0.0.3", "second alice gets next ip");

    // 等 alice1 supervisor 完全 retire（.2 由 Reserved → Free）
    let retired = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if state.ledger.available_count() == available_at_peak {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        retired.is_ok(),
        "old ip must return to free (available restored) after retire"
    );

    // retire 后 bob 可拿到释放的 .2
    let (_conn3, _framed3, ip3) = auth_as(addr, "bob", "b0bpw").await;
    assert_eq!(
        ip3, "10.0.0.2",
        "bob gets freed .2 after old supervisor retire"
    );

    let _ = tokio::time::timeout(Duration::from_secs(3), framed1.next()).await;
}

/// Spec scenario: 被顶替的旧 alice supervisor retire 不影响新 alice。
/// alice1 被 alice2 顶替，等老 supervisor retire 后，新 alice 仍在 registry 且 pool 状态正确。
#[tokio::test]
async fn test_retire_of_superseded_does_not_affect_new_session() {
    let (endpoint, state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let (_conn1, mut framed1, _ip1) = auth_as(addr, "alice", common::ALICE_PASSWORD).await;
    let (_conn2, _framed2, ip2) = auth_as(addr, "alice", common::ALICE_PASSWORD).await;

    // 等待老 alice supervisor 完全退出（retire 完成）
    let _ = tokio::time::timeout(Duration::from_secs(3), framed1.next()).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(
        state.ledger.lookup_by_username("alice").is_some(),
        "new alice still in registry after old retire"
    );
    let new_handle = state
        .ledger
        .lookup_by_ip(ip2.parse().unwrap())
        .expect("new alice ip still routable");
    assert_eq!(
        new_handle.ip,
        ip2.parse::<Ipv4Addr>().unwrap(),
        "registry maps to new alice's ip"
    );
}
