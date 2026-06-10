// omega-frontend-arch/crates/omega-frontend/src/lib.rs
//! # omega-frontend
//!
//! WASM-compatible frontend architecture for OmegaEngine v12.0.
//!
//! ## Module Map
//! | Module | Responsibility |
//! |--------|---------------|
//! | [`state`] | Immutable engine snapshot; monotonic revision counter |
//! | [`commands`] | Typed command ADT mapping to REST mutations |
//! | [`sync`] | Realtime WebSocket + REST polling state synchronisation |
//! | [`render`] | Deterministic render-frame derivation from state |
//! | [`observability`] | Frontend-side event tracing and metric counters |
//!
//! ## Design Invariants
//! 1. **No browser APIs in core** — all platform-specific I/O is behind traits.
//!    The core types compile identically for WASM UI and native test harnesses.
//! 2. **Deterministic rendering** — render frames are pure functions of state
//!    snapshots. The same snapshot always produces the same frame.
//! 3. **Monotonic revision** — every accepted snapshot or event increments a
//!    `u64` revision. Frames derived from revision N are discarded on arrival
//!    of revision N+1 or greater.
//! 4. **Compile-time validation** — all wire types are imported from
//!    `omega-control-contracts`. No inline JSON shape definitions.

#![forbid(unsafe_code)]
#![allow(missing_docs)]

pub mod commands;
pub mod observability;
pub mod render;
pub mod state;
pub mod sync;

pub use state::EngineState;
pub use commands::OmegaCommand;
pub use render::RenderFrame;
