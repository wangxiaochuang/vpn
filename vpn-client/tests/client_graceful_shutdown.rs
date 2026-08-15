#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use vpn_client::client::heartbeat_loop;
use vpn_core::ctrl::control_message::Msg;
use vpn_core::ctrl::{ControlMessage, Disconnect, Heartbeat};

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
async fn test_client_exits_immediately_on_server_disconnect() {
    let pair = common::make_connected_pair().await;
    let (client_sender, client_reader, mut server_writer, _server_reader) =
        open_control_halves(&pair).await;

    let hb_task = tokio::spawn(heartbeat_loop(
        quic_link::Session::new(pair.client.clone()),
        client_reader,
        client_sender,
        shutdown::Shutdown::default().handle(),
    ));

    server_writer
        .send(ControlMessage {
            msg: Some(Msg::Disconnect(Disconnect {
                reason: "server-shutdown".to_string(),
            })),
        })
        .await
        .expect("send disconnect");

    let result = tokio::time::timeout(Duration::from_secs(3), hb_task).await;
    assert!(
        result.is_ok(),
        "heartbeat loop should exit immediately on receiving Disconnect, not wait for timeout"
    );
}

#[tokio::test]
async fn test_client_heartbeat_exits_on_cancel() {
    let pair = common::make_connected_pair().await;
    let (client_sender, client_reader, server_writer, server_reader) =
        open_control_halves(&pair).await;

    let sd = shutdown::Shutdown::default();
    let hb_task = tokio::spawn(heartbeat_loop(
        quic_link::Session::new(pair.client.clone()),
        client_reader,
        client_sender,
        sd.handle(),
    ));

    tokio::task::yield_now().await;
    sd.trigger();

    let result = tokio::time::timeout(Duration::from_secs(3), hb_task).await;
    assert!(
        result.is_ok(),
        "heartbeat loop should exit promptly when shutdown token is cancelled"
    );
    drop(server_writer);
    drop(server_reader);
}
