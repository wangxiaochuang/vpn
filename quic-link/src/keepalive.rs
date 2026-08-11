use std::time::{Duration, Instant};

use msgx::{KeepaliveTracker, Receiver, Sender};
use shutdown::ShutdownHandle;

#[derive(Clone, Copy, Debug)]
pub struct KeepaliveConfig {
    pub interval: Duration,
    pub timeout: Duration,
}

impl Default for KeepaliveConfig {
    fn default() -> Self {
        Self {
            interval: msgx::KEEPALIVE_INTERVAL,
            timeout: msgx::KEEPALIVE_TIMEOUT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopControl {
    Continue,
    Break,
}

/// 通用保活循环。
///
/// `select!` 各分支 cancel-safety：
/// - `shutdown.cancelled()`: cancel-safe，重 poll 返回相同结果。
/// - `timeout_tick.tick()`: `tokio::time::Interval::tick` 文档明确 cancel-safe。
/// - `send_tick.tick()` → `sender.send(hb)`: `SinkExt::send` 取消时缓冲一致。
/// - `receiver.recv()`: `StreamExt::next` 取消不丢已读字节。
///
/// `biased` 保证 shutdown 优先级最高。
pub async fn keepalive_loop<M, F, H>(
    close: &dyn SessionClose,
    sender: &mut Sender<M>,
    receiver: &mut Receiver<M>,
    shutdown: &ShutdownHandle,
    cfg: KeepaliveConfig,
    heartbeat: F,
    mut on_msg: H,
) where
    M: prost::Message + Default,
    F: Fn() -> M,
    H: FnMut(&M) -> LoopControl,
{
    let mut tracker = KeepaliveTracker::new(now());
    let mut last_seen = now();
    let mut send_tick = tokio::time::interval(cfg.interval);
    let mut timeout_tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        let should_break = tokio::select! {
            biased;
            () = shutdown.cancelled() => true,
            _ = timeout_tick.tick() => check_timeout(close, &mut last_seen, cfg.timeout),
            _ = send_tick.tick() => send_heartbeat(sender, &heartbeat).await.is_err(),
            msg = receiver.recv() => {
                handle_recv(msg, &mut tracker, &mut last_seen, &mut on_msg)
            }
        };
        if should_break {
            break;
        }
    }
}

fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

fn check_timeout(close: &dyn SessionClose, last_seen: &mut Instant, timeout: Duration) -> bool {
    if now().duration_since(*last_seen) >= timeout {
        close.close(0x100, b"timeout");
        true
    } else {
        false
    }
}

async fn send_heartbeat<M: prost::Message + Default>(
    sender: &mut Sender<M>,
    heartbeat: &impl Fn() -> M,
) -> Result<(), ()> {
    sender.send(heartbeat()).await.map_err(|_| ())
}

fn handle_recv<M, H: FnMut(&M) -> LoopControl>(
    msg: Result<Option<M>, msgx::RecvError>,
    tracker: &mut KeepaliveTracker,
    last_seen: &mut Instant,
    on_msg: &mut H,
) -> bool {
    match msg {
        Ok(Some(m)) => {
            let t = now();
            tracker.observe(t);
            *last_seen = t;
            on_msg(&m) == LoopControl::Break
        }
        _ => true,
    }
}

/// 抽象的关闭句柄，避免在保活循环中暴露 `quinn::Connection`。
pub trait SessionClose: Send + Sync {
    fn close(&self, code: u64, reason: &[u8]);
}

impl SessionClose for crate::Session {
    fn close(&self, code: u64, reason: &[u8]) {
        crate::Session::close(self, code, reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keepalive_config_default_equals_msgx_constants() {
        let cfg = KeepaliveConfig::default();
        assert_eq!(cfg.interval, Duration::from_secs(10));
        assert_eq!(cfg.timeout, Duration::from_secs(30));
        assert_eq!(cfg.interval, msgx::KEEPALIVE_INTERVAL);
        assert_eq!(cfg.timeout, msgx::KEEPALIVE_TIMEOUT);
    }

    #[test]
    fn test_loop_control_variants_exist() {
        assert_ne!(LoopControl::Continue, LoopControl::Break);
    }
}
