#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use futures::StreamExt;
use ipnet::Ipv4Net;
use vpn_server::ctrl::control_message::Msg;
use vpn_server::data::DownlinkDispatcher;
use vpn_server::server::RegistryDispatcher;

fn ipv4_packet(dst: [u8; 4]) -> bytes::Bytes {
    let mut pkt = vec![0u8; 20];
    pkt[0] = 0x45;
    pkt[16..20].copy_from_slice(&dst);
    bytes::Bytes::from(pkt)
}

async fn assert_no_datagram(conn: &quinn::Connection, label: &str) {
    let r = tokio::time::timeout(Duration::from_millis(200), conn.read_datagram()).await;
    assert!(
        r.is_err() || r.unwrap().is_err(),
        "no datagram data expected: {label}"
    );
}

async fn auth_alice(
    addr: std::net::SocketAddr,
) -> (quinn::Connection, common::ClientFramed, Ipv4Addr) {
    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;
    common::send_auth_request(&mut framed, "alice", common::ALICE_PASSWORD).await;
    let ok = common::recv_control(&mut framed).await.expect("auth ok");
    let ip: Ipv4Addr = match ok.msg {
        Some(Msg::AuthOk(ref ok)) => ok.assigned_ip.parse().unwrap(),
        other => panic!("expected AuthOk, got {other:?}"),
    };
    (conn, framed, ip)
}

#[tokio::test]
async fn test_auth_failed_no_datagram_no_heartbeat_before_close() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;
    common::send_auth_request(&mut framed, "alice", "wrong").await;

    let msg = common::recv_control(&mut framed).await.expect("response");
    assert!(
        matches!(msg.msg, Some(Msg::AuthDenied(_))),
        "expected AuthDenied"
    );

    assert_no_datagram(&conn, "after auth denied, no uplink echo").await;

    let close_result = tokio::time::timeout(Duration::from_secs(3), framed.next()).await;
    assert!(
        close_result.is_ok(),
        "connection must close after auth denied"
    );

    assert_no_datagram(&conn, "after close, no heartbeat or telemetry datagram").await;
}

#[tokio::test]
async fn test_protocol_error_no_datagram_no_heartbeat_before_close() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;
    common::send_heartbeat(&mut framed).await;

    assert_no_datagram(&conn, "after protocol error, no datagram").await;

    let close_result = tokio::time::timeout(Duration::from_secs(3), framed.next()).await;
    assert!(
        close_result.is_ok(),
        "connection must close after protocol error"
    );

    assert_no_datagram(&conn, "after close, no heartbeat or telemetry datagram").await;
}

#[tokio::test]
async fn test_downlink_survives_conn_retire_and_serves_realloc() {
    let (endpoint, state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let (conn_a, framed_a, ip_a) = auth_alice(addr).await;
    let (conn_b, framed_b, ip_b) = auth_alice(addr).await;
    assert_ne!(ip_a, ip_b, "A and B should get different IPs");

    drop(framed_a);
    conn_a.close(0u32.into(), b"bye");
    tokio::time::sleep(Duration::from_secs(1)).await;

    let dispatcher = RegistryDispatcher {
        ledger: state.ledger.clone(),
    };
    let pkt_b = ipv4_packet(ip_b.octets());
    dispatcher.dispatch(pkt_b.clone()).await;
    let received_b = tokio::time::timeout(Duration::from_secs(3), conn_b.read_datagram())
        .await
        .expect("B should receive downlink after A retired")
        .expect("datagram");
    assert_eq!(received_b, pkt_b);

    let (conn_c, framed_c, ip_c) = auth_alice(addr).await;
    assert_eq!(ip_c, ip_a, "C should get A's freed IP after retire");

    let pkt_c = ipv4_packet(ip_c.octets());
    let dispatcher = RegistryDispatcher {
        ledger: state.ledger.clone(),
    };
    dispatcher.dispatch(pkt_c.clone()).await;
    let received_c = tokio::time::timeout(Duration::from_secs(3), conn_c.read_datagram())
        .await
        .expect("C should receive downlink on reallocated IP")
        .expect("datagram");
    assert_eq!(received_c, pkt_c);

    drop(framed_b);
    drop(framed_c);
}
