use std::time::{Duration, Instant};

pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
pub const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct KeepaliveTracker {
    last_seen: Instant,
}

impl KeepaliveTracker {
    pub fn new(now: Instant) -> Self {
        Self { last_seen: now }
    }

    pub fn observe(&mut self, now: Instant) {
        self.last_seen = now;
    }

    pub fn is_dead(&self, now: Instant) -> bool {
        now.duration_since(self.last_seen) >= KEEPALIVE_TIMEOUT
    }

    pub fn next_deadline(&self) -> Instant {
        self.last_seen + KEEPALIVE_TIMEOUT
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_keepalive_interval_equals_10_seconds() {
        assert_eq!(KEEPALIVE_INTERVAL, Duration::from_secs(10));
    }

    #[test]
    fn test_keepalive_timeout_equals_30_seconds() {
        assert_eq!(KEEPALIVE_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn test_is_dead_at_construction_returns_false() {
        let t0 = Instant::now();
        let tracker = KeepaliveTracker::new(t0);
        assert!(!tracker.is_dead(t0));
    }

    #[test]
    fn test_is_dead_just_below_timeout_returns_false() {
        let t0 = Instant::now();
        let tracker = KeepaliveTracker::new(t0);
        let just_below = (t0 + KEEPALIVE_TIMEOUT)
            .checked_sub(Duration::from_nanos(1))
            .unwrap();
        assert!(!tracker.is_dead(just_below));
    }

    #[test]
    fn test_is_dead_at_exact_timeout_returns_true() {
        let t0 = Instant::now();
        let tracker = KeepaliveTracker::new(t0);
        assert!(tracker.is_dead(t0 + KEEPALIVE_TIMEOUT));
    }

    #[test]
    fn test_is_dead_beyond_timeout_returns_true() {
        let t0 = Instant::now();
        let tracker = KeepaliveTracker::new(t0);
        assert!(tracker.is_dead(t0 + KEEPALIVE_TIMEOUT + Duration::from_secs(5)));
    }

    #[test]
    fn test_observe_revives_after_death() {
        let t0 = Instant::now();
        let mut tracker = KeepaliveTracker::new(t0);
        let deadline = t0 + KEEPALIVE_TIMEOUT;
        assert!(tracker.is_dead(deadline));
        tracker.observe(deadline);
        assert!(!tracker.is_dead(deadline + Duration::from_secs(1)));
    }

    #[test]
    fn test_next_deadline_equals_last_seen_plus_timeout() {
        let t0 = Instant::now();
        let tracker = KeepaliveTracker::new(t0);
        assert_eq!(tracker.next_deadline(), t0 + KEEPALIVE_TIMEOUT);
    }

    #[test]
    fn test_next_deadline_updates_after_observe() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(100);
        let mut tracker = KeepaliveTracker::new(t0);
        tracker.observe(t1);
        assert_eq!(tracker.next_deadline(), t1 + KEEPALIVE_TIMEOUT);
    }
}
