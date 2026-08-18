#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use prost::Message as _;
use sysprobe::proto::InfoKind;
use sysprobe::proto::InfoSnapshot;
use sysprobe::proto::TelemetryMessage;
use sysprobe::proto::TelemetryReport;
use sysprobe::proto::info_snapshot::Payload;
use sysprobe::proto::telemetry_message::Msg;
use sysprobe::sink::SinkSource;
use sysprobe::sink::TelemetrySink;
use vpn_client::telemetry::build_default_registry;
use vpn_client::telemetry::client_telemetry_loop;
use vpn_server::db::TelemetryFilter;
use vpn_server::db::TelemetryRow;
use vpn_server::db::open_telemetry_store;
use vpn_server::db::sqlite::SqliteTelemetrySink;
use vpn_server::telemetry::request_collect;
use vpn_server::telemetry::server_telemetry_loop;

struct LiveHarness {
    _dir: tempfile::TempDir,
    db: String,
    client_sd: shutdown::Shutdown,
    server_sd: shutdown::Shutdown,
    client_loop: tokio::task::JoinHandle<()>,
    server_loop: tokio::task::JoinHandle<()>,
}

async fn start_live_store() -> (
    LiveHarness,
    Arc<tokio::sync::Mutex<Option<vpn_core::telemetry::TelemetrySender>>>,
) {
    let pair = common::make_connected_pair().await;
    let dir = tempfile::tempdir().unwrap();
    let db = format!("sqlite://{}", dir.path().join("telemetry.db").display());
    let db_for_sink = db.clone();

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

    let store = open_telemetry_store(&db_for_sink).await.unwrap();
    let sink = Arc::new(SqliteTelemetrySink::new(store)) as Arc<dyn TelemetrySink>;
    let source = SinkSource {
        session_id: 1,
        username: "alice".into(),
        virtual_ip: Some("10.0.0.2".into()),
    };
    let server_sd = shutdown::Shutdown::new(Duration::from_secs(30));
    let server_handle = server_sd.handle();
    let server_loop = tokio::spawn(async move {
        server_telemetry_loop(server_reader, sink, source, server_handle).await;
    });

    let harness = LiveHarness {
        _dir: dir,
        db,
        client_sd,
        server_sd,
        client_loop: loop_task,
        server_loop,
    };
    let slot = Arc::new(tokio::sync::Mutex::new(Some(server_writer)));
    (harness, slot)
}

async fn query_alice(harness: &LiveHarness, kind: InfoKind) -> Vec<TelemetryRow> {
    let store = open_telemetry_store(&harness.db).await.unwrap();
    let filter = TelemetryFilter {
        username: Some("alice".into()),
        kind: Some(kind),
        ..TelemetryFilter::default()
    };
    store.query(&filter).await.unwrap()
}

async fn wait_for_rows(harness: &LiveHarness, kind: InfoKind) -> Vec<TelemetryRow> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let rows = query_alice(harness, kind).await;
        if !rows.is_empty() {
            return rows;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no persisted telemetry within 15s"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn stop(harness: LiveHarness) {
    harness.client_sd.trigger();
    harness.server_sd.trigger();
    let _ = tokio::time::timeout(Duration::from_secs(2), harness.client_loop).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), harness.server_loop).await;
}

#[tokio::test]
async fn test_client_report_persists_to_server_db() {
    let (harness, slot) = start_live_store().await;
    request_collect(&slot, vec![InfoKind::DiskInfo])
        .await
        .unwrap();

    let rows = wait_for_rows(&harness, InfoKind::DiskInfo).await;
    let row = &rows[0];
    assert_eq!(row.session_id, 1);
    assert_eq!(row.username, "alice");
    assert_eq!(row.virtual_ip.as_deref(), Some("10.0.0.2"));
    let snapshot = InfoSnapshot::decode(row.payload.as_slice()).unwrap();
    assert!(matches!(snapshot.payload, Some(Payload::Disks(_))));
    assert_eq!(snapshot.kind, InfoKind::DiskInfo as i32);

    stop(harness).await;
}

#[tokio::test]
async fn test_client_report_rows_carry_report_timestamp() {
    let (harness, slot) = start_live_store().await;
    request_collect(&slot, vec![InfoKind::DiskInfo])
        .await
        .unwrap();

    let rows = wait_for_rows(&harness, InfoKind::DiskInfo).await;
    let row = &rows[0];
    assert!(
        row.report_ts_ms > 0,
        "report ts_ms should be a real clock value"
    );
    assert!(row.received_at_ms >= row.report_ts_ms);

    stop(harness).await;
}
