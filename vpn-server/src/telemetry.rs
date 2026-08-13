use std::sync::Arc;
use std::time::Duration;

use shutdown::ShutdownHandle;
use sysprobe::proto::InfoKind;
use sysprobe::proto::TelemetryMessage;
use sysprobe::proto::telemetry_message::Msg;
use sysprobe::sink::SinkSource;
use sysprobe::sink::TelemetrySink;

pub use vpn_core::telemetry::TelemetryChannel;
pub use vpn_core::telemetry::TelemetryError;
pub use vpn_core::telemetry::TelemetryPlane;
pub use vpn_core::telemetry::TelemetryReceiver;
pub use vpn_core::telemetry::TelemetrySender;
pub use vpn_core::telemetry::TelemetryTxSlot;
pub use vpn_core::telemetry::build_default_registry;
pub use vpn_core::telemetry::collect_req_msg;
pub use vpn_core::telemetry::make_telemetry_tx_slot;
pub use vpn_core::telemetry::report_msg;

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

#[allow(dead_code)]
const _DEFAULT_PER_SINK_TIMEOUT: Duration = Duration::from_secs(1);
