// omega-engine\crates\omega-testnet\src\lib.rs
//! omega-testnet
//!
//! Phase 0.75 dry-run layer: config schema, report format, and phase-gate
//! criteria tracking for running the engine against a real relay on a
//! testnet (e.g. Sepolia) with a burner wallet holding only faucet funds.
//!
//! This crate does NOT implement a relay-submission client or signing
//! flow. It defines: (1) what a valid testnet run is configured as, with
//! guardrails against accidentally pointing it at a mainnet chain ID; (2)
//! what a completed run's report looks like, including the
//! relay-specific fields `omega-simulation` explicitly cannot measure
//! (inclusion latency, acceptance/rejection, sim-vs-real profit delta);
//! and (3) the Phase 1 gate-criteria checklist, tracked as data rather
//! than tribal knowledge.

pub mod config;
pub mod error;
pub mod gate;
pub mod report;

pub use config::TestnetConfig;
pub use error::TestnetError;
pub use gate::{GateCriteria, GateStatus};
pub use report::{RelayOutcome, TestnetReport};