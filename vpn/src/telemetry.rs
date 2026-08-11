use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use msgx::channel::Receiver;
use msgx::channel::Sender;
use sysprobe::collector::CollectorRegistry;
use sysprobe::collectors::DiskCollector;
use sysprobe::collectors::NetifCollector;
use sysprobe::collectors::PortCollector;
use sysprobe::collectors::ProcessFullCollector;
use sysprobe::collectors::ProcessSummaryCollector;
use sysprobe::proto::CollectRequest;
use sysprobe::proto::InfoKind;
use sysprobe::proto::TelemetryMessage;
use sysprobe::proto::TelemetryReport;
use sysprobe::proto::telemetry_message::Msg;
use sysprobe::sink::SinkSource;
use sysprobe::sink::TelemetrySink;

use quic_link::Session;
use shutdown::ShutdownHandle;

const PUSH_TICK: Duration = Duration::from_secs(1);

pub type TelemetryChannel = msgx::Channel<TelemetryMessage>;
pub type TelemetrySender = Sender<TelemetryMessage>;
pub type TelemetryReceiver = Receiver<TelemetryMessage>;
pub type TelemetryTxSlot = Arc<tokio::sync::Mutex<Option<TelemetrySender>>>;

pub fn build_default_registry() -> CollectorRegistry {
    let mut reg = CollectorRegistry::new();
    reg.register(Box::new(ProcessSummaryCollector::new()));
    reg.register(Box::new(ProcessFullCollector::new()));
    reg.register(Box::new(PortCollector::new()));
    reg.register(Box::new(NetifCollector::new()));
    reg.register(Box::new(DiskCollector::new()));
    reg
}

pub fn make_telemetry_tx_slot() -> TelemetryTxSlot {
    Arc::new(tokio::sync::Mutex::new(None))
}

fn report_msg(report: TelemetryReport) -> TelemetryMessage {
    TelemetryMessage {
        msg: Some(Msg::Report(report)),
    }
}

fn collect_req_msg(kinds: Vec<InfoKind>) -> TelemetryMessage {
    TelemetryMessage {
        msg: Some(Msg::CollectReq(CollectRequest {
            kinds: kinds.into_iter().map(|k| k as i32).collect(),
        })),
    }
}

fn kinds_from_i32(raw: &[i32]) -> Vec<InfoKind> {
    raw.iter()
        .filter_map(|&k| InfoKind::try_from(k).ok())
        .collect()
}

pub async fn open_telemetry_stream(session: &Session) -> Option<TelemetryChannel> {
    match session.open_stream::<TelemetryMessage>().await {
        Ok(ch) => Some(ch),
        Err(e) => {
            tracing::warn!("failed to open telemetry stream: {e}");
            None
        }
    }
}

pub async fn run_client_telemetry(session: Session, shutdown: ShutdownHandle) {
    let Some(channel) = open_telemetry_stream(&session).await else {
        return;
    };
    let (mut writer, reader) = channel.split();
    let mut registry = build_default_registry();
    let _ = writer
        .send(report_msg(TelemetryReport {
            ts_ms: 0,
            items: vec![],
        }))
        .await;
    client_telemetry_loop(writer, reader, &mut registry, &shutdown).await;
}

pub async fn client_telemetry_loop(
    mut writer: TelemetrySender,
    mut reader: TelemetryReceiver,
    registry: &mut CollectorRegistry,
    shutdown: &ShutdownHandle,
) {
    let mut interval = tokio::time::interval(PUSH_TICK);
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            _ = interval.tick() => handle_push_tick(&mut writer, registry).await,
            msg = reader.recv() => {
                if !handle_pull_msg(&mut writer, registry, msg).await {
                    break;
                }
            }
        }
    }
}

async fn handle_push_tick(writer: &mut TelemetrySender, registry: &mut CollectorRegistry) {
    let now = Instant::now();
    let due = registry.push_due(now);
    if due.is_empty() {
        return;
    }
    let report = registry.collect_by_kinds(&due).await;
    for k in &due {
        registry.mark_pushed(*k, now);
    }
    let _ = writer.send(report_msg(report)).await;
}

async fn handle_pull_msg(
    writer: &mut TelemetrySender,
    registry: &mut CollectorRegistry,
    msg: Result<Option<TelemetryMessage>, msgx::RecvError>,
) -> bool {
    match msg {
        Ok(Some(m)) => match m.msg {
            Some(Msg::CollectReq(req)) => {
                let kinds = kinds_from_i32(&req.kinds);
                let report = registry.collect_by_kinds(&kinds).await;
                let _ = writer.send(report_msg(report)).await;
                true
            }
            _ => false,
        },
        _ => false,
    }
}

pub async fn server_telemetry_loop(
    mut reader: TelemetryReceiver,
    sink: Arc<dyn TelemetrySink>,
    source: SinkSource,
    shutdown: ShutdownHandle,
) {
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            msg = reader.recv() => {
                if !handle_server_msg(&sink, &source, msg).await {
                    break;
                }
            }
        }
    }
}

async fn handle_server_msg(
    sink: &Arc<dyn TelemetrySink>,
    source: &SinkSource,
    msg: Result<Option<TelemetryMessage>, msgx::RecvError>,
) -> bool {
    match msg {
        Ok(Some(m)) => match m.msg {
            Some(Msg::Report(report)) => {
                if let Err(e) = sink.store(source, &report).await {
                    tracing::warn!("telemetry sink error: {e}");
                }
                true
            }
            Some(Msg::CollectReq(_)) => {
                tracing::warn!("received unexpected collect_req from client, ignoring");
                true
            }
            None => false,
        },
        _ => false,
    }
}

pub async fn request_collect(
    slot: &TelemetryTxSlot,
    kinds: Vec<InfoKind>,
) -> Result<(), TelemetryError> {
    let mut guard = slot.lock().await;
    let Some(sender) = guard.as_mut() else {
        return Err(TelemetryError::StreamUnavailable);
    };
    sender
        .send(collect_req_msg(kinds))
        .await
        .map_err(|_| TelemetryError::SendFailed)
}

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("telemetry stream unavailable")]
    StreamUnavailable,
    #[error("failed to send on telemetry stream")]
    SendFailed,
}
