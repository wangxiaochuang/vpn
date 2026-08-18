use std::str::FromStr;

pub mod telemetry;
pub mod user;

pub use telemetry::SqliteTelemetrySink;
pub use telemetry::SqliteTelemetryStore;
pub use user::SqliteUserStore;

use crate::db::DbError;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqliteJournalMode;
use sqlx::sqlite::SqlitePoolOptions;

pub(super) fn io_err(e: sqlx::Error) -> DbError {
    DbError::Io(Box::new(e))
}

pub(super) async fn open_pool(url: &str, ddl: &[&'static str]) -> Result<SqlitePool, DbError> {
    let pool = SqlitePoolOptions::new()
        .connect_with(connect_options(url)?)
        .await
        .map_err(io_err)?;
    for stmt in ddl {
        sqlx::query(*stmt).execute(&pool).await.map_err(io_err)?;
    }
    Ok(pool)
}

fn connect_options(url: &str) -> Result<SqliteConnectOptions, DbError> {
    if !url.starts_with("sqlite:") {
        return Err(DbError::InvalidInput(format!("invalid sqlite url: {url}")));
    }
    let options = SqliteConnectOptions::from_str(url)
        .map_err(|e| DbError::InvalidInput(format!("invalid sqlite url: {e}")))?;
    Ok(options
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal))
}
