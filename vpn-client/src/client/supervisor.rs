use msgx::Channel;
use msgx::channel::{Receiver, Sender};
use quic_link::KeepaliveConfig;
use quic_link::LoopControl;
use quic_link::Session;
use quic_link::keepalive_loop;
use shutdown::Shutdown;
use shutdown::ShutdownHandle;
use vpn_core::data::{PacketSink, PacketSource, forward};
use vpn_core::vpn::ControlMessage;
use vpn_core::vpn::control_message::Msg;

/// 连接 supervisor 各 task 的结束原因（"遗言"契约）。纯枚举，不携带错误信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCause {
    Interrupted,
    ServerDisconnect,
    HeartbeatEnded,
    UplinkEnded,
    DownlinkEnded,
    TelemetryEnded,
    TaskPanicked,
}

impl ExitCause {
    pub const ALL: [Self; 7] = [
        Self::Interrupted,
        Self::ServerDisconnect,
        Self::HeartbeatEnded,
        Self::UplinkEnded,
        Self::DownlinkEnded,
        Self::TelemetryEnded,
        Self::TaskPanicked,
    ];

    pub fn code(self) -> u64 {
        match self {
            Self::UplinkEnded | Self::DownlinkEnded => 0x1,
            Self::TaskPanicked => 0x2,
            Self::Interrupted
            | Self::ServerDisconnect
            | Self::HeartbeatEnded
            | Self::TelemetryEnded => 0,
        }
    }

    pub fn reason(self) -> &'static [u8] {
        match self {
            Self::Interrupted => b"client-shutdown",
            Self::ServerDisconnect => b"server-disconnect",
            Self::HeartbeatEnded => b"heartbeat-timeout",
            Self::UplinkEnded => b"uplink-ended",
            Self::DownlinkEnded => b"downlink-ended",
            Self::TelemetryEnded => b"telemetry-ended",
            Self::TaskPanicked => b"data-plane-panic",
        }
    }
}

impl std::fmt::Display for ExitCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interrupted => write!(f, "interrupted"),
            Self::ServerDisconnect => write!(f, "server-disconnect"),
            Self::HeartbeatEnded => write!(f, "heartbeat-ended"),
            Self::UplinkEnded => write!(f, "uplink-ended"),
            Self::DownlinkEnded => write!(f, "downlink-ended"),
            Self::TelemetryEnded => write!(f, "telemetry-ended"),
            Self::TaskPanicked => write!(f, "task-panicked"),
        }
    }
}

pub async fn heartbeat_loop(
    session: Session,
    mut reader: Receiver<ControlMessage>,
    mut writer: Sender<ControlMessage>,
    shutdown: ShutdownHandle,
) -> ExitCause {
    let mut saw_disconnect = false;
    run_keepalive(
        &session,
        &mut reader,
        &mut writer,
        &shutdown,
        &mut saw_disconnect,
    )
    .await;
    resolve_cause(saw_disconnect, &shutdown)
}

async fn run_keepalive(
    session: &Session,
    reader: &mut Receiver<ControlMessage>,
    writer: &mut Sender<ControlMessage>,
    shutdown: &ShutdownHandle,
    saw_disconnect: &mut bool,
) {
    let hb = || ControlMessage {
        msg: Some(Msg::Heartbeat(vpn_core::vpn::Heartbeat {})),
    };
    keepalive_loop(
        session,
        writer,
        reader,
        shutdown,
        KeepaliveConfig::default(),
        hb,
        disconnect_handler(saw_disconnect),
    )
    .await;
}

fn disconnect_handler(
    saw_disconnect: &mut bool,
) -> impl FnMut(&ControlMessage) -> LoopControl + '_ {
    move |m| {
        if matches!(m.msg, Some(Msg::Disconnect(_))) {
            tracing::info!("server disconnected");
            *saw_disconnect = true;
            LoopControl::Break
        } else {
            LoopControl::Continue
        }
    }
}

fn resolve_cause(saw_disconnect: bool, shutdown: &ShutdownHandle) -> ExitCause {
    if saw_disconnect {
        ExitCause::ServerDisconnect
    } else if shutdown.is_cancelled() {
        ExitCause::Interrupted
    } else {
        ExitCause::HeartbeatEnded
    }
}

/// 每连接 supervisor：集中 spawn 心跳/上行/下行/遥测 task，并统一关闭协调。
pub struct ConnectionSupervisor<S> {
    session: Session,
    tasks: tokio::task::JoinSet<ExitCause>,
    _tun: std::marker::PhantomData<S>,
}

impl<S> ConnectionSupervisor<S>
where
    S: PacketSource + PacketSink + Clone + Unpin + Send + Sync + 'static,
{
    pub fn spawn(
        session: Session,
        tun: S,
        channel: Channel<ControlMessage>,
        sd: &Shutdown,
    ) -> Self {
        let (writer, reader) = channel.split();
        let mut tasks: tokio::task::JoinSet<ExitCause> = tokio::task::JoinSet::new();
        spawn_heartbeat(&mut tasks, session.clone(), reader, writer, sd);
        spawn_uplink_task(&mut tasks, session.clone(), tun.clone(), sd);
        spawn_downlink_task(&mut tasks, session.clone(), tun, sd);
        spawn_telemetry_task(&mut tasks, session.clone(), sd);
        Self {
            session,
            tasks,
            _tun: std::marker::PhantomData,
        }
    }

    pub async fn run(mut self, sd: Shutdown) -> ExitCause {
        let cause = self.await_cause(&sd).await;
        self.session.close(cause.code(), cause.reason());
        sd.trigger();
        sd.drain(&mut self.tasks, "client").await;
        cause
    }

    async fn await_cause(&mut self, sd: &Shutdown) -> ExitCause {
        loop {
            let cause = tokio::select! {
                biased;
                () = sd.triggered() => ExitCause::Interrupted,
                Some(r) = self.tasks.join_next() => match r {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("data plane task panicked: {e}");
                        ExitCause::TaskPanicked
                    }
                },
            };
            if cause != ExitCause::TelemetryEnded {
                return cause;
            }
        }
    }
}

async fn uplink<S>(session: Session, tun: S, cancel: ShutdownHandle) -> ExitCause
where
    S: PacketSource + Unpin,
{
    let mut source = tun;
    let mut sink = session.datagram_tx();
    match forward(&mut source, &mut sink, &cancel).await {
        Ok(()) => ExitCause::UplinkEnded,
        Err(e) => {
            tracing::warn!("uplink ended with error: {e}");
            ExitCause::UplinkEnded
        }
    }
}

fn spawn_heartbeat(
    tasks: &mut tokio::task::JoinSet<ExitCause>,
    session: Session,
    reader: Receiver<ControlMessage>,
    writer: Sender<ControlMessage>,
    sd: &Shutdown,
) {
    let handle = sd.handle();
    tasks.spawn(async move { heartbeat_loop(session, reader, writer, handle).await });
}

fn spawn_uplink_task<S>(
    tasks: &mut tokio::task::JoinSet<ExitCause>,
    session: Session,
    tun: S,
    sd: &Shutdown,
) where
    S: PacketSource + PacketSink + Clone + Unpin + Send + Sync + 'static,
{
    let handle = sd.handle();
    tasks.spawn(async move { uplink(session, tun, handle).await });
}

fn spawn_downlink_task<S>(
    tasks: &mut tokio::task::JoinSet<ExitCause>,
    session: Session,
    tun: S,
    sd: &Shutdown,
) where
    S: PacketSource + PacketSink + Clone + Unpin + Send + Sync + 'static,
{
    let handle = sd.handle();
    tasks.spawn(async move { downlink(session, tun, handle).await });
}

fn spawn_telemetry_task(
    tasks: &mut tokio::task::JoinSet<ExitCause>,
    session: Session,
    sd: &Shutdown,
) {
    let handle = sd.handle();
    let fut = async move {
        crate::telemetry::run_client_telemetry(session, handle).await;
    };
    tasks.spawn(guarded_telemetry(fut));
}

async fn guarded_telemetry<F: Future<Output = ()>>(fut: F) -> ExitCause {
    if let Err(panic) = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(fut)).await {
        tracing::error!("telemetry task panicked: {panic:?}");
    }
    ExitCause::TelemetryEnded
}

async fn downlink<S>(session: Session, tun: S, cancel: ShutdownHandle) -> ExitCause
where
    S: PacketSink + Unpin,
{
    let mut source = session.datagram_rx();
    let mut sink = tun;
    match forward(&mut source, &mut sink, &cancel).await {
        Ok(()) => ExitCause::DownlinkEnded,
        Err(e) => {
            tracing::warn!("downlink ended with error: {e}");
            ExitCause::DownlinkEnded
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_cause_code_reason_mapping() {
        let cases = [
            (ExitCause::Interrupted, 0, "client-shutdown"),
            (ExitCause::ServerDisconnect, 0, "server-disconnect"),
            (ExitCause::HeartbeatEnded, 0, "heartbeat-timeout"),
            (ExitCause::UplinkEnded, 0x1, "uplink-ended"),
            (ExitCause::DownlinkEnded, 0x1, "downlink-ended"),
            (ExitCause::TelemetryEnded, 0, "telemetry-ended"),
            (ExitCause::TaskPanicked, 0x2, "data-plane-panic"),
        ];
        for (cause, code, reason) in cases {
            assert_eq!(cause.code(), code, "{cause:?}");
            assert_eq!(cause.reason(), reason.as_bytes(), "{cause:?}");
        }
    }

    fn assert_displays_unique(all: &[String]) {
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "duplicate at {i},{j}");
            }
        }
    }

    #[test]
    fn test_exit_cause_displays_are_distinct() {
        let all: Vec<String> = ExitCause::ALL.iter().map(ToString::to_string).collect();
        assert_displays_unique(&all);
    }

    #[test]
    fn test_exit_cause_is_copy_and_eq() {
        let a = ExitCause::HeartbeatEnded;
        let b = a;
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn test_guarded_telemetry_normalizes_panic() {
        let cause = guarded_telemetry(async { panic!("telemetry boom") }).await;
        assert_eq!(cause, ExitCause::TelemetryEnded);
    }

    #[tokio::test]
    async fn test_guarded_telemetry_normal_exit() {
        let cause = guarded_telemetry(async {}).await;
        assert_eq!(cause, ExitCause::TelemetryEnded);
    }
}
