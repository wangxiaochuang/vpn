#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use futures::StreamExt;
use ipnet::Ipv4Net;
use vpn_server::ctrl::control_message::Msg;

async fn auth_alice(
    addr: std::net::SocketAddr,
) -> (quinn::Connection, common::ClientFramed, String) {
    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;
    common::send_auth_request(&mut framed, "alice", common::ALICE_PASSWORD).await;
    let ok = common::recv_control(&mut framed).await.expect("AuthOk");
    let ip = match ok.msg {
        Some(Msg::AuthOk(ok)) => ok.assigned_ip,
        other => panic!("expected AuthOk, got {other:?}"),
    };
    (conn, framed, ip)
}

#[tokio::test]
async fn test_first_core_task_end_triggers_full_cleanup_and_ip_return() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let (conn1, framed1, ip1) = auth_alice(addr).await;

    drop(framed1);
    conn1.close(0u32.into(), b"bye");

    tokio::time::sleep(Duration::from_secs(1)).await;

    let conn2 = common::test_client_connect(addr).await;
    let mut framed2 = common::open_control(&conn2).await;
    let _ = &mut framed2;
    common::send_auth_request(&mut framed2, "alice", common::ALICE_PASSWORD).await;
    let ok2 = common::recv_control(&mut framed2).await.expect("auth ok 2");
    let ip2 = match ok2.msg {
        Some(Msg::AuthOk(ok)) => ok.assigned_ip,
        other => panic!("expected AuthOk, got {other:?}"),
    };
    assert_eq!(
        ip1, ip2,
        "supervisor cleanup must free IP so it can be re-allocated"
    );
}

#[tokio::test]
async fn test_telemetry_task_end_does_not_trigger_conn_cleanup() {
    let (endpoint, _state, _sd) = common::start_test_server_with_shutdown(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;

    let conn = common::test_client_connect(endpoint.local_addr().unwrap()).await;
    let mut ctrl = common::open_control(&conn).await;
    common::send_auth_request(&mut ctrl, "alice", common::ALICE_PASSWORD).await;
    let _ok = common::recv_control(&mut ctrl).await.expect("AuthOk");

    tokio::time::sleep(Duration::from_secs(7)).await;

    common::send_heartbeat(&mut ctrl).await;
    let reply = tokio::time::timeout(Duration::from_secs(10), common::recv_control(&mut ctrl))
        .await
        .expect("timeout waiting for heartbeat reply after telemetry skip")
        .expect("no heartbeat reply");
    assert!(
        matches!(reply.msg, Some(Msg::Heartbeat(_))),
        "connection alive after telemetry ended"
    );

    assert!(
        conn.close_reason().is_none(),
        "telemetry exit SHALL NOT close connection"
    );
}

#[tokio::test]
async fn test_uplink_end_via_client_disconnect_triggers_cleanup() {
    let (endpoint, state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let (conn1, framed1, _ip1) = auth_alice(addr).await;

    drop(framed1);
    conn1.close(0u32.into(), b"bye");

    tokio::time::sleep(Duration::from_secs(1)).await;

    let registry_empty = state.ledger.lookup_by_username("alice").is_none();
    assert!(
        registry_empty,
        "supervisor cleanup must remove registry entry after uplink end"
    );
}

#[tokio::test]
async fn test_supervisor_cleanup_is_idempotent_under_supersede() {
    let (endpoint, state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let (conn1, mut framed1, _ip1) = auth_alice(addr).await;
    let (conn2, framed2, ip2) = auth_alice(addr).await;
    assert_ne!(_ip1, ip2, "second alice must get a different IP");

    let _ = tokio::time::timeout(Duration::from_secs(3), framed1.next()).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    drop(framed1);
    drop(framed2);
    conn1.close(0u32.into(), b"bye");
    conn2.close(0u32.into(), b"bye");

    tokio::time::sleep(Duration::from_secs(1)).await;

    let registry_empty = state.ledger.lookup_by_username("alice").is_none();
    assert!(
        registry_empty,
        "both connections cleaned up after supersede"
    );
}
