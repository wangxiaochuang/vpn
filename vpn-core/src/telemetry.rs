use msgx::channel::Receiver;
use msgx::channel::Sender;
use sysprobe::proto::CollectRequest;
use sysprobe::proto::InfoKind;
use sysprobe::proto::TelemetryMessage;
use sysprobe::proto::telemetry_message::Msg;

pub type TelemetryChannel = msgx::Channel<TelemetryMessage>;
pub type TelemetrySender = Sender<TelemetryMessage>;
pub type TelemetryReceiver = Receiver<TelemetryMessage>;

pub fn report_msg(report: sysprobe::proto::TelemetryReport) -> TelemetryMessage {
    TelemetryMessage {
        msg: Some(Msg::Report(report)),
    }
}

pub fn collect_req_msg(kinds: Vec<InfoKind>) -> TelemetryMessage {
    TelemetryMessage {
        msg: Some(Msg::CollectReq(CollectRequest {
            kinds: kinds.into_iter().map(|k| k as i32).collect(),
        })),
    }
}

pub fn kinds_from_i32(raw: &[i32]) -> Vec<InfoKind> {
    raw.iter()
        .filter_map(|&k| InfoKind::try_from(k).ok())
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("telemetry stream unavailable")]
    StreamUnavailable,
    #[error("failed to send on telemetry stream")]
    SendFailed,
}
