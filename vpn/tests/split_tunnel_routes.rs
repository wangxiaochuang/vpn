#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::Ipv4Addr;

use ipnet::Ipv4Net;
use vpn::ctrl::DenyReason;
use vpn::ctrl::control_message::Msg;

#[tokio::test]
async fn test_server_with_routes_sends_routes_in_auth_ok() {
    let routes = vec![
        Ipv4Net::new(Ipv4Addr::new(192, 168, 100, 0), 24).unwrap(),
        Ipv4Net::new(Ipv4Addr::new(10, 88, 0, 0), 16).unwrap(),
    ];
    let (endpoint, _state, _shutdown) = common::start_test_server_with_routes(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
        routes,
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;
    common::send_auth_request(&mut framed, "alice", common::ALICE_PASSWORD).await;

    let msg = common::recv_control(&mut framed).await.expect("auth reply");
    let Msg::AuthOk(ok) = msg.msg.expect("AuthOk") else {
        panic!("expected AuthOk");
    };
    assert_eq!(ok.assigned_ip, "10.0.0.2");
    assert_eq!(ok.subnet, "10.0.0.0/24");
    assert_eq!(ok.gateway, "10.0.0.1");
    assert_eq!(ok.mtu, 1280);
    assert_eq!(ok.routes.len(), 2);
    assert_eq!(ok.routes[0], "192.168.100.0/24");
    assert_eq!(ok.routes[1], "10.88.0.0/16");
}

#[tokio::test]
async fn test_server_without_routes_sends_empty_routes_in_auth_ok() {
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
    let Msg::AuthOk(ok) = msg.msg.expect("AuthOk") else {
        panic!("expected AuthOk");
    };
    assert!(ok.routes.is_empty());
}

#[tokio::test]
async fn test_client_parses_routes_from_auth_ok_over_wire() {
    let routes = vec![Ipv4Net::new(Ipv4Addr::new(172, 16, 0, 0), 12).unwrap()];
    let (endpoint, _state, _shutdown) = common::start_test_server_with_routes(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
        routes,
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;
    common::send_auth_request(&mut framed, "alice", common::ALICE_PASSWORD).await;

    let msg = common::recv_control(&mut framed).await.expect("auth reply");
    let Msg::AuthOk(ok) = msg.msg.expect("AuthOk") else {
        panic!("expected AuthOk");
    };

    let params = vpn::client::parse_auth_ok(&ok).expect("parse_auth_ok");
    assert_eq!(params.assigned_ip, Ipv4Addr::new(10, 0, 0, 2));
    assert_eq!(
        params.routes,
        vec![Ipv4Net::new(Ipv4Addr::new(172, 16, 0, 0), 12).unwrap()]
    );
}

#[tokio::test]
async fn test_auth_denied_unaffected_by_routes() {
    let routes = vec![Ipv4Net::new(Ipv4Addr::new(192, 168, 100, 0), 24).unwrap()];
    let (endpoint, _state, _shutdown) = common::start_test_server_with_routes(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
        routes,
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let conn = common::test_client_connect(addr).await;
    let mut framed = common::open_control(&conn).await;
    common::send_auth_request(&mut framed, "alice", "wrong").await;

    let msg = common::recv_control(&mut framed).await.expect("auth reply");
    let Msg::AuthDenied(denied) = msg.msg.expect("AuthDenied") else {
        panic!("expected AuthDenied");
    };
    assert_eq!(denied.reason, DenyReason::AuthFailed as i32);
}

#[test]
fn test_auth_ok_with_routes_over_wire_round_trips() {
    let ok = vpn::ctrl::AuthOk {
        assigned_ip: "10.0.0.2".to_string(),
        subnet: "10.0.0.0/24".to_string(),
        gateway: "10.0.0.1".to_string(),
        mtu: 1280,
        routes: vec!["192.168.100.0/24".to_string(), "10.88.0.0/16".to_string()],
    };
    let params = vpn::client::parse_auth_ok(&ok).expect("parse_auth_ok");
    assert_eq!(params.routes.len(), 2);
    assert_eq!(
        params.routes[0],
        "192.168.100.0/24".parse::<Ipv4Net>().unwrap()
    );
}
