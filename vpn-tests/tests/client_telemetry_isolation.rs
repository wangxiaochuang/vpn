#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use quic_link::Session;
use sysprobe::collector::CollectorRegistry;
use sysprobe::proto::TelemetryMessage;
use sysprobe::proto::TelemetryReport;
use sysprobe::proto::telemetry_message::Msg;
use vpn_client::client::heartbeat_loop;
use vpn_client::telemetry::client_telemetry_loop;
use vpn_core::ctrl::ControlMessage;
use vpn_core::ctrl::Heartbeat;
use vpn_core::ctrl::control_message::Msg as CtrlMsg;

fn hb() -> ControlMessage {
    ControlMessage {
        msg: Some(CtrlMsg::Heartbeat(Heartbeat {})),
    }
}

fn empty_report() -> TelemetryMessage {
    TelemetryMessage {
        msg: Some(Msg::Report(TelemetryReport {
            ts_ms: 0,
            items: vec![],
        })),
    }
}

async fn open_telemetry_pair(
    pair: &common::ConnectionPair,
) -> (
    msgx::channel::Receiver<TelemetryMessage>,
    msgx::channel::Receiver<TelemetryMessage>,
) {
    let server_session = Session::new(pair.server.clone());
    let server_accept = tokio::spawn(async move {
        server_session
            .accept_stream::<TelemetryMessage>()
            .await
            .unwrap()
    });

    let client_session = Session::new(pair.client.clone());
    let client_channel = client_session
        .open_stream::<TelemetryMessage>()
        .await
        .unwrap();
    let (mut client_writer, client_reader) = client_channel.split();
    let _ = client_writer.send(empty_report()).await;

    let server_channel = tokio::time::timeout(Duration::from_secs(5), server_accept)
        .await
        .expect("server telemetry accept timeout")
        .unwrap();
    let (server_writer, server_reader) = server_channel.split();

    std::mem::forget(client_writer);
    std::mem::forget(server_writer);
    (client_reader, server_reader)
}

#[tokio::test]
async fn test_telemetry_stream_close_does_not_affect_control_stream() {
    let pair = common::make_connected_pair().await;

    let (csend, crecv) = pair.client.open_bi().await.unwrap();
    let client_control = msgx::Channel::from_io(msgx::ByteStream::new(crecv, csend));
    let (mut client_sender, client_receiver) = client_control.split();
    client_sender.send(hb()).await.unwrap();

    let (ssend, srecv) = pair.server.accept_bi().await.unwrap();
    let server_control = msgx::Channel::from_io(msgx::ByteStream::new(srecv, ssend));
    let (mut server_sender, mut server_receiver) = server_control.split();
    let _ = server_receiver.recv().await.unwrap();

    let hb_task = tokio::spawn(heartbeat_loop(
        Session::new(pair.client.clone()),
        client_receiver,
        client_sender,
        shutdown::Shutdown::default().handle(),
    ));

    let (client_reader, server_reader) = open_telemetry_pair(&pair).await;
    drop(client_reader);
    drop(server_reader);

    server_sender.send(hb()).await.unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(3), server_receiver.recv())
        .await
        .expect("timeout waiting for heartbeat reply after telemetry close")
        .expect("recv error")
        .expect("control stream closed");
    assert!(matches!(reply.msg, Some(CtrlMsg::Heartbeat(_))));

    assert!(
        pair.client.close_reason().is_none(),
        "client should keep the connection alive after telemetry stream close"
    );
    hb_task.abort();
}

#[tokio::test]
async fn test_telemetry_loop_exits_on_shutdown_cancel() {
    let pair = common::make_connected_pair().await;

    let server_session = Session::new(pair.server.clone());
    let server_accept = tokio::spawn(async move {
        server_session
            .accept_stream::<TelemetryMessage>()
            .await
            .unwrap()
    });

    let client_session = Session::new(pair.client.clone());
    let client_channel = client_session
        .open_stream::<TelemetryMessage>()
        .await
        .unwrap();
    let (mut client_writer, client_reader) = client_channel.split();
    let _ = client_writer.send(empty_report()).await;

    let server_channel = tokio::time::timeout(Duration::from_secs(5), server_accept)
        .await
        .expect("server telemetry accept timeout")
        .unwrap();
    let (_server_writer, server_reader) = server_channel.split();

    let mut registry = CollectorRegistry::new();
    let sd = shutdown::Shutdown::default();
    let handle = sd.handle();
    let telemetry_task = tokio::spawn(async move {
        client_telemetry_loop(client_writer, client_reader, &mut registry, &handle).await;
    });

    sd.trigger();

    let result = tokio::time::timeout(Duration::from_secs(3), telemetry_task).await;
    assert!(
        result.is_ok(),
        "telemetry task should exit when shutdown is cancelled"
    );
    let _ = server_reader;
}
