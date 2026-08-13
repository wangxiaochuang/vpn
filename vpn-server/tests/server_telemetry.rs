#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use sysprobe::proto::TelemetryReport;
use sysprobe::sink::SinkError;
use sysprobe::sink::SinkSource;
use sysprobe::sink::TelemetrySink;
use vpn_server::telemetry::TelemetryPlane;

#[derive(Clone, Default)]
struct RecordingSink {
    reports: Arc<Mutex<Vec<TelemetryReport>>>,
}

#[async_trait]
impl TelemetrySink for RecordingSink {
    async fn store(&self, _source: &SinkSource, report: &TelemetryReport) -> Result<(), SinkError> {
        self.reports.lock().unwrap().push(report.clone());
        Ok(())
    }
}

#[derive(Clone, Default)]
struct FailingSink;

#[async_trait]
impl TelemetrySink for FailingSink {
    async fn store(
        &self,
        _source: &SinkSource,
        _report: &TelemetryReport,
    ) -> Result<(), SinkError> {
        Err(SinkError::Backend("always fails".into()))
    }
}

fn sample_report() -> TelemetryReport {
    TelemetryReport {
        ts_ms: 1,
        items: vec![],
    }
}

fn sample_source() -> SinkSource {
    SinkSource {
        session_id: 1,
        username: "alice".into(),
        virtual_ip: None,
    }
}

#[tokio::test]
async fn test_plane_single_sink_failure_does_not_block_other_sink() {
    let ok_sink = RecordingSink::default();
    let plane = TelemetryPlane::new(vec![
        Arc::new(FailingSink) as Arc<dyn TelemetrySink>,
        Arc::new(ok_sink.clone()) as Arc<dyn TelemetrySink>,
    ]);
    let result = plane.store(&sample_source(), &sample_report()).await;
    assert!(result.is_ok());
    assert_eq!(ok_sink.reports.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn test_plane_multiple_sinks_all_receive_report() {
    let sink_a = RecordingSink::default();
    let sink_b = RecordingSink::default();
    let plane = TelemetryPlane::new(vec![
        Arc::new(sink_a.clone()) as Arc<dyn TelemetrySink>,
        Arc::new(sink_b.clone()) as Arc<dyn TelemetrySink>,
    ]);
    plane
        .store(&sample_source(), &sample_report())
        .await
        .unwrap();
    assert_eq!(sink_a.reports.lock().unwrap().len(), 1);
    assert_eq!(sink_b.reports.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn test_plane_always_ok_even_if_all_sinks_fail() {
    let plane = TelemetryPlane::new(vec![
        Arc::new(FailingSink) as Arc<dyn TelemetrySink>,
        Arc::new(FailingSink) as Arc<dyn TelemetrySink>,
    ]);
    let result = plane.store(&sample_source(), &sample_report()).await;
    assert!(result.is_ok(), "fan-out plane never propagates sink errors");
}

#[tokio::test]
async fn test_server_runtime_telemetry_plane_shared_across_arc_clones() {
    let state = common::make_test_state().await;
    let plane_clone: Arc<TelemetryPlane> = state.telemetry.clone();
    assert!(
        Arc::ptr_eq(&state.telemetry, &plane_clone),
        "telemetry plane Arc clones point to same instance"
    );
    assert!(
        state.telemetry.sinks_len() >= 1,
        "default assembly has at least one sink"
    );
    let _ = Duration::from_millis(1);
}
