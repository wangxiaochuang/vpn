#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use futures::StreamExt;
use ipnet::Ipv4Net;
use vpn_core::ctrl::control_message::Msg;

#[tokio::test]
async fn test_disconnect_returns_ip_and_can_realloc() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(std::net::Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn1 = common::test_client_connect(addr).await;
    let mut framed1 = common::open_control(&conn1).await;
    common::send_auth_request(&mut framed1, "alice", common::ALICE_PASSWORD).await;
    let ok1 = common::recv_control(&mut framed1).await.expect("auth ok 1");
    let ip1 = match ok1.msg {
        Some(Msg::AuthOk(ok)) => ok.assigned_ip,
        other => panic!("expected AuthOk, got {other:?}"),
    };

    drop(framed1);
    conn1.close(0u32.into(), b"bye");

    tokio::time::sleep(Duration::from_secs(1)).await;

    let conn2 = common::test_client_connect(addr).await;
    let mut framed2 = common::open_control(&conn2).await;
    common::send_auth_request(&mut framed2, "alice", common::ALICE_PASSWORD).await;
    let ok2 = common::recv_control(&mut framed2).await.expect("auth ok 2");
    let ip2 = match ok2.msg {
        Some(Msg::AuthOk(ok)) => ok.assigned_ip,
        other => panic!("expected AuthOk, got {other:?}"),
    };

    assert_eq!(
        ip1, ip2,
        "after cleanup, the freed IP should be available for reuse"
    );
}

#[tokio::test]
async fn test_superseded_old_conn_cleanup_does_not_affect_new_conn() {
    let (endpoint, state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(std::net::Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn1 = common::test_client_connect(addr).await;
    let mut framed1 = common::open_control(&conn1).await;
    common::send_auth_request(&mut framed1, "alice", common::ALICE_PASSWORD).await;
    let ok1 = common::recv_control(&mut framed1).await.expect("auth ok 1");
    assert_eq!(
        match ok1.msg {
            Some(Msg::AuthOk(ref ok)) => ok.assigned_ip.as_str(),
            _ => panic!("expected AuthOk"),
        },
        "10.0.0.2"
    );

    let conn2 = common::test_client_connect(addr).await;
    let mut framed2 = common::open_control(&conn2).await;
    common::send_auth_request(&mut framed2, "alice", common::ALICE_PASSWORD).await;
    let ok2 = common::recv_control(&mut framed2).await.expect("auth ok 2");
    let ip2 = match ok2.msg {
        Some(Msg::AuthOk(ok)) => ok.assigned_ip,
        other => panic!("expected AuthOk, got {other:?}"),
    };
    assert_eq!(ip2, "10.0.0.3");

    let _ = tokio::time::timeout(Duration::from_secs(3), framed1.next()).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let has_new = state.ledger.lookup_by_username("alice").is_some();
    assert!(
        has_new,
        "new alice session should still be in registry after old cleanup"
    );
}
