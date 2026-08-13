#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use vpn_client::client::{ExitCause, heartbeat_loop};
use vpn_client::ctrl::control_message::Msg;
use vpn_client::ctrl::{ControlMessage, Disconnect, Heartbeat};

fn sd_handle() -> shutdown::ShutdownHandle {
    shutdown::Shutdown::new(Duration::from_secs(5)).handle()
}

fn hb() -> ControlMessage {
    ControlMessage {
        msg: Some(Msg::Heartbeat(Heartbeat {})),
    }
}

type Halves = (
    msgx::channel::Sender<ControlMessage>,
    msgx::channel::Receiver<ControlMessage>,
    msgx::channel::Sender<ControlMessage>,
    msgx::channel::Receiver<ControlMessage>,
);

async fn open_control_halves(pair: &common::ConnectionPair) -> Halves {
    let (csend, crecv) = pair.client.open_bi().await.unwrap();
    let client_channel = msgx::Channel::from_io(msgx::ByteStream::new(crecv, csend));
    let (mut client_sender, client_receiver) = client_channel.split();
    client_sender.send(hb()).await.unwrap();

    let (ssend, srecv) = pair.server.accept_bi().await.unwrap();
    let server_channel = msgx::Channel::from_io(msgx::ByteStream::new(srecv, ssend));
    let (server_sender, mut server_receiver) = server_channel.split();
    let _ = server_receiver.recv().await.unwrap();

    (
        client_sender,
        client_receiver,
        server_sender,
        server_receiver,
    )
}

#[tokio::test]
async fn test_client_heartbeats_from_server_keep_connection_alive() {
    let pair = common::make_connected_pair().await;
    let (client_sender, client_reader, mut server_writer, mut server_reader) =
        open_control_halves(&pair).await;

    let hb_task = tokio::spawn(heartbeat_loop(
        quic_link::Session::new(pair.client.clone()),
        client_reader,
        client_sender,
        sd_handle(),
    ));

    tokio::time::pause();
    for _ in 0..15 {
        server_writer.send(hb()).await.unwrap();
        let _ = tokio::time::timeout(Duration::from_millis(50), server_reader.recv()).await;
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
    let (client_sender, client_reader, server_writer, server_reader) =
        open_control_halves(&pair).await;

    let hb_task = tokio::spawn(heartbeat_loop(
        quic_link::Session::new(pair.client.clone()),
        client_reader,
        client_sender,
        sd_handle(),
    ));

    tokio::time::pause();
    tokio::time::sleep(Duration::from_secs(35)).await;
    tokio::time::resume();

    let result = tokio::time::timeout(Duration::from_secs(3), hb_task).await;
    assert!(result.is_ok(), "heartbeat loop should exit after timeout");
    assert_eq!(
        result.unwrap().unwrap(),
        ExitCause::HeartbeatEnded,
        "timeout should produce HeartbeatEnded"
    );
    assert!(
        pair.client.close_reason().is_some(),
        "client should close the connection after 30s without heartbeats"
    );
    drop(server_writer);
    drop(server_reader);
}

#[tokio::test]
async fn test_client_exits_with_server_disconnect_cause_on_disconnect_message() {
    let pair = common::make_connected_pair().await;
    let (client_sender, client_reader, mut server_writer, _server_reader) =
        open_control_halves(&pair).await;

    let hb_task = tokio::spawn(heartbeat_loop(
        quic_link::Session::new(pair.client.clone()),
        client_reader,
        client_sender,
        sd_handle(),
    ));

    let _ = server_writer
        .send(ControlMessage {
            msg: Some(Msg::Disconnect(Disconnect {
                reason: "bye".to_string(),
            })),
        })
        .await;

    let result = tokio::time::timeout(Duration::from_secs(3), hb_task).await;
    assert!(
        result.is_ok(),
        "heartbeat loop should exit when server sends Disconnect"
    );
    assert_eq!(
        result.unwrap().unwrap(),
        ExitCause::ServerDisconnect,
        "Disconnect message should produce ServerDisconnect"
    );
}
