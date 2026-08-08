use std::time::Duration;

use tokio::task::JoinSet;

pub async fn drain_with_timeout(tasks: &mut JoinSet<()>, timeout: Duration, label: &str) {
    let drain = async { while tasks.join_next().await.is_some() {} };
    if tokio::time::timeout(timeout, drain).await.is_ok() {
        tracing::info!("{label} graceful shutdown complete");
    } else {
        let remaining = tasks.len();
        tasks.abort_all();
        tracing::warn!(
            "{label} graceful shutdown timed out, aborted {remaining} remaining task(s)"
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_drain_with_timeout_when_tasks_finish_returns_gracefully() {
        let mut tasks: JoinSet<()> = JoinSet::new();
        for _ in 0..3 {
            tasks.spawn(async {});
        }
        let completed = tokio::time::timeout(Duration::from_secs(2), async {
            drain_with_timeout(&mut tasks, Duration::from_secs(5), "test").await;
        })
        .await;
        assert!(completed.is_ok(), "should return well under inner timeout");
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn test_drain_with_timeout_when_task_hangs_aborts_after_timeout() {
        let mut tasks: JoinSet<()> = JoinSet::new();
        tasks.spawn(async {
            std::future::pending::<()>().await;
        });
        let elapsed = tokio::time::Instant::now();
        drain_with_timeout(&mut tasks, Duration::from_millis(50), "test").await;
        let elapsed = elapsed.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "abort path must bound runtime, elapsed {elapsed:?}"
        );
    }
}
