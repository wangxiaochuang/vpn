#![allow(clippy::unwrap_used, clippy::expect_used, clippy::manual_async_fn)]

mod common;

use std::future::Future;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use bytes::Bytes;
use quic_link::Session;
use shutdown::Shutdown;
use sysprobe::proto::TelemetryMessage;
use vpn_client::client::{ConnectionSupervisor, ExitCause};
use vpn_core::ctrl::control_message::Msg;
use vpn_core::ctrl::{ControlMessage, Heartbeat};

fn hb() -> ControlMessage {
    ControlMessage {
        msg: Some(Msg::Heartbeat(Heartbeat {})),
    }
}

#[derive(Clone)]
struct MockTun {
    mode: Arc<AtomicU8>,
}

const MODE_ERR: u8 = 0;
const MODE_PANIC: u8 = 1;
const MODE_PENDING: u8 = 2;

impl MockTun {
    fn new(mode: u8) -> Self {
        Self {
            mode: Arc::new(AtomicU8::new(mode)),
        }
    }
}

impl vpn_core::data::PacketSource for MockTun {
    fn recv(&mut self) -> impl Future<Output = io::Result<Bytes>> + Send {
        let mode = self.mode.load(Ordering::SeqCst);
        async move {
            match mode {
                MODE_ERR => Err(io::Error::other("mock tun closed")),
                MODE_PANIC => panic!("mock tun recv panicked"),
                _ => Ok(std::future::pending::<Bytes>().await),
            }
        }
    }
}

impl vpn_core::data::PacketSink for MockTun {
    fn send(&mut self, _pkt: Bytes) -> impl Future<Output = io::Result<()>> + Send {
        async { Ok(()) }
    }
}

fn client_session(pair: &common::ConnectionPair) -> Session {
    Session::new(pair.client.clone())
}

/// 打开控制流对，返回 (客户端 channel, 服务端 sender)。
///
/// 服务端 sender 在测试期间保持存活，保证控制流不关闭，避免心跳 task 因流
/// 关闭而提前退出干扰断言。
async fn open_control_pair(
    pair: &common::ConnectionPair,
) -> (
    msgx::Channel<ControlMessage>,
    msgx::channel::Sender<ControlMessage>,
) {
    let (csend, crecv) = pair.client.open_bi().await.unwrap();
    let client_control = msgx::Channel::from_io(msgx::ByteStream::new(crecv, csend));
    let server_conn = pair.server.clone();
    let server_accept = tokio::spawn(async move { server_conn.accept_bi().await });

    let mut client_channel = client_control;
    client_channel.send(hb()).await.unwrap();
    let (ssend, srecv) = server_accept
        .await
        .expect("server accept task")
        .expect("server accept_bi");
    let server_control = msgx::Channel::from_io(msgx::ByteStream::new(srecv, ssend));
    let (server_sender, mut server_receiver) = server_control.split();
    let _ = server_receiver.recv().await.unwrap();
    (client_channel, server_sender)
}

#[tokio::test]
async fn test_supervisor_core_task_end_triggers_teardown() {
    let pair = common::make_connected_pair().await;
    let (channel, server_sender) = open_control_pair(&pair).await;

    let sd = Shutdown::default();
    let supervisor =
        ConnectionSupervisor::spawn(client_session(&pair), MockTun::new(MODE_ERR), channel, &sd);

    let cause = supervisor.run(sd.clone()).await;
    assert_eq!(cause, ExitCause::UplinkEnded);
    let observed = tokio::time::timeout(Duration::from_secs(2), async {
        while pair.server.close_reason().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok();
    assert!(
        observed,
        "server should observe connection close after uplink ends"
    );
    drop(server_sender);
}

#[tokio::test]
async fn test_supervisor_telemetry_end_ignored_and_wait_continues() {
    let pair = common::make_connected_pair().await;
    let (channel, server_sender) = open_control_pair(&pair).await;

    let server_session = Session::new(pair.server.clone());
    let server_accept = tokio::spawn(async move {
        server_session
            .accept_stream::<TelemetryMessage>()
            .await
            .unwrap()
    });

    let sd = Shutdown::default();
    let sd_for_run = sd.clone();
    let supervisor = ConnectionSupervisor::spawn(
        client_session(&pair),
        MockTun::new(MODE_PENDING),
        channel,
        &sd,
    );
    let mut run_task = tokio::spawn(async move { supervisor.run(sd_for_run).await });

    let server_channel = tokio::time::timeout(Duration::from_secs(3), server_accept)
        .await
        .expect("server telemetry accept timeout")
        .unwrap();
    drop(server_channel);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let result = tokio::time::timeout(Duration::from_millis(100), &mut run_task).await;
    assert!(
        result.is_err(),
        "run should ignore TelemetryEnded and keep waiting for core tasks"
    );

    sd.trigger();
    let cause = tokio::time::timeout(Duration::from_secs(3), run_task)
        .await
        .expect("run should resolve after trigger")
        .expect("run task should not panic");
    assert_eq!(cause, ExitCause::Interrupted);
    drop(server_sender);
}

#[tokio::test]
async fn test_supervisor_task_panic_maps_to_task_panicked() {
    let pair = common::make_connected_pair().await;
    let (channel, server_sender) = open_control_pair(&pair).await;

    let sd = Shutdown::default();
    let supervisor = ConnectionSupervisor::spawn(
        client_session(&pair),
        MockTun::new(MODE_PANIC),
        channel,
        &sd,
    );
    let run_task = tokio::spawn(async move { supervisor.run(sd).await });
    let cause = tokio::time::timeout(Duration::from_secs(3), run_task)
        .await
        .expect("run should resolve")
        .expect("run task should not panic");
    assert_eq!(cause, ExitCause::TaskPanicked);
    drop(server_sender);
}
