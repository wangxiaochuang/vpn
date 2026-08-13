#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use quic_link::Session;
use sysprobe::collector::CollectError;
use sysprobe::collector::Collector;
use sysprobe::collector::CollectorRegistry;
use sysprobe::collectors::DiskCollector;
use sysprobe::proto::InfoKind;
use sysprobe::proto::InfoSnapshot;
use sysprobe::proto::TelemetryMessage;
use sysprobe::proto::telemetry_message::Msg;
use vpn_client::telemetry::client_telemetry_loop;

struct FastCollector {
    kind: InfoKind,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Collector for FastCollector {
    fn kind(&self) -> InfoKind {
        self.kind
    }

    fn cadence(&self) -> Option<Duration> {
        Some(Duration::from_millis(100))
    }

    async fn collect(&self) -> Result<InfoSnapshot, CollectError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(InfoSnapshot {
            kind: self.kind as i32,
            payload: None,
        })
    }
}

#[tokio::test]
async fn test_client_push_loop_sends_report_when_collector_due() {
    let pair = common::make_connected_pair().await;
    let server_session = Session::new(pair.server.clone());

    let server_accept = tokio::spawn(async move {
        server_session
            .accept_stream::<TelemetryMessage>()
            .await
            .unwrap()
    });

    let client_session = Session::new(pair.client.clone());
    let client_channel = client_session
        .open_stream::<TelemetryMessage>()
        .await
        .unwrap();
    let (client_writer, client_reader) = client_channel.split();

    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = CollectorRegistry::new();
    registry.register(Box::new(FastCollector {
        kind: InfoKind::ProcessSummary,
        calls: calls.clone(),
    }));

    let sd = shutdown::Shutdown::new(Duration::from_secs(5));
    let handle = sd.handle();
    let loop_task = tokio::spawn(async move {
        client_telemetry_loop(client_writer, client_reader, &mut registry, &handle).await;
    });

    let server_channel = tokio::time::timeout(Duration::from_secs(5), server_accept)
        .await
        .expect("server accept timeout")
        .unwrap();
    let (_server_writer, mut server_reader) = server_channel.split();

    let msg = tokio::time::timeout(Duration::from_secs(5), server_reader.recv())
        .await
        .expect("timeout waiting for telemetry report")
        .expect("recv error")
        .expect("stream closed");

    assert!(matches!(msg.msg, Some(Msg::Report(ref r)) if !r.items.is_empty()));
    assert!(calls.load(Ordering::Relaxed) > 0);

    sd.trigger();
    let _ = tokio::time::timeout(Duration::from_secs(2), loop_task).await;
}

#[tokio::test]
async fn test_client_push_loop_responds_to_collect_request() {
    let pair = common::make_connected_pair().await;
    let server_session = Session::new(pair.server.clone());

    let server_accept = tokio::spawn(async move {
        server_session
            .accept_stream::<TelemetryMessage>()
            .await
            .unwrap()
    });

    let client_session = Session::new(pair.client.clone());
    let client_channel = client_session
        .open_stream::<TelemetryMessage>()
        .await
        .unwrap();
    let (client_writer, client_reader) = client_channel.split();

    let mut registry = CollectorRegistry::new();
    registry.register(Box::new(FastCollector {
        kind: InfoKind::ProcessSummary,
        calls: Arc::new(AtomicUsize::new(0)),
    }));
    registry.register(Box::new(DiskCollector::new()));

    let sd = shutdown::Shutdown::new(Duration::from_secs(5));
    let handle = sd.handle();
    let loop_task = tokio::spawn(async move {
        client_telemetry_loop(client_writer, client_reader, &mut registry, &handle).await;
    });

    let server_channel = tokio::time::timeout(Duration::from_secs(5), server_accept)
        .await
        .expect("server accept timeout")
        .unwrap();
    let (mut server_writer, mut server_reader) = server_channel.split();

    let req = TelemetryMessage {
        msg: Some(Msg::CollectReq(sysprobe::proto::CollectRequest {
            kinds: vec![InfoKind::DiskInfo as i32],
        })),
    };
    server_writer.send(req).await.unwrap();

    let found_disk = loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), server_reader.recv())
            .await
            .expect("timeout waiting for pull response")
            .expect("recv error")
            .expect("stream closed");
        if let Some(Msg::Report(ref report)) = msg.msg
            && report
                .items
                .iter()
                .any(|i| i.kind == InfoKind::DiskInfo as i32)
        {
            break true;
        }
    };
    assert!(found_disk);

    sd.trigger();
    let _ = tokio::time::timeout(Duration::from_secs(2), loop_task).await;
}
