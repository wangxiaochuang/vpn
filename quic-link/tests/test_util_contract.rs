#![allow(clippy::unwrap_used, clippy::expect_used)]

use quic_link::test_util::make_session_pair;
use quic_link::test_util::repo_file;

#[derive(Clone, PartialEq, prost::Message)]
struct PairMsg {
    #[prost(string, tag = "1")]
    text: String,
}

#[tokio::test]
async fn test_make_session_pair_streams_interoperate() {
    let (server, client) = make_session_pair(&repo_file("cert.pem"), &repo_file("key.pem")).await;

    let accept_task = tokio::spawn(async move { server.accept_stream::<PairMsg>().await });
    let mut tx = client.open_stream::<PairMsg>().await.unwrap();
    tx.send(PairMsg {
        text: "ping".to_string(),
    })
    .await
    .unwrap();
    let mut rx = accept_task.await.unwrap().unwrap();

    let got = rx.recv().await.unwrap().unwrap();
    assert_eq!(got.text, "ping");
}
