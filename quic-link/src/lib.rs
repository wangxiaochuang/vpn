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
//! `quic-link` —— 通用 QUIC 连接管道 crate。
//!
//! 封装 QUIC 连接的建立、TLS、stream→Channel 适配、datagram 收发、保活循环，
//! 对调用方完全隐藏 [`quinn::Connection`]。

mod datagram;
mod endpoint;
mod keepalive;
#[doc(hidden)]
pub mod quinn_stream;
mod session;
#[cfg(any(test, feature = "test-util"))]
pub mod test_util;
mod tls;

pub use datagram::{DatagramRx, DatagramTx, PacketSink, PacketSource, forward};
pub use endpoint::{Client, ClientBuilder, Server, ServerBuilder};
pub use keepalive::{KeepaliveConfig, LoopControl, SessionClose, keepalive_loop};
pub use session::{Session, SessionError};
pub use tls::{build_quinn_client_config, build_quinn_server_config};
