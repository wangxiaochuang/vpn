pub use crate::vpn::*;
pub use msgx::KEEPALIVE_INTERVAL as HEARTBEAT_INTERVAL;
pub use msgx::KEEPALIVE_TIMEOUT as HEARTBEAT_TIMEOUT;
pub use msgx::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, KeepaliveTracker, MAX_FRAME_LENGTH};

pub type HeartbeatTracker = KeepaliveTracker;
