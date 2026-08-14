use std::sync::Arc;

use super::conn::{AuthStore, ClientNetProfile, ConnExitCause, ConnectionHandle};
use super::handshake::try_authenticate;
use crate::ledger::ConnectionLedger;
use crate::telemetry::TelemetryPlane;
use crate::telemetry::TelemetrySender;
use crate::telemetry::TelemetryTxSlot;
use msgx::Channel;
use msgx::channel::{Receiver, Sender};
use quic_link::{KeepaliveConfig, LoopControl, PacketSink, Session, forward, keepalive_loop};
use shutdown::Shutdown;
use shutdown::ShutdownHandle;
use sysprobe::proto::TelemetryMessage;
use sysprobe::sink::SinkSource;
use vpn_core::vpn::control_message::Msg;
use vpn_core::vpn::{ControlMessage, Disconnect, Heartbeat};

const TELEMETRY_ACCEPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub async fn handle_conn<S: PacketSink + Unpin + Send + 'static>(
    session: Session,
    auth: Arc<AuthStore>,
    ledger: Arc<ConnectionLedger<ConnectionHandle>>,
    net_profile: Arc<ClientNetProfile>,
    telemetry: Arc<TelemetryPlane>,
    uplink_sink: S,
    sd: ShutdownHandle,
) -> ConnExitCause {
    let Some((handle, username, channel)) =
        try_authenticate(&session, &auth, &ledger, &net_profile).await
    else {
        return ConnExitCause::CtrlEnded;
    };
    let supervisor = ConnectionSupervisor::spawn(
        handle,
        ledger,
        telemetry,
        uplink_sink,
        channel,
        username,
        &sd,
    );
    let cause = supervisor.run(&sd).await;
    tracing::info!("connection exited: {cause}");
    cause
}

/// 每连接 supervisor：集中 spawn ctrl/uplink/telemetry 三个 task，
/// 统一"等待结束信号 → 决定退出原因 → close → drain → cleanup"。
pub struct ConnectionSupervisor {
    session: Session,
    handle: ConnectionHandle,
    ledger: Arc<ConnectionLedger<ConnectionHandle>>,
    tasks: tokio::task::JoinSet<ConnExitCause>,
    drain_sd: Shutdown,
}

impl ConnectionSupervisor {
    pub fn spawn<S: PacketSink + Unpin + Send + 'static>(
        handle: ConnectionHandle,
        ledger: Arc<ConnectionLedger<ConnectionHandle>>,
        telemetry: Arc<TelemetryPlane>,
        uplink_sink: S,
        channel: Channel<ControlMessage>,
        username: String,
        sd: &ShutdownHandle,
    ) -> Self {
        let mut tasks: tokio::task::JoinSet<ConnExitCause> = tokio::task::JoinSet::new();
        let (sender, receiver) = channel.split();
        let session = handle.session.clone();
        let telemetry_tx = handle.telemetry_tx.clone();
        spawn_ctrl_task(&mut tasks, session.clone(), sender, receiver, sd); // 控制面: 心跳保活
        spawn_uplink_task(&mut tasks, uplink_sink, session.clone(), sd); // 数据面上行: datagram → TUN
        spawn_telemetry_task(&mut tasks, session, telemetry, username, telemetry_tx, sd); // 采集面: telemetry
        Self {
            session: handle.session.clone(),
            handle,
            ledger,
            tasks,
            drain_sd: Shutdown::new(Shutdown::DEFAULT_DRAIN_TIMEOUT),
        }
    }

    pub async fn run(mut self, global_sd: &ShutdownHandle) -> ConnExitCause {
        let cause = self.await_cause(global_sd).await;
        let close_after_drain = cause == ConnExitCause::ServerShutdown;
        if !close_after_drain {
            self.session.close(cause.code(), cause.reason());
        }
        self.drain_sd.drain(&mut self.tasks, "conn").await;
        if close_after_drain {
            self.session.close(cause.code(), cause.reason());
        }
        let reserved = self
            .handle
            .retire_slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.ledger.retire(&self.handle, reserved);
        cause
    }

    async fn await_cause(&mut self, global_sd: &ShutdownHandle) -> ConnExitCause {
        loop {
            // cancel-safety: global_sd.cancelled() 和 tasks.join_next() 均 cancel-safe（tokio 文档）。
            let cause = tokio::select! {
                biased;
                () = global_sd.cancelled() => ConnExitCause::ServerShutdown,
                Some(r) = self.tasks.join_next() => match r {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("conn task panicked: {e}");
                        ConnExitCause::TaskPanicked
                    }
                },
            };
            if cause != ConnExitCause::TelemetryEnded {
                return cause;
            }
        }
    }
}

fn spawn_ctrl_task(
    tasks: &mut tokio::task::JoinSet<ConnExitCause>,
    session: Session,
    sender: Sender<ControlMessage>,
    receiver: Receiver<ControlMessage>,
    sd: &ShutdownHandle,
) {
    let sd = sd.clone();
    tasks.spawn(async move { ctrl_task(session, sender, receiver, sd).await });
}

pub fn spawn_uplink_task<S: PacketSink + Unpin + Send + 'static>(
    tasks: &mut tokio::task::JoinSet<ConnExitCause>,
    sink: S,
    session: Session,
    sd: &ShutdownHandle,
) {
    let sd = sd.clone();
    tasks.spawn(async move { uplink_task(sink, session, sd).await });
}

fn spawn_telemetry_task(
    tasks: &mut tokio::task::JoinSet<ConnExitCause>,
    session: Session,
    telemetry: Arc<TelemetryPlane>,
    username: String,
    telemetry_tx: TelemetryTxSlot,
    sd: &ShutdownHandle,
) {
    let sd = sd.clone();
    tasks
        .spawn(async move { telemetry_task(session, telemetry, username, telemetry_tx, sd).await });
}

async fn ctrl_task(
    session: Session,
    mut writer: Sender<ControlMessage>,
    mut reader: Receiver<ControlMessage>,
    shutdown: ShutdownHandle,
) -> ConnExitCause {
    let hb = || ControlMessage {
        msg: Some(Msg::Heartbeat(Heartbeat {})),
    };
    keepalive_loop(
        &session,
        &mut writer,
        &mut reader,
        &shutdown,
        KeepaliveConfig::default(),
        hb,
        |_| LoopControl::Continue,
    )
    .await;
    send_disconnect_on_shutdown(&shutdown, &mut writer).await;
    ConnExitCause::CtrlEnded
}

async fn send_disconnect_on_shutdown(
    shutdown: &ShutdownHandle,
    writer: &mut Sender<ControlMessage>,
) {
    if shutdown.is_cancelled() {
        let _ = writer.send(server_disconnect_msg()).await;
    }
}

async fn uplink_task<S: PacketSink + Unpin + Send>(
    mut sink: S,
    session: Session,
    shutdown: ShutdownHandle,
) -> ConnExitCause {
    let mut source = session.datagram_rx();
    match forward(&mut source, &mut sink, &shutdown).await {
        Ok(()) => {}
        Err(e) => tracing::warn!("uplink ended with error: {e}"),
    }
    ConnExitCause::UplinkEnded
}

async fn telemetry_task(
    session: Session,
    telemetry: Arc<TelemetryPlane>,
    username: String,
    telemetry_tx: TelemetryTxSlot,
    shutdown: ShutdownHandle,
) -> ConnExitCause {
    let Some(channel) = accept_telemetry_channel(&session, &shutdown).await else {
        return ConnExitCause::TelemetryEnded;
    };
    let (writer, reader) = channel.split();
    set_telemetry_sender(&telemetry_tx, writer).await;
    let source = build_sink_source(&session, &username);
    crate::telemetry::server_telemetry_loop(reader, telemetry, source, shutdown).await;
    ConnExitCause::TelemetryEnded
}

async fn accept_telemetry_channel(
    session: &Session,
    shutdown: &ShutdownHandle,
) -> Option<Channel<TelemetryMessage>> {
    // cancel-safety: shutdown.cancelled() 与 timeout+accept_stream 均 cancel-safe。
    tokio::select! {
        biased;
        () = shutdown.cancelled() => None,
        result = tokio::time::timeout(
            TELEMETRY_ACCEPT_TIMEOUT,
            session.accept_stream::<TelemetryMessage>(),
        ) => if let Ok(Ok(ch)) = result {
            Some(ch)
        } else {
            tracing::debug!("telemetry stream not opened within timeout, skipping");
            None
        },
    }
}

fn build_sink_source(session: &Session, username: &str) -> SinkSource {
    SinkSource {
        session_id: session.id() as u64,
        username: username.to_string(),
        virtual_ip: None,
    }
}

async fn set_telemetry_sender(slot: &TelemetryTxSlot, sender: TelemetrySender) {
    *slot.lock().await = Some(sender);
}

fn server_disconnect_msg() -> ControlMessage {
    ControlMessage {
        msg: Some(Msg::Disconnect(Disconnect {
            reason: "server-shutdown".to_string(),
        })),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::mutable_key_type,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_graceful_stop_drains_conns_before_daemon() {
        let sd = Shutdown::default();
        let mut conn_set: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
        let mut daemon_set: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
        let log: Arc<std::sync::Mutex<Vec<u8>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        spawn_order_probe(&mut conn_set, sd.handle(), log.clone(), 1);
        spawn_order_probe(&mut daemon_set, sd.handle(), log.clone(), 2);
        sd.trigger();
        sd.drain(&mut conn_set, "conn").await;
        sd.drain(&mut daemon_set, "daemon").await;
        let recorded = log.lock().unwrap();
        assert_eq!(*recorded, vec![1u8, 2u8], "conns must drain before daemon");
    }

    fn spawn_order_probe(
        tasks: &mut tokio::task::JoinSet<()>,
        sd: ShutdownHandle,
        log: Arc<std::sync::Mutex<Vec<u8>>>,
        tag: u8,
    ) {
        tasks.spawn(async move {
            sd.cancelled().await;
            log.lock().unwrap().push(tag);
        });
    }
}
