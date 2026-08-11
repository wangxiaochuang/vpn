#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use quic_link::{Client, Server};

fn repo(p: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("quic-link crate nested under repo root")
        .join(p)
}

fn build_server() -> Server {
    Server::builder()
        .tls_from_files(repo("cert.pem"), repo("key.pem"))
        .build("127.0.0.1:0".parse().unwrap())
        .expect("server build")
}

fn build_client() -> Client {
    Client::builder()
        .trust_ca(repo("cert.pem"))
        .server_name("localhost")
        .build()
        .expect("client build")
}

#[tokio::test]
async fn test_client_connect_returns_session() {
    let server = build_server();
    let addr = server.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = server.accept().await.expect("accept should resolve");
    });

    let client = build_client();
    let session = tokio::time::timeout(std::time::Duration::from_secs(5), client.connect(addr))
        .await
        .expect("connect should not hang")
        .expect("connect should succeed");
    assert!(session.id() > 0);
}

#[tokio::test]
async fn test_client_connect_cert_mismatch_returns_err() {
    let server = build_server();
    let addr = server.local_addr().unwrap();
    tokio::spawn(async move {
        while let Some(res) = server.accept().await {
            let _ = res;
        }
    });

    let client = Client::builder()
        .trust_ca(repo("cert.pem"))
        .server_name("not-the-cert-name")
        .build()
        .expect("client build");
    let result = client.connect(addr).await;
    assert!(
        result.is_err(),
        "connect with mismatched server_name should fail TLS verification"
    );
}

#[tokio::test]
async fn test_server_accept_after_endpoint_close_returns_none() {
    let server = build_server();
    server.close();
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), server.accept())
        .await
        .expect("accept should not hang after close");
    assert!(result.is_none(), "accept returns None after endpoint close");
}
