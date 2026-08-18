#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use super::DbError;
use super::TelemetryFilter;
use super::TelemetryRow;
use super::TelemetryStore;
use super::UserStore;
use prost::Message as _;
use sysprobe::proto::DiskInfo;
use sysprobe::proto::InfoKind;
use sysprobe::proto::InfoSnapshot;
use sysprobe::proto::PortList;
use sysprobe::proto::ProcessSummary;
use sysprobe::proto::TelemetryReport;
use sysprobe::proto::info_snapshot::Payload;
use sysprobe::sink::SinkSource;

fn hash_password(pw: &str) -> String {
    use argon2::PasswordHasher;
    use argon2::password_hash::SaltString;
    use argon2::password_hash::rand_core::OsRng;
    let salt = SaltString::generate(&mut OsRng);
    argon2::Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .unwrap()
        .to_string()
}

pub(crate) async fn lookup_on_empty_store_returns_none(store: &dyn UserStore) {
    assert_eq!(store.password_hash("alice").await.unwrap(), None);
}

pub(crate) async fn upsert_then_lookup_round_trips(store: &dyn UserStore) {
    let phc = hash_password("s3cret");
    store.upsert("alice", &phc).await.unwrap();
    assert_eq!(store.password_hash("alice").await.unwrap(), Some(phc));
}

pub(crate) async fn upsert_empty_username_rejected_without_write(store: &dyn UserStore) {
    let err = store
        .upsert("", &hash_password("s3cret"))
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::InvalidInput(_)));
    assert!(store.list().await.unwrap().is_empty());
}

pub(crate) async fn upsert_malformed_phc_rejected_without_write(store: &dyn UserStore) {
    let err = store.upsert("alice", "not-a-valid-hash").await.unwrap_err();
    assert!(matches!(err, DbError::InvalidInput(_)));
    assert_eq!(store.password_hash("alice").await.unwrap(), None);
}

pub(crate) async fn upsert_same_user_updates_in_place(store: &dyn UserStore) {
    let first = hash_password("one");
    let second = hash_password("two");
    store.upsert("alice", &first).await.unwrap();
    store.upsert("alice", &second).await.unwrap();
    assert_eq!(store.list().await.unwrap(), vec!["alice".to_string()]);
    assert_eq!(store.password_hash("alice").await.unwrap(), Some(second));
}

pub(crate) async fn delete_existing_user_returns_true_and_clears(store: &dyn UserStore) {
    store
        .upsert("alice", &hash_password("s3cret"))
        .await
        .unwrap();
    assert!(store.delete("alice").await.unwrap());
    assert_eq!(store.password_hash("alice").await.unwrap(), None);
    assert!(store.list().await.unwrap().is_empty());
}

pub(crate) async fn delete_missing_user_returns_false(store: &dyn UserStore) {
    assert!(!store.delete("alice").await.unwrap());
}

pub(crate) async fn list_is_sorted_and_stable(store: &dyn UserStore) {
    for name in ["carol", "alice", "bob"] {
        store.upsert(name, &hash_password("s3cret")).await.unwrap();
    }
    let expected = vec!["alice", "bob", "carol"];
    for _ in 0..2 {
        let got: Vec<String> = store.list().await.unwrap();
        let got: Vec<&str> = got.iter().map(String::as_str).collect();
        assert_eq!(got, expected);
    }
}

fn source_for(username: &str) -> SinkSource {
    SinkSource {
        session_id: 7,
        username: username.into(),
        virtual_ip: Some("10.0.0.5".into()),
    }
}

fn snapshot(kind: InfoKind) -> InfoSnapshot {
    let payload = match kind {
        InfoKind::ProcessSummary => Payload::ProcessSummary(ProcessSummary {
            count: 1,
            top_by_cpu: vec![],
        }),
        InfoKind::PortList => Payload::Ports(PortList { ports: vec![] }),
        InfoKind::DiskInfo => Payload::Disks(DiskInfo { disks: vec![] }),
        _ => unimplemented!("contract 只覆盖 {kind:?} 之外的三个 kind"),
    };
    InfoSnapshot {
        kind: kind as i32,
        payload: Some(payload),
    }
}

fn report(ts_ms: u64, items: Vec<InfoSnapshot>) -> TelemetryReport {
    TelemetryReport { ts_ms, items }
}

pub(crate) async fn telemetry_append_empty_report_writes_nothing(store: &dyn TelemetryStore) {
    store
        .append(&source_for("alice"), &report(42, vec![]))
        .await
        .unwrap();
    assert!(
        store
            .query(&TelemetryFilter::default())
            .await
            .unwrap()
            .is_empty()
    );
}

pub(crate) async fn telemetry_append_writes_rows_with_context(store: &dyn TelemetryStore) {
    let kinds = [
        InfoKind::ProcessSummary,
        InfoKind::PortList,
        InfoKind::DiskInfo,
    ];
    let items: Vec<InfoSnapshot> = kinds.map(snapshot).to_vec();
    store
        .append(&source_for("alice"), &report(42, items))
        .await
        .unwrap();
    let rows = store.query(&TelemetryFilter::default()).await.unwrap();
    assert_row_context(&rows);
    let mut got: Vec<i32> = rows.iter().map(|r| r.kind).collect();
    got.sort_unstable();
    assert_eq!(got, vec![0, 2, 4]);
}

fn assert_row_context(rows: &[TelemetryRow]) {
    assert_eq!(rows.len(), 3);
    for row in rows {
        assert_eq!(row.session_id, 7);
        assert_eq!(row.username, "alice");
        assert_eq!(row.virtual_ip.as_deref(), Some("10.0.0.5"));
        assert_eq!(row.report_ts_ms, 42);
    }
}

pub(crate) async fn telemetry_append_payload_round_trips(store: &dyn TelemetryStore) {
    let snap = snapshot(InfoKind::ProcessSummary);
    store
        .append(&source_for("alice"), &report(42, vec![snap.clone()]))
        .await
        .unwrap();
    let rows = store.query(&TelemetryFilter::default()).await.unwrap();
    let decoded = InfoSnapshot::decode(rows[0].payload.as_slice()).unwrap();
    assert_eq!(decoded, snap);
}

async fn seed_filter_rows(store: &dyn TelemetryStore) {
    let seeds = [
        ("alice", InfoKind::ProcessSummary),
        ("alice", InfoKind::DiskInfo),
        ("bob", InfoKind::ProcessSummary),
        ("alice", InfoKind::ProcessSummary),
    ];
    for (user, kind) in seeds {
        store
            .append(&source_for(user), &report(42, vec![snapshot(kind)]))
            .await
            .unwrap();
    }
}

fn filter(username: Option<&str>, kind: Option<InfoKind>, limit: u32) -> TelemetryFilter {
    TelemetryFilter {
        username: username.map(str::to_string),
        kind,
        limit,
        ..TelemetryFilter::default()
    }
}

pub(crate) async fn telemetry_query_filters_by_username_and_kind(store: &dyn TelemetryStore) {
    seed_filter_rows(store).await;
    let rows = store
        .query(&filter(Some("alice"), Some(InfoKind::ProcessSummary), 50))
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.username == "alice" && r.kind == 0));
}

pub(crate) async fn telemetry_query_applies_limit(store: &dyn TelemetryStore) {
    seed_filter_rows(store).await;
    let rows = store.query(&filter(None, None, 2)).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_desc_order(&rows);
}

pub(crate) async fn telemetry_query_returns_empty_when_no_match(store: &dyn TelemetryStore) {
    seed_filter_rows(store).await;
    let rows = store.query(&filter(Some("carol"), None, 50)).await.unwrap();
    assert!(rows.is_empty());
}

fn assert_desc_order(rows: &[TelemetryRow]) {
    for pair in rows.windows(2) {
        assert!(pair[0].received_at_ms >= pair[1].received_at_ms);
        assert!(!pair[0].id.is_empty() && !pair[1].id.is_empty());
    }
}

pub(crate) async fn telemetry_query_orders_desc_with_string_ids(store: &dyn TelemetryStore) {
    seed_filter_rows(store).await;
    let rows = store.query(&filter(None, None, 50)).await.unwrap();
    assert_desc_order(&rows);
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|r| !r.id.is_empty()));
}
