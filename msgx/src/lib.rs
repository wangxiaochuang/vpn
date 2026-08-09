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

pub mod channel;
pub mod codec;
pub mod keepalive;

pub use channel::{
    ByteStream, Channel, Receiver, RecvError, RecvTimeoutError, SendError, SendTimeoutError, Sender,
};
pub use codec::{FrameError, MAX_FRAME_LENGTH, ProtoCodec};
pub use keepalive::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, KeepaliveTracker};
