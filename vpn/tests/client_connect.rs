#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use futures::StreamExt;
use ipnet::Ipv4Net;
use vpn::ctrl::control_message::Msg;
use vpn::ctrl::{ControlMessage, DenyReason};

#[tokio::test]
async fn test_legal_credentials_receive_auth_ok() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;
    common::send_auth_request(&mut framed, "alice", common::ALICE_PASSWORD).await;

    let msg = common::recv_control(&mut framed).await.expect("auth reply");
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
async fn test_wrong_password_receives_auth_denied() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;
    common::send_auth_request(&mut framed, "alice", "wrong").await;

    let msg = common::recv_control(&mut framed).await.expect("auth reply");
    match msg.msg {
        Some(Msg::AuthDenied(denied)) => {
            assert_eq!(denied.reason, DenyReason::AuthFailed as i32);
        }
        other => panic!("expected AuthDenied, got {other:?}"),
    }

    let result = tokio::time::timeout(Duration::from_secs(3), framed.next()).await;
    assert!(
        matches!(result, Ok(Some(Err(_))) | Ok(None)),
        "server should close the control stream after denying auth"
    );
}

#[tokio::test]
async fn test_pool_exhausted_receives_server_busy() {
    let users = common::alice_users();
    let subnet = Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 30).unwrap();
    let (endpoint, _state) = common::start_test_server(users, subnet).await;
    let addr = endpoint.local_addr().unwrap();

    let conn1 = common::test_client_connect(addr).await;
    let mut framed1 = common::open_control(&conn1).await;
    common::send_auth_request(&mut framed1, "alice", common::ALICE_PASSWORD).await;
    let ok1 = common::recv_control(&mut framed1)
        .await
        .expect("first auth");
    assert!(matches!(ok1.msg, Some(Msg::AuthOk(_))));

    let conn2 = common::test_client_connect(addr).await;
    let mut framed2 = common::open_control(&conn2).await;
    common::send_auth_request(&mut framed2, "alice", common::ALICE_PASSWORD).await;
    let msg2 = common::recv_control(&mut framed2)
        .await
        .expect("second auth");
    match msg2.msg {
        Some(Msg::AuthDenied(denied)) => {
            assert_eq!(denied.reason, DenyReason::ServerBusy as i32);
        }
        other => panic!("expected AuthDenied ServerBusy, got {other:?}"),
    }
}

#[test]
fn test_missing_ca_returns_error_from_tls_builder() {
    let ca = common::repo("nonexistent-ca.pem");
    let result = vpn::tls::build_quinn_client_config(&ca, "localhost");
    assert!(result.is_err(), "missing CA should fail TLS config build");
}

#[test]
fn test_parse_auth_ok_malformed_returns_client_error() {
    let bad = ControlMessage {
        msg: Some(Msg::AuthOk(vpn::ctrl::AuthOk {
            assigned_ip: "not-an-ip".to_string(),
            subnet: "10.0.0.0/24".to_string(),
            gateway: "10.0.0.1".to_string(),
            mtu: 1280,
            routes: vec![],
        })),
    };
    let Msg::AuthOk(ok) = bad.msg.unwrap() else {
        unreachable!()
    };
    assert!(vpn::client::parse_auth_ok(&ok).is_err());
}
