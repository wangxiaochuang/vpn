#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::Ipv4Addr;

use ipnet::Ipv4Net;
use vpn::ctrl::DenyReason;
use vpn::ctrl::control_message::Msg;

#[tokio::test]
async fn test_valid_credentials_receive_auth_ok() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let client_conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&client_conn).await;
    common::send_auth_request(&mut framed, "alice", common::ALICE_PASSWORD).await;

    let msg = common::recv_control(&mut framed).await.expect("response");
    match msg.msg {
        Some(Msg::AuthOk(ok)) => {
            assert_eq!(ok.assigned_ip, "10.0.0.2");
            assert_eq!(ok.subnet, "10.0.0.0/24");
            assert_eq!(ok.gateway, "10.0.0.1");
            assert_eq!(ok.mtu, 1280);
        }
        other => panic!("expected AuthOk, got {other:?}"),
    }
}

#[tokio::test]
async fn test_wrong_password_receives_auth_denied_and_connection_closes() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let client_conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&client_conn).await;
    common::send_auth_request(&mut framed, "alice", "wrong").await;

    let msg = common::recv_control(&mut framed).await.expect("response");
    match msg.msg {
        Some(Msg::AuthDenied(d)) => {
            assert_eq!(d.reason, DenyReason::AuthFailed as i32);
        }
        other => panic!("expected AuthDenied, got {other:?}"),
    }

    let read_result = futures::StreamExt::next(&mut framed).await;
    assert!(
        read_result.is_none() || read_result.unwrap().is_err(),
        "stream should be closed after AuthDenied"
    );
}

#[tokio::test]
async fn test_pool_exhausted_receives_server_busy() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 30).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let client_conn1 = common::test_client_connect(addr).await;
    let mut framed1 = common::open_control(&client_conn1).await;
    common::send_auth_request(&mut framed1, "alice", common::ALICE_PASSWORD).await;
    let msg = common::recv_control(&mut framed1).await.expect("response");
    assert!(matches!(msg.msg, Some(Msg::AuthOk(_))));

    let client_conn2 = common::test_client_connect(addr).await;
    let mut framed2 = common::open_control(&client_conn2).await;
    common::send_auth_request(&mut framed2, "alice", common::ALICE_PASSWORD).await;

    let msg = common::recv_control(&mut framed2).await.expect("response");
    match msg.msg {
        Some(Msg::AuthDenied(d)) => {
            assert_eq!(d.reason, DenyReason::ServerBusy as i32);
        }
        other => panic!("expected AuthDenied SERVER_BUSY, got {other:?}"),
    }
}

#[tokio::test]
async fn test_first_message_not_auth_request_closes_connection() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let client_conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&client_conn).await;
    common::send_heartbeat(&mut framed).await;

    let read_result = futures::StreamExt::next(&mut framed).await;
    assert!(
        read_result.is_none() || read_result.unwrap().is_err(),
        "stream should be closed without AuthOk/AuthDenied"
    );
}

#[tokio::test]
async fn test_auth_denied_connection_closes_within_bounded_time_without_sleep() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let client_conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&client_conn).await;
    common::send_auth_request(&mut framed, "alice", "wrong").await;

    let msg = common::recv_control(&mut framed).await.expect("response");
    assert!(matches!(msg.msg, Some(Msg::AuthDenied(_))));

    let close_result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        futures::StreamExt::next(&mut framed),
    )
    .await;
    assert!(
        close_result.is_ok(),
        "connection must close within bounded time after AuthDenied (FIN handshake, no sleep)"
    );
}
