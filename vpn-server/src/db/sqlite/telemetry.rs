use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use prost::Message as _;
use sqlx::SqlitePool;
use sysprobe::proto::InfoSnapshot;
use sysprobe::proto::TelemetryReport;
use sysprobe::sink::SinkError;
use sysprobe::sink::SinkSource;
use sysprobe::sink::TelemetrySink;

use crate::db::DbError;
use crate::db::TelemetryFilter;
use crate::db::TelemetryRow;
use crate::db::TelemetryStore;

use super::open_pool;

const CREATE_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS telemetry_items (\
    id INTEGER PRIMARY KEY AUTOINCREMENT,\
    session_id INTEGER NOT NULL,\
    username TEXT NOT NULL,\
    virtual_ip TEXT,\
    report_ts_ms INTEGER NOT NULL,\
    received_at_ms INTEGER NOT NULL,\
    kind INTEGER NOT NULL,\
    payload BLOB NOT NULL)";

const CREATE_INDEX_SQL: &str = "CREATE INDEX IF NOT EXISTS idx_telemetry_username_time \
    ON telemetry_items (username, received_at_ms)";

const INSERT_SQL: &str = "INSERT INTO telemetry_items \
    (session_id, username, virtual_ip, report_ts_ms, received_at_ms, kind, payload) \
    VALUES (?, ?, ?, ?, ?, ?, ?)";

const SELECT_SQL: &str = "SELECT id, session_id, username, virtual_ip, report_ts_ms, \
     received_at_ms, kind, payload FROM telemetry_items WHERE 1 = 1";

const ORDER_SQL: &str = " ORDER BY received_at_ms DESC, id DESC LIMIT ";

type TelemetryTuple = (i64, i64, String, Option<String>, i64, i64, i32, Vec<u8>);

#[derive(Debug)]
pub struct SqliteTelemetryStore {
    pool: SqlitePool,
}

impl SqliteTelemetryStore {
    pub async fn connect(url: &str) -> Result<Self, DbError> {
        let pool = open_pool(url, &[CREATE_TABLE_SQL, CREATE_INDEX_SQL]).await?;
        Ok(Self { pool })
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

#[async_trait]
impl TelemetryStore for SqliteTelemetryStore {
    async fn append(&self, source: &SinkSource, report: &TelemetryReport) -> Result<(), DbError> {
        let received_at = now_ms();
        let mut tx = self.pool.begin().await.map_err(super::io_err)?;
        for item in &report.items {
            insert_item(&mut tx, source, report.ts_ms, item, received_at).await?;
        }
        tx.commit().await.map_err(super::io_err)?;
        Ok(())
    }

    async fn query(&self, filter: &TelemetryFilter) -> Result<Vec<TelemetryRow>, DbError> {
        let rows: Vec<TelemetryTuple> = select_query(filter)
            .build_query_as()
            .fetch_all(&self.pool)
            .await
            .map_err(super::io_err)?;
        Ok(rows.into_iter().map(row_from_tuple).collect())
    }
}

async fn insert_item(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    source: &SinkSource,
    ts_ms: u64,
    item: &InfoSnapshot,
    received_at: i64,
) -> Result<(), DbError> {
    sqlx::query(INSERT_SQL)
        .bind(i64::try_from(source.session_id).unwrap_or(i64::MAX))
        .bind(source.username.as_str())
        .bind(source.virtual_ip.as_deref())
        .bind(i64::try_from(ts_ms).unwrap_or(i64::MAX))
        .bind(received_at)
        .bind(item.kind)
        .bind(item.encode_to_vec())
        .execute(&mut **tx)
        .await
        .map_err(super::io_err)?;
    Ok(())
}

fn select_query(filter: &TelemetryFilter) -> sqlx::QueryBuilder<sqlx::Sqlite> {
    let mut qb = sqlx::QueryBuilder::new(SELECT_SQL);
    apply_filters(&mut qb, filter);
    qb.push(ORDER_SQL).push_bind(filter.limit);
    qb
}

fn apply_filters(qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>, filter: &TelemetryFilter) {
    if let Some(username) = &filter.username {
        qb.push(" AND username = ").push_bind(username.clone());
    }
    if let Some(kind) = filter.kind {
        qb.push(" AND kind = ").push_bind(kind as i32);
    }
    if let Some(since) = filter.since_ms {
        qb.push(" AND received_at_ms >= ").push_bind(since);
    }
    if let Some(until) = filter.until_ms {
        qb.push(" AND received_at_ms <= ").push_bind(until);
    }
}

fn row_from_tuple(t: TelemetryTuple) -> TelemetryRow {
    TelemetryRow {
        id: t.0.to_string(),
        session_id: t.1,
        username: t.2,
        virtual_ip: t.3,
        report_ts_ms: t.4,
        received_at_ms: t.5,
        kind: t.6,
        payload: t.7,
    }
}

fn backend_err(e: &DbError) -> SinkError {
    SinkError::Backend(e.to_string())
}

pub struct SqliteTelemetrySink {
    store: Arc<dyn TelemetryStore>,
}

impl SqliteTelemetrySink {
    pub fn new(store: Arc<dyn TelemetryStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl TelemetrySink for SqliteTelemetrySink {
    async fn store(&self, source: &SinkSource, report: &TelemetryReport) -> Result<(), SinkError> {
        self.store
            .append(source, report)
            .await
            .map_err(|e| backend_err(&e))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use std::str::FromStr;
    use std::time::Duration;

    use super::*;
    use crate::db::contract;
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::sqlite::SqlitePoolOptions;
    use sysprobe::proto::DiskInfo;
    use sysprobe::proto::InfoKind;
    use sysprobe::proto::PortList;
    use sysprobe::proto::ProcessSummary;
    use sysprobe::proto::info_snapshot::Payload;

    const ABORT_DISK_TRIGGER_SQL: &str = "CREATE TRIGGER abort_disk_info \
        BEFORE INSERT ON telemetry_items WHEN NEW.kind = 4 \
        BEGIN SELECT RAISE(ABORT, 'boom'); END";

    fn temp_url(dir: &tempfile::TempDir) -> String {
        format!("sqlite://{}", dir.path().join("telemetry.db").display())
    }

    async fn temp_store() -> (tempfile::TempDir, SqliteTelemetryStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteTelemetryStore::connect(&temp_url(&dir))
            .await
            .unwrap();
        (dir, store)
    }

    fn sample_source() -> SinkSource {
        SinkSource {
            session_id: 7,
            username: "alice".into(),
            virtual_ip: Some("10.0.0.5".into()),
        }
    }

    fn report(items: Vec<InfoSnapshot>) -> TelemetryReport {
        TelemetryReport { ts_ms: 42, items }
    }

    fn snapshot(kind: InfoKind, count: u32) -> InfoSnapshot {
        let payload = match kind {
            InfoKind::ProcessSummary => Payload::ProcessSummary(ProcessSummary {
                count,
                top_by_cpu: vec![],
            }),
            InfoKind::PortList => Payload::Ports(PortList { ports: vec![] }),
            InfoKind::DiskInfo => Payload::Disks(DiskInfo { disks: vec![] }),
            _ => unimplemented!("test 只覆盖三个 kind"),
        };
        InfoSnapshot {
            kind: kind as i32,
            payload: Some(payload),
        }
    }

    async fn exec(pool: &SqlitePool, sql: &'static str) {
        sqlx::query(sql).execute(pool).await.unwrap();
    }

    async fn count_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM telemetry_items")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn seed_row(pool: &SqlitePool, username: &str, kind: i32, received_at: i64) {
        sqlx::query(
            "INSERT INTO telemetry_items \
             (session_id, username, virtual_ip, report_ts_ms, received_at_ms, kind, payload) \
             VALUES (1, ?, NULL, 0, ?, ?, X'00')",
        )
        .bind(username)
        .bind(received_at)
        .bind(kind)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seeded_store() -> (tempfile::TempDir, SqliteTelemetryStore) {
        let (dir, store) = temp_store().await;
        seed_row(&store.pool, "alice", 0, 100).await;
        seed_row(&store.pool, "alice", 4, 200).await;
        seed_row(&store.pool, "bob", 0, 300).await;
        seed_row(&store.pool, "alice", 0, 150).await;
        (dir, store)
    }

    fn received_times(rows: &[TelemetryRow]) -> Vec<i64> {
        rows.iter().map(|r| r.received_at_ms).collect()
    }

    async fn lock_pool(url: &str) -> SqlitePool {
        SqlitePoolOptions::new()
            .connect_with(SqliteConnectOptions::from_str(url).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_sqlite_telemetry_contract_suite() {
        let (_dir, store) = temp_store().await;
        contract::telemetry_append_empty_report_writes_nothing(&store).await;
        let (_d2, store) = temp_store().await;
        contract::telemetry_append_writes_rows_with_context(&store).await;
        let (_d3, store) = temp_store().await;
        contract::telemetry_append_payload_round_trips(&store).await;
        let (_d4, store) = temp_store().await;
        contract::telemetry_query_filters_by_username_and_kind(&store).await;
        let (_d5, store) = temp_store().await;
        contract::telemetry_query_applies_limit(&store).await;
        let (_d6, store) = temp_store().await;
        contract::telemetry_query_returns_empty_when_no_match(&store).await;
        let (_d7, store) = temp_store().await;
        contract::telemetry_query_orders_desc_with_string_ids(&store).await;
    }

    #[tokio::test]
    async fn test_query_applies_time_range() {
        let (_dir, store) = seeded_store().await;
        let filter = TelemetryFilter {
            since_ms: Some(150),
            until_ms: Some(250),
            ..TelemetryFilter::default()
        };
        let rows = store.query(&filter).await.unwrap();
        assert_eq!(received_times(&rows), vec![200, 150]);
    }

    #[tokio::test]
    async fn test_query_orders_same_ms_by_id_desc() {
        let (dir, store) = temp_store().await;
        seed_row(&store.pool, "alice", 0, 200).await;
        seed_row(&store.pool, "alice", 0, 200).await;
        drop(store);
        let reader = SqliteTelemetryStore::connect(&temp_url(&dir))
            .await
            .unwrap();
        let rows = reader.query(&TelemetryFilter::default()).await.unwrap();
        assert_eq!(rows.len(), 2);
        let first: i64 = rows[0].id.parse().unwrap();
        let second: i64 = rows[1].id.parse().unwrap();
        assert!(first > second);
    }

    #[tokio::test]
    async fn test_append_failure_writes_no_partial_rows() {
        let (_dir, store) = temp_store().await;
        exec(&store.pool, ABORT_DISK_TRIGGER_SQL).await;
        let items = vec![
            snapshot(InfoKind::ProcessSummary, 1),
            snapshot(InfoKind::DiskInfo, 0),
        ];
        let err = store
            .append(&sample_source(), &report(items))
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::Io(_)));
        assert_eq!(count_rows(&store.pool).await, 0);
    }

    #[tokio::test]
    async fn test_append_recovers_after_failure() {
        let (_dir, store) = temp_store().await;
        exec(&store.pool, ABORT_DISK_TRIGGER_SQL).await;
        let _ = store
            .append(
                &sample_source(),
                &report(vec![snapshot(InfoKind::DiskInfo, 0)]),
            )
            .await;
        exec(&store.pool, "DROP TRIGGER abort_disk_info").await;
        store
            .append(
                &sample_source(),
                &report(vec![snapshot(InfoKind::ProcessSummary, 1)]),
            )
            .await
            .unwrap();
        assert_eq!(count_rows(&store.pool).await, 1);
    }

    #[tokio::test]
    async fn test_connect_twice_is_idempotent_preserving_data() {
        let (dir, first) = temp_store().await;
        first
            .append(
                &sample_source(),
                &report(vec![snapshot(InfoKind::ProcessSummary, 1)]),
            )
            .await
            .unwrap();
        drop(first);
        let second = SqliteTelemetryStore::connect(&temp_url(&dir))
            .await
            .unwrap();
        assert_eq!(
            second
                .query(&TelemetryFilter::default())
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn test_connect_invalid_url_returns_invalid_input() {
        let err = SqliteTelemetryStore::connect("not-a-url")
            .await
            .map(|_| ())
            .unwrap_err();
        assert!(matches!(err, DbError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn test_connect_unwritable_path_returns_io_error() {
        let err = SqliteTelemetryStore::connect("sqlite:///nonexistent-dir/telemetry.db")
            .await
            .map(|_| ())
            .unwrap_err();
        assert!(matches!(err, DbError::Io(_)));
    }

    #[tokio::test]
    async fn test_dropped_append_future_leaves_no_partial_write_and_stays_usable() {
        let (dir, store) = temp_store().await;
        let lock = lock_pool(&temp_url(&dir)).await;
        exec(&lock, "BEGIN EXCLUSIVE").await;
        let source = sample_source();
        let first = report(vec![snapshot(InfoKind::ProcessSummary, 1)]);
        let fut = store.append(&source, &first);
        tokio::time::timeout(Duration::from_millis(100), fut)
            .await
            .unwrap_err();
        exec(&lock, "ROLLBACK").await;
        assert_eq!(count_rows(&store.pool).await, 0);
        let second = report(vec![snapshot(InfoKind::ProcessSummary, 2)]);
        store.append(&sample_source(), &second).await.unwrap();
        assert_eq!(count_rows(&store.pool).await, 1);
    }

    fn sink_for(store: &SqliteTelemetryStore) -> Arc<dyn TelemetryStore> {
        Arc::new(SqliteTelemetryStore {
            pool: store.pool.clone(),
        })
    }

    #[tokio::test]
    async fn test_sink_delegates_append() {
        let (_dir, store) = temp_store().await;
        let sink = SqliteTelemetrySink::new(sink_for(&store));
        sink.store(
            &sample_source(),
            &report(vec![snapshot(InfoKind::ProcessSummary, 3)]),
        )
        .await
        .unwrap();
        let rows = store.query(&TelemetryFilter::default()).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].username, "alice");
    }

    #[tokio::test]
    async fn test_sink_maps_db_error_to_backend() {
        let (_dir, store) = temp_store().await;
        exec(&store.pool, ABORT_DISK_TRIGGER_SQL).await;
        let sink = SqliteTelemetrySink::new(sink_for(&store));
        let err = sink
            .store(
                &sample_source(),
                &report(vec![snapshot(InfoKind::DiskInfo, 0)]),
            )
            .await
            .unwrap_err();
        let SinkError::Backend(msg) = err else {
            panic!("expected Backend, got {err:?}");
        };
        assert!(!msg.is_empty());
    }
}
