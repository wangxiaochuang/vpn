#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use ipnet::Ipv4Net;
use msgx::Channel;
use msgx::channel::ByteStream;
use sysprobe::proto::InfoKind;
use sysprobe::proto::InfoSnapshot;
use sysprobe::proto::ProcessSummary;
use sysprobe::proto::TelemetryMessage;
use sysprobe::proto::TelemetryReport;
use sysprobe::proto::info_snapshot::Payload;
use sysprobe::proto::telemetry_message::Msg;
use sysprobe::sink::SinkError;
use sysprobe::sink::SinkSource;
use sysprobe::sink::TelemetrySink;
use vpn_core::framing::ControlCodec;

type CapturedReport = (SinkSource, TelemetryReport);

#[derive(Clone, Default)]
struct RecordingSink {
    reports: Arc<Mutex<Vec<CapturedReport>>>,
}

#[async_trait]
impl TelemetrySink for RecordingSink {
    async fn store(&self, source: &SinkSource, report: &TelemetryReport) -> Result<(), SinkError> {
        self.reports
            .lock()
            .unwrap()
            .push((source.clone(), report.clone()));
        Ok(())
    }
}

async fn auth_and_get_telemetry_writer(
    endpoint: &quinn::Endpoint,
) -> msgx::channel::Sender<TelemetryMessage> {
    let conn = common::test_client_connect(endpoint.local_addr().unwrap()).await;
    let (send, recv) = conn.open_bi().await.unwrap();
    let mut ctrl = tokio_util::codec::Framed::new(
        quic_link::quinn_stream::QuinnStream::new(send, recv),
        ControlCodec::new(),
    );
    common::send_auth_request(&mut ctrl, "alice", common::ALICE_PASSWORD).await;
    let _ = common::recv_control(&mut ctrl).await.expect("AuthOk");

    let (tsend, trecv) = conn.open_bi().await.unwrap();
    let tch = Channel::<TelemetryMessage>::from_io(ByteStream::new(trecv, tsend));
    let (writer, _reader) = tch.split();
    writer
}

#[tokio::test]
async fn test_server_ingests_telemetry_report_to_sink() {
    let sink = RecordingSink::default();
    let state = common::test_state_with_sink(
        Ipv4Net::new(std::net::Ipv4Addr::new(10, 0, 0, 0), 24).unwrap(),
        common::alice_users(),
        Arc::new(sink.clone()) as Arc<dyn TelemetrySink>,
    );
    let (endpoint, _sd) = common::start_test_server_with_state(state).await;

    let mut writer = auth_and_get_telemetry_writer(&endpoint).await;

    let report = TelemetryReport {
        ts_ms: 1,
        items: vec![InfoSnapshot {
            kind: InfoKind::ProcessSummary as i32,
            payload: Some(Payload::ProcessSummary(ProcessSummary {
                count: 1,
                top_by_cpu: vec![],
            })),
        }],
    };
    writer
        .send(TelemetryMessage {
            msg: Some(Msg::Report(report)),
        })
        .await
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if !sink.reports.lock().unwrap().is_empty() {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("sink did not receive report within 5s");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let captured = sink.reports.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].0.username, "alice");
    assert!(captured[0].0.session_id > 0);
    assert_eq!(captured[0].1.items[0].kind, InfoKind::ProcessSummary as i32);
}
