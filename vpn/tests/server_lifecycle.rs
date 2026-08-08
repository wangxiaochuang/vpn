#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use futures::SinkExt;
use futures::StreamExt;
use ipnet::Ipv4Net;
use vpn::ctrl::control_message::Msg;
use vpn::ctrl::{ControlMessage, Heartbeat};

#[tokio::test]
async fn test_full_lifecycle_connect_auth_heartbeat_disconnect_cleanup() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;

    common::send_auth_request(&mut framed, "alice", common::ALICE_PASSWORD).await;
    let auth_ok = common::recv_control(&mut framed).await.expect("auth ok");
    let assigned_ip = match auth_ok.msg {
        Some(Msg::AuthOk(ref ok)) => {
            assert_eq!(ok.assigned_ip, "10.0.0.2");
            assert_eq!(ok.subnet, "10.0.0.0/24");
            assert_eq!(ok.gateway, "10.0.0.1");
            assert_eq!(ok.mtu, 1280);
            ok.assigned_ip.clone()
        }
        other => panic!("expected AuthOk, got {other:?}"),
    };

    tokio::time::pause();
    for _ in 0..3 {
        framed
            .send(ControlMessage {
                msg: Some(Msg::Heartbeat(Heartbeat {})),
            })
            .await
            .expect("send heartbeat");
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    let mut got_server_hb = false;
    while let Ok(Some(Ok(msg))) = tokio::time::timeout(Duration::from_secs(2), framed.next()).await
    {
        if matches!(msg.msg, Some(Msg::Heartbeat(_))) {
            got_server_hb = true;
            break;
        }
    }
    tokio::time::resume();
    assert!(
        got_server_hb,
        "should receive at least one server Heartbeat during session"
    );

    drop(framed);
    conn.close(0u32.into(), b"done");
    tokio::time::sleep(Duration::from_secs(1)).await;

    let conn2 = common::test_client_connect(addr).await;
    let mut framed2 = common::open_control(&conn2).await;
    common::send_auth_request(&mut framed2, "alice", common::ALICE_PASSWORD).await;
    let auth_ok2 = common::recv_control(&mut framed2).await.expect("auth ok 2");
    match auth_ok2.msg {
        Some(Msg::AuthOk(ref ok)) => {
            assert_eq!(
                ok.assigned_ip, assigned_ip,
                "after cleanup the freed IP should be reused"
            );
        }
        other => panic!("expected AuthOk on reconnect, got {other:?}"),
    }
}

#[tokio::test]
async fn test_supersede_then_disconnect_releases_both_ips() {
    let (endpoint, _state) = common::start_test_server(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
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
    assert_eq!(
        match ok2.msg {
            Some(Msg::AuthOk(ref ok)) => ok.assigned_ip.as_str(),
            _ => panic!("expected AuthOk"),
        },
        "10.0.0.3"
    );

    let _ = tokio::time::timeout(Duration::from_secs(3), framed1.next()).await;

    drop(framed2);
    conn2.close(0u32.into(), b"done");
    tokio::time::sleep(Duration::from_secs(1)).await;

    let conn3 = common::test_client_connect(addr).await;
    let mut framed3 = common::open_control(&conn3).await;
    common::send_auth_request(&mut framed3, "alice", common::ALICE_PASSWORD).await;
    let ok3 = common::recv_control(&mut framed3).await.expect("auth ok 3");
    let ip3 = match ok3.msg {
        Some(Msg::AuthOk(ref ok)) => ok.assigned_ip.clone(),
        other => panic!("expected AuthOk on reconnect, got {other:?}"),
    };
    assert!(
        ip3 == "10.0.0.2" || ip3 == "10.0.0.3",
        "one of the freed IPs should be reused, got {ip3}"
    );
}
