use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use shutdown::ShutdownHandle;
use sysprobe::proto::InfoKind;
use sysprobe::proto::TelemetryMessage;
use sysprobe::proto::TelemetryReport;
use sysprobe::proto::telemetry_message::Msg;
use sysprobe::sink::SinkError;
use sysprobe::sink::SinkSource;
use sysprobe::sink::TelemetrySink;
use vpn_core::telemetry::TelemetryError;
use vpn_core::telemetry::TelemetryReceiver;
use vpn_core::telemetry::collect_req_msg;

const DEFAULT_PER_SINK_TIMEOUT: Duration = Duration::from_secs(1);

pub type TelemetryTxSlot = Arc<tokio::sync::Mutex<Option<vpn_core::telemetry::TelemetrySender>>>;

pub fn make_telemetry_tx_slot() -> TelemetryTxSlot {
    Arc::new(tokio::sync::Mutex::new(None))
}

pub struct TelemetryPlane {
    sinks: Vec<Arc<dyn TelemetrySink>>,
    per_sink_timeout: Duration,
}

impl TelemetryPlane {
    pub fn new(sinks: Vec<Arc<dyn TelemetrySink>>) -> Self {
        Self {
            sinks,
            per_sink_timeout: DEFAULT_PER_SINK_TIMEOUT,
        }
    }

    pub fn with_timeout(sinks: Vec<Arc<dyn TelemetrySink>>, timeout: Duration) -> Self {
        Self {
            sinks,
            per_sink_timeout: timeout,
        }
    }

    pub fn sinks_len(&self) -> usize {
        self.sinks.len()
    }
}

#[async_trait]
impl TelemetrySink for TelemetryPlane {
    async fn store(&self, source: &SinkSource, report: &TelemetryReport) -> Result<(), SinkError> {
        for sink in &self.sinks {
            deliver_to_sink(sink, source, report, self.per_sink_timeout).await;
        }
        Ok(())
    }
}

async fn deliver_to_sink(
    sink: &Arc<dyn TelemetrySink>,
    source: &SinkSource,
    report: &TelemetryReport,
    timeout: Duration,
) {
    match tokio::time::timeout(timeout, sink.store(source, report)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("telemetry sink error, skipping: {e}"),
        Err(_) => tracing::warn!("telemetry sink timed out after {timeout:?}, skipping"),
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod plane_tests {
    use std::sync::Mutex;
    use std::time::Instant;

    use super::*;

    #[derive(Clone, Default)]
    struct RecordingSink {
        reports: Arc<Mutex<Vec<TelemetryReport>>>,
    }

    #[async_trait]
    impl TelemetrySink for RecordingSink {
        async fn store(
            &self,
            _source: &SinkSource,
            report: &TelemetryReport,
        ) -> Result<(), SinkError> {
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

    #[derive(Clone, Default)]
    struct SlowSink {
        recorded: Arc<Mutex<Vec<TelemetryReport>>>,
    }

    #[async_trait]
    impl TelemetrySink for SlowSink {
        async fn store(
            &self,
            _source: &SinkSource,
            report: &TelemetryReport,
        ) -> Result<(), SinkError> {
            tokio::time::sleep(Duration::from_secs(5)).await;
            self.recorded.lock().unwrap().push(report.clone());
            Ok(())
        }
    }

    fn sample_report() -> TelemetryReport {
        TelemetryReport {
            ts_ms: 42,
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
    async fn test_plane_fans_out_to_all_sinks() {
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
    async fn test_plane_single_sink_error_does_not_block_others() {
        let ok_sink = RecordingSink::default();
        let plane = TelemetryPlane::new(vec![
            Arc::new(FailingSink) as Arc<dyn TelemetrySink>,
            Arc::new(ok_sink.clone()) as Arc<dyn TelemetrySink>,
        ]);
        let result = plane.store(&sample_source(), &sample_report()).await;
        assert!(result.is_ok(), "plane always returns Ok for fan-out");
        assert_eq!(
            ok_sink.reports.lock().unwrap().len(),
            1,
            "ok sink still receives report despite sibling failure"
        );
    }

    #[tokio::test]
    async fn test_plane_slow_sink_times_out_and_skips() {
        let slow = SlowSink::default();
        let ok_sink = RecordingSink::default();
        let plane = TelemetryPlane::with_timeout(
            vec![
                Arc::new(slow.clone()) as Arc<dyn TelemetrySink>,
                Arc::new(ok_sink.clone()) as Arc<dyn TelemetrySink>,
            ],
            Duration::from_millis(100),
        );
        store_within(plane, Duration::from_secs(2)).await;
        assert!(
            slow.recorded.lock().unwrap().is_empty(),
            "slow sink should have been skipped (timed out)"
        );
        assert_eq!(
            ok_sink.reports.lock().unwrap().len(),
            1,
            "ok sink still receives report after sibling timeout"
        );
    }

    async fn store_within(plane: TelemetryPlane, limit: Duration) {
        let start = Instant::now();
        plane
            .store(&sample_source(), &sample_report())
            .await
            .unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < limit,
            "plane must not wait for slow sink; elapsed {elapsed:?}"
        );
    }
}
