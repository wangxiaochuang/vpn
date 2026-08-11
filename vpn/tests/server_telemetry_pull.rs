#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use sysprobe::proto::InfoKind;
use sysprobe::proto::TelemetryMessage;
use sysprobe::proto::TelemetryReport;
use sysprobe::proto::telemetry_message::Msg;
use sysprobe::sink::SinkError;
use sysprobe::sink::SinkSource;
use sysprobe::sink::TelemetrySink;
use vpn::telemetry::build_default_registry;
use vpn::telemetry::client_telemetry_loop;
use vpn::telemetry::server_telemetry_loop;

#[derive(Clone, Default)]
struct RecordingSink {
    reports: Arc<Mutex<Vec<(SinkSource, TelemetryReport)>>>,
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

#[tokio::test]
async fn test_server_pull_requests_disk_info_and_receives_report() {
    let pair = common::make_connected_pair().await;

    let server_session = quic_link::Session::new(pair.server.clone());
    let server_accept = tokio::spawn(async move {
        server_session
            .accept_stream::<TelemetryMessage>()
            .await
            .unwrap()
    });

    let client_session = quic_link::Session::new(pair.client.clone());
    let client_channel = client_session
        .open_stream::<TelemetryMessage>()
        .await
        .unwrap();
    let (mut client_writer, client_reader) = client_channel.split();

    let _ = client_writer
        .send(TelemetryMessage {
            msg: Some(Msg::Report(TelemetryReport {
                ts_ms: 0,
                items: vec![],
            })),
        })
        .await;

    let mut registry = build_default_registry();
    let client_sd = shutdown::Shutdown::new(Duration::from_secs(30));
    let client_handle = client_sd.handle();
    let loop_task = tokio::spawn(async move {
        client_telemetry_loop(client_writer, client_reader, &mut registry, &client_handle).await;
    });

    let server_channel = tokio::time::timeout(Duration::from_secs(5), server_accept)
        .await
        .expect("server accept timeout")
        .unwrap();
    let (server_writer, server_reader) = server_channel.split();

    let reports = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::new(RecordingSink {
        reports: reports.clone(),
    }) as Arc<dyn TelemetrySink>;
    let source = SinkSource {
        session_id: 1,
        username: "alice".into(),
        virtual_ip: None,
    };
    let server_sd = shutdown::Shutdown::new(Duration::from_secs(30));
    let server_handle = server_sd.handle();
    let server_loop = tokio::spawn(async move {
        server_telemetry_loop(server_reader, sink, source, server_handle).await;
    });

    let slot = Arc::new(tokio::sync::Mutex::new(Some(server_writer)));
    vpn::telemetry::request_collect(&slot, vec![InfoKind::DiskInfo])
        .await
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let found = reports
            .lock()
            .unwrap()
            .iter()
            .any(|(_, r)| r.items.iter().any(|i| i.kind == InfoKind::DiskInfo as i32));
        if found {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("sink did not receive DiskInfo report within 15s");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    client_sd.trigger();
    server_sd.trigger();
    let _ = tokio::time::timeout(Duration::from_secs(2), loop_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), server_loop).await;
}
