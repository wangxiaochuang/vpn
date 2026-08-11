#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use ipnet::Ipv4Net;
use vpn::ctrl::control_message::Msg;

#[tokio::test]
async fn test_server_skips_telemetry_when_client_does_not_open_stream() {
    let (endpoint, _state, _sd) = common::start_test_server_with_shutdown(
        common::alice_users(),
        Ipv4Net::new(std::net::Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
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
    assert!(matches!(reply.msg, Some(Msg::Heartbeat(_))));

    assert!(
        conn.close_reason().is_none(),
        "connection should stay alive after telemetry timeout"
    );
}
