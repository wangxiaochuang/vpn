#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use futures::SinkExt;
use futures::StreamExt;
use ipnet::Ipv4Net;
use vpn_core::ctrl::control_message::Msg;
use vpn_core::ctrl::{ControlMessage, Heartbeat};

#[tokio::test]
async fn test_client_heartbeat_keeps_connection_alive() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(std::net::Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;
    common::send_auth_request(&mut framed, "alice", common::ALICE_PASSWORD).await;
    let _ = common::recv_control(&mut framed).await.expect("auth ok");

    tokio::time::pause();

    for _ in 0..15 {
        framed
            .send(ControlMessage {
                msg: Some(Msg::Heartbeat(Heartbeat {})),
            })
            .await
            .expect("send heartbeat");
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    assert!(
        conn.close_reason().is_none(),
        "connection should still be alive after heartbeats"
    );
}

#[tokio::test]
async fn test_no_heartbeat_server_closes_after_timeout() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(std::net::Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;
    common::send_auth_request(&mut framed, "alice", common::ALICE_PASSWORD).await;
    let _ = common::recv_control(&mut framed).await.expect("auth ok");

    tokio::time::pause();
    tokio::time::sleep(Duration::from_secs(35)).await;
    tokio::time::resume();

    let result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match framed.next().await {
                Some(Err(_)) | None => return,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    assert!(result.is_ok(), "connection should be closed after timeout");
}

#[tokio::test]
async fn test_server_sends_periodic_heartbeat() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(std::net::Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;
    common::send_auth_request(&mut framed, "alice", common::ALICE_PASSWORD).await;
    let _ = common::recv_control(&mut framed).await.expect("auth ok");

    tokio::time::pause();
    tokio::time::sleep(Duration::from_secs(11)).await;

    let mut got_heartbeat = false;
    while let Ok(Some(Ok(msg))) = tokio::time::timeout(Duration::from_secs(2), framed.next()).await
    {
        if matches!(msg.msg, Some(Msg::Heartbeat(_))) {
            got_heartbeat = true;
            break;
        }
    }
    assert!(got_heartbeat, "should receive at least one Heartbeat");
}
