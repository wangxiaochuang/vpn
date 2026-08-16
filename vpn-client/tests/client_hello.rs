#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::PathBuf;
use std::time::Duration;

use vpn_client::client::{ClientError, PreAuthClient};
use vpn_client::config::ClientConfig;
use vpn_core::ctrl::AuthOk;
use vpn_core::ctrl::ControlMessage;
use vpn_core::ctrl::ServerHello;
use vpn_core::ctrl::control_message::Msg;

fn repo(p: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("vpn-client crate nested under repo root")
        .join(p)
}

fn client_config(addr: std::net::SocketAddr) -> ClientConfig {
    ClientConfig {
        server: addr,
        server_name: "localhost".to_string(),
        ca_cert: repo("cert.pem"),
    }
}

fn server_hello(version: u32) -> ControlMessage {
    use vpn_core::vpn::AuthMethod;
    ControlMessage {
        msg: Some(Msg::ServerHello(ServerHello {
            protocol_version: version,
            supported_methods: vec![AuthMethod::Password as i32],
        })),
    }
}

async fn mock_server(first: ControlMessage) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let server = quic_link::Server::builder()
        .tls_from_files(repo("cert.pem"), repo("key.pem"))
        .build("127.0.0.1:0".parse().unwrap())
        .expect("build server");
    let addr = server.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let session = server.accept().await.expect("accept").expect("conn");
        let mut channel = session
            .accept_stream::<ControlMessage>()
            .await
            .expect("accept stream");
        channel.send(first).await.expect("send first");
        std::future::pending::<()>().await;
    });
    (addr, handle)
}

fn mismatch_config(addr: std::net::SocketAddr) -> ClientConfig {
    ClientConfig {
        server: addr,
        server_name: "evil.invalid".to_string(),
        ca_cert: repo("cert.pem"),
    }
}

#[tokio::test]
async fn test_preauth_connect_when_connection_fails_returns_err() {
    let good = server_hello(vpn_core::ctrl::PROTOCOL_VERSION);
    let (addr, _guard) = mock_server(good).await;
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        PreAuthClient::connect(&mismatch_config(addr)),
    )
    .await
    .expect("TLS name mismatch should fail fast, not hang");
    assert!(
        err.is_err(),
        "connection failure must yield Err before any password prompt"
    );
}

#[tokio::test]
async fn test_preauth_connect_when_version_mismatch_returns_incompatible() {
    let bad = server_hello(99);
    let (addr, _guard) = mock_server(bad).await;
    let err = match PreAuthClient::connect(&client_config(addr)).await {
        Ok(_) => panic!("version mismatch must fail"),
        Err(e) => e,
    };
    match err.downcast_ref::<ClientError>() {
        Some(ClientError::IncompatibleVersion(v)) => assert_eq!(*v, 99),
        other => panic!("expected IncompatibleVersion(99), got {other:?}"),
    }
}

#[tokio::test]
async fn test_preauth_connect_when_first_not_server_hello_returns_protocol_err() {
    let not_hello = ControlMessage {
        msg: Some(Msg::AuthOk(AuthOk {
            assigned_ip: "10.0.0.2".to_string(),
            subnet: "10.0.0.0/24".to_string(),
            gateway: "10.0.0.1".to_string(),
            mtu: 1280,
            routes: vec![],
        })),
    };
    let (addr, _guard) = mock_server(not_hello).await;
    let err = match PreAuthClient::connect(&client_config(addr)).await {
        Ok(_) => panic!("non-ServerHello first message must fail"),
        Err(e) => e,
    };
    assert!(
        matches!(
            err.downcast_ref::<ClientError>(),
            Some(ClientError::Protocol(_))
        ),
        "expected Protocol error, got {err}"
    );
}

#[tokio::test]
async fn test_preauth_connect_when_valid_server_hello_returns_ok() {
    let good = server_hello(vpn_core::ctrl::PROTOCOL_VERSION);
    let (addr, _guard) = mock_server(good).await;
    let pre = tokio::time::timeout(
        Duration::from_secs(5),
        PreAuthClient::connect(&client_config(addr)),
    )
    .await
    .expect("timed out")
    .expect("valid ServerHello should succeed");
    let _ = pre.session_id();
}
