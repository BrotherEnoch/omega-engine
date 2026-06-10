//! Rust/WASM frontend architecture for Omega Engine.
//!
//! The crate intentionally keeps transport and rendering behind small typed
//! boundaries. UI frameworks can subscribe to `EngineStore` snapshots without
//! duplicating backend schemas.

pub mod commands;
pub mod render;
pub mod state;
pub mod sync;

pub use omega_control_contracts as contracts;
