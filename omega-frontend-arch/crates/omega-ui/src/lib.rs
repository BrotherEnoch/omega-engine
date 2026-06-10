// omega-frontend-arch/crates/omega-ui/src/lib.rs
#![forbid(unsafe_code)]
#![allow(
    clippy::module_name_repetitions,
    clippy::multiple_crate_versions,
    clippy::unwrap_used,
    clippy::expect_used,
)]

pub mod app;
pub mod components;
pub mod sync_adapter;
mod ws_client;

// Public re-exports
pub use app::App;