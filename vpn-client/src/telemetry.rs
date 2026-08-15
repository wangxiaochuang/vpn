use std::time::Duration;
use std::time::Instant;

use quic_link::Session;
use shutdown::ShutdownHandle;
use sysprobe::collector::CollectorRegistry;
use sysprobe::collectors::DiskCollector;
use sysprobe::collectors::NetifCollector;
use sysprobe::collectors::PortCollector;
use sysprobe::collectors::ProcessFullCollector;
use sysprobe::collectors::ProcessSummaryCollector;
use sysprobe::proto::TelemetryMessage;
use sysprobe::proto::telemetry_message::Msg;
use vpn_core::telemetry::TelemetryChannel;
use vpn_core::telemetry::TelemetryReceiver;
use vpn_core::telemetry::TelemetrySender;
use vpn_core::telemetry::kinds_from_i32;
use vpn_core::telemetry::report_msg;

pub fn build_default_registry() -> CollectorRegistry {
    let mut reg = CollectorRegistry::new();
    reg.register(Box::new(ProcessSummaryCollector::new()));
    reg.register(Box::new(ProcessFullCollector::new()));
    reg.register(Box::new(PortCollector::new()));
    reg.register(Box::new(NetifCollector::new()));
    reg.register(Box::new(DiskCollector::new()));
    reg
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

const PUSH_TICK: Duration = Duration::from_secs(1);

pub async fn run_client_telemetry(
    session: Session,
    shutdown: ShutdownHandle,
) -> crate::client::ExitCause {
    let Some(channel) = open_telemetry_stream(&session).await else {
        return crate::client::ExitCause::TelemetryEnded;
    };
    let (mut writer, reader) = channel.split();
    let mut registry = build_default_registry();
    let _ = writer
        .send(report_msg(sysprobe::proto::TelemetryReport {
            ts_ms: 0,
            items: vec![],
        }))
        .await;
    client_telemetry_loop(writer, reader, &mut registry, &shutdown).await;
    crate::client::ExitCause::TelemetryEnded
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
