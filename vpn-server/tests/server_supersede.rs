#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use futures::StreamExt;
use ipnet::Ipv4Net;
use vpn_server::ctrl::control_message::Msg;

#[tokio::test]
async fn test_second_same_username_supersedes_first_connection() {
    let users = common::alice_users();
    let subnet = Ipv4Net::new(std::net::Ipv4Addr::new(10, 0, 0, 0), 24).unwrap();
    let (endpoint, _state) = common::start_test_server(users, subnet).await;
    let addr = endpoint.local_addr().unwrap();

    let conn1 = common::test_client_connect(addr).await;
    let mut framed1 = common::open_control(&conn1).await;
    common::send_auth_request(&mut framed1, "alice", common::ALICE_PASSWORD).await;
    let ok1 = common::recv_control(&mut framed1)
        .await
        .expect("first auth ok");
    let ip1 = match ok1.msg {
        Some(Msg::AuthOk(ok)) => ok.assigned_ip,
        other => panic!("expected AuthOk for first, got {other:?}"),
    };
    assert_eq!(ip1, "10.0.0.2");

    let conn2 = common::test_client_connect(addr).await;
    let mut framed2 = common::open_control(&conn2).await;
    common::send_auth_request(&mut framed2, "alice", common::ALICE_PASSWORD).await;
    let ok2 = common::recv_control(&mut framed2)
        .await
        .expect("second auth ok");
    let ip2 = match ok2.msg {
        Some(Msg::AuthOk(ok)) => ok.assigned_ip,
        other => panic!("expected AuthOk for second, got {other:?}"),
    };
    assert_ne!(ip1, ip2, "second connection should get a different IP");
    assert_eq!(ip2, "10.0.0.3");

    loop {
        let read_result = tokio::time::timeout(Duration::from_secs(3), framed1.next()).await;
        match read_result {
            Ok(Some(Err(_))) | Ok(None) => break,
            Ok(Some(Ok(_))) => continue,
            Err(_) => panic!("timeout waiting for first connection to close"),
        }
    }
}

#[tokio::test]
async fn test_superseded_old_ip_can_be_reallocated() {
    let bob_hash = common::hash_password("b0bpw");
    let users = {
        let mut u = common::alice_users();
        u.push(("bob".to_string(), bob_hash));
        u
    };
    let subnet = Ipv4Net::new(std::net::Ipv4Addr::new(10, 0, 0, 0), 24).unwrap();
    let (endpoint, _state) = common::start_test_server(users, subnet).await;
    let addr = endpoint.local_addr().unwrap();

    let conn1 = common::test_client_connect(addr).await;
    let mut framed1 = common::open_control(&conn1).await;
    common::send_auth_request(&mut framed1, "alice", common::ALICE_PASSWORD).await;
    let ok1 = common::recv_control(&mut framed1)
        .await
        .expect("first auth ok");
    let ip1 = match ok1.msg {
        Some(Msg::AuthOk(ok)) => ok.assigned_ip,
        other => panic!("expected AuthOk, got {other:?}"),
    };
    assert_eq!(ip1, "10.0.0.2");

    let conn2 = common::test_client_connect(addr).await;
    let mut framed2 = common::open_control(&conn2).await;
    common::send_auth_request(&mut framed2, "alice", common::ALICE_PASSWORD).await;
    let ok2 = common::recv_control(&mut framed2)
        .await
        .expect("second auth ok");
    match ok2.msg {
        Some(Msg::AuthOk(ok)) => assert_eq!(ok.assigned_ip, "10.0.0.3"),
        other => panic!("expected AuthOk for second alice, got {other:?}"),
    }

    let _ = tokio::time::timeout(Duration::from_secs(3), framed1.next()).await;

    let conn3 = common::test_client_connect(addr).await;
    let mut framed3 = common::open_control(&conn3).await;
    common::send_auth_request(&mut framed3, "bob", "b0bpw").await;
    let ok3 = common::recv_control(&mut framed3)
        .await
        .expect("bob auth ok");
    let ip3 = match ok3.msg {
        Some(Msg::AuthOk(ok)) => ok.assigned_ip,
        other => panic!("expected AuthOk for bob, got {other:?}"),
    };
    assert!(
        ip3 == "10.0.0.2" || ip3 == "10.0.0.4",
        "bob should get freed IP 10.0.0.2 or next allocatable, got {ip3}"
    );
}
