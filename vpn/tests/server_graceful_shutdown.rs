#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use futures::StreamExt;
use ipnet::Ipv4Net;
use vpn::ctrl::control_message::Msg;

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
async fn test_shutdown_cancel_frees_ip_and_clears_registry() {
    let (endpoint, state, shutdown) = common::start_test_server_with_shutdown(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let (_conn, _framed, _ip) = auth_alice(addr).await;

    let available_before = state.pool.lock().unwrap().available_count();
    assert!(
        state
            .registry
            .lock()
            .unwrap()
            .lookup_by_username("alice")
            .is_some(),
        "alice should be in registry while connected"
    );

    shutdown.cancel();

    let cleaned = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let in_registry = state
                .registry
                .lock()
                .unwrap()
                .lookup_by_username("alice")
                .is_some();
            let available = state.pool.lock().unwrap().available_count();
            if !in_registry && available > available_before {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        cleaned.is_ok(),
        "registry entry should be removed and IP returned to pool after shutdown"
    );
}

#[tokio::test]
async fn test_shutdown_cancel_sends_disconnect_to_client() {
    let (endpoint, _state, shutdown) = common::start_test_server_with_shutdown(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let (_conn, mut framed, _ip) = auth_alice(addr).await;

    shutdown.cancel();

    let mut got_disconnect = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), framed.next()).await {
            Ok(Some(Ok(msg))) => {
                if let Some(Msg::Disconnect(ref d)) = msg.msg {
                    assert_eq!(d.reason, "server-shutdown");
                    got_disconnect = true;
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(
        got_disconnect,
        "client should receive a Disconnect {{ reason: \"server-shutdown\" }} after server shutdown"
    );
}
