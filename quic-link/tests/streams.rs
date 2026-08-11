#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;

use quic_link::{Client, PacketSink, PacketSource, Server, Session};

#[derive(Clone, PartialEq, prost::Message)]
struct Ctrl {
    #[prost(string, tag = "1")]
    v: String,
}

#[derive(Clone, PartialEq, prost::Message)]
struct Data {
    #[prost(uint32, tag = "1")]
    n: u32,
}

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

async fn make_pair() -> (Server, Client, Session, Session) {
    let server = build_server();
    let addr = server.local_addr().unwrap();
    let client = build_client();
    let (server_result, client_result) = tokio::join!(server.accept(), client.connect(addr));
    let server_sess = server_result.expect("accept").expect("conn");
    let client_sess = client_result.expect("connect");
    (server, client, server_sess, client_sess)
}

#[tokio::test]
async fn test_open_accept_stream_bidirectional() {
    let (_server, _client, server_sess, client_sess) = make_pair().await;
    let client_task = tokio::spawn(async move {
        let mut ch = client_sess.open_stream::<Ctrl>().await.unwrap();
        ch.send(Ctrl { v: "hi".into() }).await.unwrap();
        ch
    });
    let mut server_ch = server_sess.accept_stream::<Ctrl>().await.unwrap();
    let mut client_ch = client_task.await.unwrap();
    let received = server_ch.recv().await.unwrap().unwrap();
    assert_eq!(received, Ctrl { v: "hi".into() });
    server_ch.send(Ctrl { v: "bye".into() }).await.unwrap();
    let echo = client_ch.recv().await.unwrap().unwrap();
    assert_eq!(echo, Ctrl { v: "bye".into() });
}

#[tokio::test]
async fn test_two_streams_independent_no_hol_blocking() {
    let (_server, _client, server_sess, client_sess) = make_pair().await;
    let server_task = tokio::spawn(async move {
        let _server_ctrl = server_sess.accept_stream::<Ctrl>().await.unwrap();
        let mut server_data = server_sess.accept_stream::<Data>().await.unwrap();
        server_data.recv().await.unwrap().unwrap()
    });
    let mut ctrl = client_sess.open_stream::<Ctrl>().await.unwrap();
    ctrl.send(Ctrl { v: "x".into() }).await.unwrap();
    let mut data = client_sess.open_stream::<Data>().await.unwrap();
    data.send(Data { n: 1 }).await.unwrap();
    let got_data = server_task.await.unwrap();
    assert_eq!(got_data, Data { n: 1 });
}

#[tokio::test]
async fn test_two_streams_different_msg_types() {
    let (_server, _client, server_sess, client_sess) = make_pair().await;
    let server_task = tokio::spawn(async move {
        let mut server_ctrl = server_sess.accept_stream::<Ctrl>().await.unwrap();
        let mut server_data = server_sess.accept_stream::<Data>().await.unwrap();
        (
            server_ctrl.recv().await.unwrap().unwrap(),
            server_data.recv().await.unwrap().unwrap(),
        )
    });
    let mut ctrl = client_sess.open_stream::<Ctrl>().await.unwrap();
    ctrl.send(Ctrl { v: "ctrl".into() }).await.unwrap();
    let mut data = client_sess.open_stream::<Data>().await.unwrap();
    data.send(Data { n: 42 }).await.unwrap();
    let (got_ctrl, got_data) = server_task.await.unwrap();
    assert_eq!(got_ctrl, Ctrl { v: "ctrl".into() });
    assert_eq!(got_data, Data { n: 42 });
}

#[tokio::test]
async fn test_accept_stream_outer_timeout_no_deadlock() {
    let (_server, _client, server_sess, client_sess) = make_pair().await;
    let _ = client_sess;
    let result = tokio::time::timeout(
        Duration::from_millis(100),
        server_sess.accept_stream::<Ctrl>(),
    )
    .await;
    assert!(result.is_err(), "timeout should fire, not deadlock");
}

#[tokio::test]
async fn test_session_datagram_works_without_any_stream() {
    let (_server, _client, server_sess, client_sess) = make_pair().await;
    let mut tx = client_sess.datagram_tx();
    let mut rx = server_sess.datagram_rx();
    tx.send(Bytes::from_static(b"dg")).await.unwrap();
    let received = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("recv")
        .unwrap();
    assert_eq!(received, Bytes::from_static(b"dg"));
}
