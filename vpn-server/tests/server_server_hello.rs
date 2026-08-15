#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use futures::SinkExt;
use futures::StreamExt;
use ipnet::Ipv4Net;
use vpn_core::ctrl::ControlMessage;
use vpn_core::ctrl::PROTOCOL_VERSION;
use vpn_core::ctrl::control_message::Msg;

#[tokio::test]
async fn test_server_hello_is_first_message_before_any_client_data() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;

    framed
        .send(ControlMessage { msg: None })
        .await
        .expect("send open signal");

    let msg = tokio::time::timeout(Duration::from_secs(5), framed.next())
        .await
        .expect("timed out waiting for ServerHello")
        .expect("stream closed")
        .expect("decode error");
    match msg.msg {
        Some(Msg::ServerHello(h)) => {
            assert_eq!(h.protocol_version, PROTOCOL_VERSION);
            assert_eq!(
                h.supported_methods,
                vec![vpn_core::ctrl::AuthMethod::Password as i32]
            );
        }
        other => panic!("expected ServerHello, got {other:?}"),
    }
}

#[tokio::test]
async fn test_connection_stays_open_briefly_without_auth_request() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;

    common::send_open_signal(&mut framed).await;
    let _hello = common::recv_server_hello(&mut framed).await;

    let still_open = tokio::time::timeout(Duration::from_secs(2), framed.next())
        .await
        .is_err();
    assert!(
        still_open,
        "server must wait for AuthRequest, not close immediately"
    );
}

#[tokio::test]
async fn test_non_auth_request_after_server_hello_closes_connection() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;

    common::send_open_signal(&mut framed).await;
    let _hello = common::recv_server_hello(&mut framed).await;
    common::send_heartbeat(&mut framed).await;

    let read_result = tokio::time::timeout(Duration::from_secs(3), framed.next()).await;
    assert!(
        read_result.is_ok(),
        "connection must close after non-AuthRequest first message"
    );
}

#[tokio::test]
async fn test_server_hello_then_auth_request_succeeds() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;

    common::send_open_signal(&mut framed).await;
    let hello = common::recv_server_hello(&mut framed).await;
    assert_eq!(hello.protocol_version, PROTOCOL_VERSION);

    use futures::SinkExt;
    use vpn_core::ctrl::auth_init::Method;
    framed
        .send(ControlMessage {
            msg: Some(Msg::AuthInit(vpn_core::ctrl::AuthInit {
                username: "alice".to_string(),
                method: Some(Method::Password(vpn_core::ctrl::PasswordAuth {
                    password: common::ALICE_PASSWORD.to_string(),
                })),
            })),
        })
        .await
        .expect("send auth init");
    let msg = common::recv_control(&mut framed).await.expect("auth reply");
    match msg.msg {
        Some(Msg::AuthOk(ok)) => {
            assert_eq!(ok.assigned_ip, "10.0.0.2");
        }
        other => panic!("expected AuthOk, got {other:?}"),
    }
}
