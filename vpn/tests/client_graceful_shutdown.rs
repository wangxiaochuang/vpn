#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use futures::SinkExt;
use tokio_util::codec::Framed;
use tokio_util::sync::CancellationToken;
use vpn::client::heartbeat_loop;
use vpn::ctrl::control_message::Msg;
use vpn::ctrl::{ControlMessage, Disconnect, Heartbeat};
use vpn::framing::ControlCodec;

fn hb() -> ControlMessage {
    ControlMessage {
        msg: Some(Msg::Heartbeat(Heartbeat {})),
    }
}

async fn open_control_streams(
    pair: &common::ConnectionPair,
) -> (
    Framed<quinn::SendStream, ControlCodec>,
    Framed<quinn::RecvStream, ControlCodec>,
    Framed<quinn::SendStream, ControlCodec>,
    Framed<quinn::RecvStream, ControlCodec>,
) {
    let (csend, crecv) = pair.client.open_bi().await.unwrap();
    let mut client_writer = Framed::new(csend, ControlCodec::new());
    client_writer.send(hb()).await.unwrap();
    let (ssend, srecv) = pair.server.accept_bi().await.unwrap();
    let client_reader = Framed::new(crecv, ControlCodec::new());
    let server_writer = Framed::new(ssend, ControlCodec::new());
    let server_reader = Framed::new(srecv, ControlCodec::new());
    (client_writer, client_reader, server_writer, server_reader)
}

#[tokio::test]
async fn test_client_exits_immediately_on_server_disconnect() {
    let pair = common::make_connected_pair().await;
    let (client_writer, client_reader, mut server_writer, _server_reader) =
        open_control_streams(&pair).await;

    let hb_task = tokio::spawn(heartbeat_loop(
        pair.client.clone(),
        client_reader,
        client_writer,
        CancellationToken::new(),
    ));

    server_writer
        .send(ControlMessage {
            msg: Some(Msg::Disconnect(Disconnect {
                reason: "server-shutdown".to_string(),
            })),
        })
        .await
        .expect("send disconnect");
    let _ = server_writer.close().await;

    let result = tokio::time::timeout(Duration::from_secs(3), hb_task).await;
    assert!(
        result.is_ok(),
        "heartbeat loop should exit immediately on receiving Disconnect, not wait for timeout"
    );
}

#[tokio::test]
async fn test_client_heartbeat_exits_on_cancel() {
    let pair = common::make_connected_pair().await;
    let (client_writer, client_reader, server_writer, server_reader) =
        open_control_streams(&pair).await;

    let shutdown = CancellationToken::new();
    let hb_task = tokio::spawn(heartbeat_loop(
        pair.client.clone(),
        client_reader,
        client_writer,
        shutdown.clone(),
    ));

    tokio::task::yield_now().await;
    shutdown.cancel();

    let result = tokio::time::timeout(Duration::from_secs(3), hb_task).await;
    assert!(
        result.is_ok(),
        "heartbeat loop should exit promptly when shutdown token is cancelled"
    );
    drop(server_writer);
    let _ = server_reader;
}
