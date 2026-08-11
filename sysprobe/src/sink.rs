use async_trait::async_trait;
use thiserror::Error;

use crate::proto::InfoKind;
use crate::proto::TelemetryReport;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkSource {
    pub session_id: u64,
    pub username: String,
    pub virtual_ip: Option<String>,
}

#[derive(Debug, Error)]
pub enum SinkError {
    #[error("I/O error in telemetry sink: {0}")]
    Io(String),
    #[error("backend error in telemetry sink: {0}")]
    Backend(String),
}

#[async_trait]
pub trait TelemetrySink: Send + Sync {
    async fn store(&self, source: &SinkSource, report: &TelemetryReport) -> Result<(), SinkError>;
}

#[derive(Clone, Default)]
pub struct ConsoleSink;

#[async_trait]
impl TelemetrySink for ConsoleSink {
    async fn store(&self, source: &SinkSource, report: &TelemetryReport) -> Result<(), SinkError> {
        for item in &report.items {
            let kind = InfoKind::try_from(item.kind).map_or("UNKNOWN", |k| k.as_str_name());
            tracing::info!(
                session_id = source.session_id,
                username = %source.username,
                kind = kind,
                ts_ms = report.ts_ms,
                "telemetry report received"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
