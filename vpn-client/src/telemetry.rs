use std::time::Duration;
use std::time::Instant;

use shutdown::ShutdownHandle;
use sysprobe::collector::CollectorRegistry;
use sysprobe::proto::TelemetryMessage;
use sysprobe::proto::telemetry_message::Msg;

pub use vpn_core::telemetry::TelemetryChannel;
pub use vpn_core::telemetry::TelemetryError;
pub use vpn_core::telemetry::TelemetryPlane;
pub use vpn_core::telemetry::TelemetryReceiver;
pub use vpn_core::telemetry::TelemetrySender;
pub use vpn_core::telemetry::TelemetryTxSlot;
pub use vpn_core::telemetry::build_default_registry;
pub use vpn_core::telemetry::make_telemetry_tx_slot;
pub use vpn_core::telemetry::open_telemetry_stream;

use quic_link::Session;
use vpn_core::telemetry::{kinds_from_i32, report_msg};

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
