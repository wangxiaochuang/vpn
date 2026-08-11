#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use async_trait::async_trait;

use super::ConsoleSink;
use super::SinkError;
use super::SinkSource;
use super::TelemetrySink;
use crate::proto::InfoKind;
use crate::proto::InfoSnapshot;
use crate::proto::ProcessSummary;
use crate::proto::TelemetryReport;
use crate::proto::info_snapshot::Payload;

fn sample_source() -> SinkSource {
    SinkSource {
        session_id: 1,
        username: "alice".into(),
        virtual_ip: Some("10.0.0.2".into()),
    }
}

fn sample_report() -> TelemetryReport {
    TelemetryReport {
        ts_ms: 1_700_000_000_000,
        items: vec![InfoSnapshot {
            kind: InfoKind::ProcessSummary as i32,
            payload: Some(Payload::ProcessSummary(ProcessSummary {
                count: 5,
                top_by_cpu: vec![],
            })),
        }],
    }
}

#[tokio::test]
async fn test_console_sink_store_returns_ok_and_is_cloneable_in_arc() {
    let sink = Arc::new(ConsoleSink);
    let result = sink.store(&sample_source(), &sample_report()).await;
    assert!(result.is_ok());
    let cloned = sink.clone();
    let _ = Arc::strong_count(&cloned);
}

#[tokio::test]
async fn test_console_sink_store_without_subscriber_still_returns_ok() {
    let sink = ConsoleSink;
    let result = sink.store(&sample_source(), &sample_report()).await;
    assert!(result.is_ok());
}

fn require_default<T: Default>() -> T {
    T::default()
}

#[tokio::test]
async fn test_console_sink_default_works() {
    let sink: ConsoleSink = require_default();
    let result = sink.store(&sample_source(), &sample_report()).await;
    assert!(result.is_ok());
}

struct FailingSink;

#[async_trait]
impl TelemetrySink for FailingSink {
    async fn store(&self, _: &SinkSource, _: &TelemetryReport) -> Result<(), SinkError> {
        Err(SinkError::Io("mock failure".into()))
    }
}

#[tokio::test]
async fn test_sink_failure_returns_err_without_panic() {
    let sink = FailingSink;
    let result = sink.store(&sample_source(), &sample_report()).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), SinkError::Io(_)));
}

struct RecordingSink {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl TelemetrySink for RecordingSink {
    async fn store(&self, _: &SinkSource, report: &TelemetryReport) -> Result<(), SinkError> {
        self.calls.fetch_add(report.items.len(), Ordering::Relaxed);
        Ok(())
    }
}

#[tokio::test]
async fn test_sink_store_receives_report_reference() {
    let calls = Arc::new(AtomicUsize::new(0));
    let sink = RecordingSink {
        calls: calls.clone(),
    };
    let report = sample_report();
    sink.store(&sample_source(), &report).await.unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}
