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

pub mod auth;
pub mod client;
pub mod config;
pub mod ctrl;
pub mod data;
pub mod framing;
pub mod ipam;
pub mod quinn_stream;
pub mod route;
pub mod server;
pub mod tls;
pub mod tun_setup;

pub mod vpn {
    #![allow(clippy::doc_markdown)]
    include!(concat!(env!("OUT_DIR"), "/vpn.rs"));
}
