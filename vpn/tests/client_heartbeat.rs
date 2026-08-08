#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use futures::SinkExt;
use futures::StreamExt;
use tokio_util::codec::Framed;
use vpn::client::heartbeat_loop;
use vpn::ctrl::control_message::Msg;
use vpn::ctrl::{ControlMessage, Heartbeat};
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
async fn test_client_heartbeats_from_server_keep_connection_alive() {
    let pair = common::make_connected_pair().await;
    let (client_writer, client_reader, mut server_writer, mut server_reader) =
        open_control_streams(&pair).await;

    let hb_task = tokio::spawn(heartbeat_loop(
        pair.client.clone(),
        client_reader,
        client_writer,
    ));

    tokio::time::pause();
    for _ in 0..15 {
        server_writer.send(hb()).await.unwrap();
        let _ = tokio::time::timeout(Duration::from_millis(50), server_reader.next()).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    tokio::time::resume();

    assert!(
        pair.client.close_reason().is_none(),
        "connection should stay alive while server heartbeats arrive"
    );
    hb_task.abort();
}

#[tokio::test]
async fn test_client_closes_connection_after_heartbeat_timeout() {
    let pair = common::make_connected_pair().await;
    let (client_writer, client_reader, server_writer, server_reader) =
        open_control_streams(&pair).await;

    let hb_task = tokio::spawn(heartbeat_loop(
        pair.client.clone(),
        client_reader,
        client_writer,
    ));

    tokio::time::pause();
    tokio::time::sleep(Duration::from_secs(35)).await;
    tokio::time::resume();

    let result = tokio::time::timeout(Duration::from_secs(3), hb_task).await;
    assert!(result.is_ok(), "heartbeat loop should exit after timeout");
    assert!(
        pair.client.close_reason().is_some(),
        "client should close the connection after 30s without heartbeats"
    );
    drop(server_writer);
    let _ = server_reader;
}

#[tokio::test]
async fn test_client_exits_when_connection_closed_by_server() {
    let pair = common::make_connected_pair().await;
    let (client_writer, client_reader, mut server_writer, server_reader) =
        open_control_streams(&pair).await;

    let hb_task = tokio::spawn(heartbeat_loop(
        pair.client.clone(),
        client_reader,
        client_writer,
    ));

    let _ = server_writer.close().await;
    pair.server.close(0u32.into(), b"bye");

    let result = tokio::time::timeout(Duration::from_secs(3), hb_task).await;
    assert!(
        result.is_ok(),
        "heartbeat loop should exit when server closes the connection"
    );
    let _ = server_reader;
}
