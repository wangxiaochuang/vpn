#![warn(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::dbg_macro,
    clippy::print_stdout,
    clippy::print_stderr
)]
#![allow(
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::manual_async_fn
)]

use std::time::Duration;

use tokio::signal::unix::Signal;
use tokio::signal::unix::SignalKind;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tokio_util::sync::WaitForCancellationFuture;

pub struct Shutdown {
    token: CancellationToken,
    timeout: Duration,
}

impl Clone for Shutdown {
    fn clone(&self) -> Self {
        Self {
            token: self.token.clone(),
            timeout: self.timeout,
        }
    }
}

impl Default for Shutdown {
    /// 等价于 `Shutdown::new(Shutdown::DEFAULT_DRAIN_TIMEOUT)`。
    fn default() -> Self {
        Self::new(Self::DEFAULT_DRAIN_TIMEOUT)
    }
}

impl Shutdown {
    /// drain 阶段的默认超时：worker 任务有 5 秒优雅退出，超时则整体 abort。
    pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

    pub fn new(timeout: Duration) -> Self {
        Self {
            token: CancellationToken::new(),
            timeout,
        }
    }

    pub fn handle(&self) -> ShutdownHandle {
        ShutdownHandle {
            token: self.token.clone(),
        }
    }

    pub fn trigger(&self) {
        self.token.cancel();
    }

    pub fn triggered(&self) -> WaitForCancellationFuture<'_> {
        self.token.cancelled()
    }

    pub async fn drain<T: 'static>(&self, tasks: &mut JoinSet<T>, label: &str) {
        let joined = async { drain_all(tasks, label).await };
        if tokio::time::timeout(self.timeout, joined).await.is_ok() {
            tracing::info!("{label} graceful shutdown complete");
            return;
        }
        let remaining = tasks.len();
        tasks.abort_all();
        tracing::warn!(
            "{label} graceful shutdown timed out, aborted {remaining} remaining task(s)"
        );
        drain_all(tasks, label).await;
    }

    /// Spawn a watchdog task that listens for SIGINT/SIGTERM.
    ///
    /// On registration success it sends `()` on the returned [`oneshot::Receiver`]
    /// (the "ready" handshake) so callers can `await` it before doing blocking work
    /// that needs a signal handler installed (e.g. interactive password reads).
    /// When either signal arrives the watchdog calls `self.trigger()`.
    ///
    /// The watchdog task owns its own clone, so callers never pass a copy in.
    /// Callers that do not need the ready handshake may drop the returned receiver:
    /// `Sender::send` fails silently and the watchdog keeps working.
    ///
    /// If signal-handler registration fails, the watchdog logs a warning and exits
    /// without triggering; callers should keep a fallback `ctrl_c()` path.
    pub fn spawn_signal_watchdog(&self) -> oneshot::Receiver<()> {
        let (ready_tx, ready_rx) = oneshot::channel();
        tokio::spawn(watchdog_task(self.clone(), ready_tx));
        ready_rx
    }

    /// Inline interrupt wait for callers that do not need a pre-spawned watchdog.
    ///
    /// Resolves immediately if `self` is already triggered; otherwise waits for
    /// Ctrl-C (SIGINT) and then calls `self.trigger()`. Does not spawn a task.
    pub async fn wait_for_interrupt(&self) {
        tokio::select! {
            biased;
            () = self.triggered() => {}
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received Ctrl+C, initiating graceful shutdown");
                self.trigger();
            }
        }
    }

    /// Construct a `Shutdown` with [`Self::DEFAULT_DRAIN_TIMEOUT`] and a signal
    /// watchdog installed, resolving only after the SIGINT/SIGTERM handlers are
    /// registered. If registration fails the watchdog logs a warning and exits;
    /// callers should keep a fallback `ctrl_c()` path.
    pub async fn with_signal_watchdog() -> Self {
        let sd = Self::new(Self::DEFAULT_DRAIN_TIMEOUT);
        let _ = sd.spawn_signal_watchdog().await;
        sd
    }
}

async fn drain_all<T: 'static>(tasks: &mut JoinSet<T>, label: &str) {
    while let Some(res) = tasks.join_next().await {
        if let Err(e) = res {
            tracing::error!("{label} task panicked: {e}");
        }
    }
}

/// Worker 侧的取消句柄，由 [`Shutdown::handle`] 派生。
///
/// 与 `CancellationToken` 不同，`ShutdownHandle` 不携带 drain 超时，也不提供
/// `drain`/`new`/`trigger` 等主控 API；worker 只能监听取消（[`Self::cancelled`]）
/// 或在同一取消根上触发广播（[`Self::cancel`]）。
#[derive(Clone)]
pub struct ShutdownHandle {
    token: CancellationToken,
}

impl ShutdownHandle {
    pub fn cancelled(&self) -> WaitForCancellationFuture<'_> {
        self.token.cancelled()
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

async fn watchdog_task(sd: Shutdown, ready_tx: oneshot::Sender<()>) {
    let Some((mut sigint, mut sigterm)) = register_handlers() else {
        return;
    };
    let _ = ready_tx.send(());
    wait_for_signal(&mut sigint, &mut sigterm).await;
    sd.trigger();
}

fn register_handlers() -> Option<(Signal, Signal)> {
    let sigint = register_signal(SignalKind::interrupt(), "SIGINT")?;
    let sigterm = register_signal(SignalKind::terminate(), "SIGTERM")?;
    Some((sigint, sigterm))
}

fn register_signal(kind: SignalKind, label: &str) -> Option<Signal> {
    match tokio::signal::unix::signal(kind) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!("failed to register {label} handler: {e}");
            None
        }
    }
}

async fn wait_for_signal(sigint: &mut Signal, sigterm: &mut Signal) {
    tokio::select! {
        _ = sigint.recv() => tracing::info!("received SIGINT, initiating graceful shutdown"),
        _ = sigterm.recv() => tracing::info!("received SIGTERM, initiating graceful shutdown"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl tracing_subscriber::fmt::MakeWriter<'_> for Capture {
        type Writer = CaptureWriter;
        fn make_writer(&self) -> Self::Writer {
            CaptureWriter(Arc::clone(&self.0))
        }
    }

    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn captured_logs() -> (Capture, Arc<Mutex<Vec<u8>>>) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        (Capture(Arc::clone(&buf)), buf)
    }

    async fn drain_capturing_error_logs(
        sd: &Shutdown,
        tasks: &mut JoinSet<()>,
        buf: Arc<Mutex<Vec<u8>>>,
    ) {
        let capture = Capture(Arc::clone(&buf));
        let guard = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_writer(capture)
                .with_max_level(tracing::Level::ERROR)
                .finish(),
        );
        sd.drain(tasks, "client").await;
        drop(guard);
    }

    #[tokio::test]
    async fn test_shutdown_trigger_broadcasts_to_cloned_tokens() {
        let sd = Shutdown::default();
        let cloned = sd.clone();
        let handle = sd.handle();
        sd.trigger();
        assert!(sd.handle().is_cancelled());
        assert!(cloned.handle().is_cancelled());
        assert!(handle.is_cancelled());
        tokio::time::timeout(Duration::from_millis(50), sd.triggered())
            .await
            .expect("triggered future should resolve immediately after trigger");
        tokio::time::timeout(Duration::from_millis(50), cloned.triggered())
            .await
            .expect("cloned triggered future should also resolve");
    }

    #[tokio::test]
    async fn test_drain_when_tasks_finish_returns_gracefully() {
        let mut tasks: JoinSet<()> = JoinSet::new();
        for _ in 0..3 {
            tasks.spawn(async {});
        }
        let sd = Shutdown::default();
        let completed = tokio::time::timeout(Duration::from_secs(2), async {
            sd.drain(&mut tasks, "test").await;
        })
        .await;
        assert!(completed.is_ok(), "should return well under inner timeout");
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn test_drain_when_task_hangs_aborts_after_timeout() {
        let mut tasks: JoinSet<()> = JoinSet::new();
        tasks.spawn(async {
            std::future::pending::<()>().await;
        });
        let sd = Shutdown::new(Duration::from_millis(50));
        let start = tokio::time::Instant::now();
        sd.drain(&mut tasks, "test").await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "abort path must bound runtime, elapsed {elapsed:?}"
        );
        assert!(tasks.is_empty(), "abort_all should clear the JoinSet");
    }

    #[tokio::test]
    async fn test_drain_embedded_in_select_is_cancel_safe() {
        let mut tasks: JoinSet<()> = JoinSet::new();
        tasks.spawn(async {
            std::future::pending::<()>().await;
        });
        let sd = Shutdown::default();
        tokio::select! {
            biased;
            () = tokio::time::sleep(Duration::from_millis(10)) => {}
            () = sd.drain(&mut tasks, "test") => {}
        }
        assert_eq!(
            tasks.len(),
            1,
            "JoinSet state must survive drain cancellation"
        );
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn test_drain_when_task_panics_logs_error() {
        let mut tasks: JoinSet<()> = JoinSet::new();
        tasks.spawn(async {
            panic!("intentional panic for drain test");
        });
        let sd = Shutdown::default();
        let (_, buf) = captured_logs();
        drain_capturing_error_logs(&sd, &mut tasks, buf.clone()).await;
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("client"),
            "log should contain label, got: {output}"
        );
        assert!(
            output.contains("panicked"),
            "log should mention panic, got: {output}"
        );
        assert!(tasks.is_empty(), "JoinSet should be drained");
    }

    fn send_signal(sig: i32) {
        unsafe {
            libc::kill(libc::getpid(), sig);
        }
    }

    async fn trigger_via_signal(sig: i32, sd: &Shutdown, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            send_signal(sig);
            match tokio::time::timeout(Duration::from_millis(50), sd.triggered()).await {
                Ok(()) => return true,
                Err(_) if tokio::time::Instant::now() >= deadline => return false,
                Err(_) => {}
            }
        }
    }

    #[tokio::test]
    async fn test_spawn_signal_watchdog_ready_fires_after_handler_registered() {
        let sd = Shutdown::default();
        let ready = sd.spawn_signal_watchdog();
        let result = tokio::time::timeout(Duration::from_secs(2), ready).await;
        assert!(
            result.is_ok(),
            "ready should fire after handler registration, no signal needed"
        );
        assert!(result.unwrap().is_ok(), "ready channel should deliver ()");
    }

    #[tokio::test]
    async fn test_spawn_signal_watchdog_triggers_on_sigint() {
        let sd = Shutdown::default();
        let ready = sd.spawn_signal_watchdog();
        let _ = ready.await;
        let triggered = trigger_via_signal(libc::SIGINT, &sd, Duration::from_secs(3)).await;
        assert!(triggered, "watchdog should trigger Shutdown on SIGINT");
    }

    #[tokio::test]
    async fn test_spawn_signal_watchdog_triggers_on_sigterm() {
        let sd = Shutdown::default();
        let ready = sd.spawn_signal_watchdog();
        let _ = ready.await;
        let triggered = trigger_via_signal(libc::SIGTERM, &sd, Duration::from_secs(3)).await;
        assert!(triggered, "watchdog should trigger Shutdown on SIGTERM");
    }

    #[tokio::test]
    async fn test_spawn_signal_watchdog_dropped_ready_still_triggers() {
        let _guard = tokio::signal::unix::signal(SignalKind::interrupt())
            .expect("install guard SIGINT handler so the process survives pre-registration");
        let sd = Shutdown::default();
        drop(sd.spawn_signal_watchdog());
        let triggered = trigger_via_signal(libc::SIGINT, &sd, Duration::from_secs(3)).await;
        assert!(
            triggered,
            "watchdog should trigger even when ready receiver is dropped"
        );
    }

    #[tokio::test]
    async fn test_with_signal_watchdog_triggers_on_sigint_immediately_after_return() {
        let sd = Shutdown::with_signal_watchdog().await;
        let triggered = trigger_via_signal(libc::SIGINT, &sd, Duration::from_secs(3)).await;
        assert!(
            triggered,
            "factory should have installed the watchdog before returning"
        );
    }

    #[tokio::test]
    async fn test_with_signal_watchdog_uses_default_drain_timeout() {
        let sd = Shutdown::with_signal_watchdog().await;
        assert_eq!(sd.timeout, Shutdown::DEFAULT_DRAIN_TIMEOUT);
    }

    // handler-registration failure cannot be reliably reproduced in a normal unix
    // environment (`signal()` only fails under extreme conditions), so the
    // "registration fails -> warn + exit without trigger" contract is documented
    // on `spawn_signal_watchdog` rather than covered by a runtime test.

    #[tokio::test]
    async fn test_wait_for_interrupt_returns_immediately_when_already_triggered() {
        let sd = Shutdown::default();
        sd.trigger();
        let result =
            tokio::time::timeout(Duration::from_millis(100), sd.wait_for_interrupt()).await;
        assert!(
            result.is_ok(),
            "should return immediately when already triggered, not wait for ctrl_c"
        );
    }

    #[tokio::test]
    async fn test_wait_for_interrupt_triggers_on_ctrl_c() {
        let _guard = tokio::signal::unix::signal(SignalKind::interrupt())
            .expect("install guard SIGINT handler so the process survives pre-registration");
        let sd = Shutdown::default();
        let sd_for_task = sd.clone();
        let wait_task = tokio::spawn(async move { sd_for_task.wait_for_interrupt().await });
        tokio::task::yield_now().await;
        let triggered = trigger_via_signal(libc::SIGINT, &sd, Duration::from_secs(3)).await;
        assert!(
            triggered,
            "wait_for_interrupt should trigger Shutdown on ctrl_c (SIGINT)"
        );
        let _ = wait_task.await;
    }
}
