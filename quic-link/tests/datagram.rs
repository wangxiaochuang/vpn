#![allow(clippy::unwrap_used, clippy::expect_used)]

use bytes::Bytes;

use quic_link::DatagramRx;
use quic_link::DatagramTx;
use quic_link::PacketSink;
use quic_link::PacketSource;
use quic_link::test_util::no_verify_client_config;
use quic_link::test_util::repo_file;

fn build_server_config() -> quinn::ServerConfig {
    quic_link::build_quinn_server_config(&repo_file("cert.pem"), &repo_file("key.pem"))
        .expect("server cfg")
}

struct ConnectionPair {
    _server_endpoint: quinn::Endpoint,
    server: quinn::Connection,
    client: quinn::Connection,
}

async fn make_connection_pair() -> ConnectionPair {
    let server =
        quinn::Endpoint::server(build_server_config(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = server.local_addr().unwrap();
    let server_for_accept = server.clone();
    let accept_task = tokio::spawn(async move {
        server_for_accept
            .accept()
            .await
            .expect("accept")
            .await
            .expect("server accept conn")
    });
    let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    let client_conn = client
        .connect_with(no_verify_client_config(), addr, "localhost")
        .unwrap()
        .await
        .unwrap();
    std::mem::forget(client);
    let server_conn = accept_task.await.unwrap();
    ConnectionPair {
        _server_endpoint: server,
        server: server_conn,
        client: client_conn,
    }
}

#[tokio::test]
async fn test_session_datagram_tx_rx_roundtrip() {
    let pair = make_connection_pair().await;
    let mut tx = DatagramTx::new(pair.client.clone());
    let mut rx = DatagramRx::new(pair.server.clone());

    tx.send(Bytes::from_static(b"hello")).await.unwrap();
    let received = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .expect("recv should not hang")
        .unwrap();
    assert_eq!(received, Bytes::from_static(b"hello"));

    tx.send(Bytes::from_static(b"world")).await.unwrap();
    let received = rx.recv().await.unwrap();
    assert_eq!(received, Bytes::from_static(b"world"));
}

#[tokio::test]
async fn test_datagram_tx_clone_both_send() {
    let pair = make_connection_pair().await;
    let mut tx1 = DatagramTx::new(pair.client.clone());
    let mut tx2 = tx1.clone();
    let mut rx = DatagramRx::new(pair.server.clone());

    tx1.send(Bytes::from_static(b"from-tx1")).await.unwrap();
    tx2.send(Bytes::from_static(b"from-tx2")).await.unwrap();

    let mut got = Vec::new();
    got.push(rx.recv().await.unwrap());
    got.push(rx.recv().await.unwrap());
    assert!(got.contains(&Bytes::from_static(b"from-tx1")));
    assert!(got.contains(&Bytes::from_static(b"from-tx2")));
}
