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

pub mod client;
pub mod config;
pub mod route;
pub mod telemetry;

pub use vpn_core::ctrl;
pub use vpn_core::data;
pub use vpn_core::framing;
pub use vpn_core::tun_setup;
pub use vpn_core::vpn;
