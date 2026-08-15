#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use futures::StreamExt;
use ipnet::Ipv4Net;
use vpn_core::ctrl::control_message::Msg;

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

/// 服务端优雅关闭时，客户端可通过两条可观测路径之一感知：
/// 1. 收到 Disconnect 控制消息（reason="server-shutdown"），或
/// 2. 连接被对端以 APPLICATION_CLOSE 关闭（reason="server-shutdown"）。
///
/// 二者在 drain 期间发送 Disconnect 与随后 `session.close()` 之间存在固有竞态，
/// 均属合法契约。
async fn expect_client_observes_server_shutdown(
    conn: &quinn::Connection,
    framed: &mut common::ClientFramed,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "client did not observe server shutdown; close_reason={:?}",
                conn.close_reason()
            );
        }
        if observed_shutdown(conn, framed).await {
            return;
        }
    }
}

async fn observed_shutdown(conn: &quinn::Connection, framed: &mut common::ClientFramed) -> bool {
    match tokio::time::timeout(Duration::from_secs(2), framed.next()).await {
        Ok(Some(Ok(msg))) => {
            if let Some(Msg::Disconnect(ref d)) = msg.msg {
                assert_eq!(d.reason, "server-shutdown");
                return true;
            }
            false
        }
        _ => {
            if is_server_shutdown_close(conn) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            false
        }
    }
}

fn is_server_shutdown_close(conn: &quinn::Connection) -> bool {
    matches!(
        conn.close_reason(),
        Some(quinn::ConnectionError::ApplicationClosed(ref ac))
            if ac.reason.as_ref() == b"server-shutdown"
    )
}

#[tokio::test]
async fn test_shutdown_cancel_frees_ip_and_clears_registry() {
    let (endpoint, state, sd) = common::start_test_server_with_shutdown(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let (_conn, _framed, _ip) = auth_alice(addr).await;

    let available_before = state.ledger.available_count();
    assert!(
        state.ledger.lookup_by_username("alice").is_some(),
        "alice should be in registry while connected"
    );

    sd.trigger();

    let cleaned = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let in_registry = state.ledger.lookup_by_username("alice").is_some();
            let available = state.ledger.available_count();
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
    let (endpoint, _state, sd) = common::start_test_server_with_shutdown(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let addr = endpoint.local_addr().unwrap();

    let (conn, mut framed, _ip) = auth_alice(addr).await;

    sd.trigger();

    expect_client_observes_server_shutdown(&conn, &mut framed).await;
}

#[tokio::test]
async fn test_shutdown_on_sigterm_sends_disconnect_to_client() {
    let (endpoint, _state, sd) = common::start_test_server_with_shutdown(
        common::alice_users(),
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
    )
    .await;
    let ready = sd.spawn_signal_watchdog();
    ready
        .await
        .expect("watchdog should finish registering signal handlers");
    let addr = endpoint.local_addr().unwrap();

    let (conn, mut framed, _ip) = auth_alice(addr).await;

    unsafe {
        libc::kill(libc::getpid(), libc::SIGTERM);
    }

    expect_client_observes_server_shutdown(&conn, &mut framed).await;
}
