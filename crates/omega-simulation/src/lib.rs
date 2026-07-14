// omega-engine\crates\omega-simulation\src\lib.rs
//! omega-simulation
//!
//! Phase 0.5 harness: runs the engine's real opportunity-detection and
//! profitability logic against a *forked* copy of live chain state, using
//! real flash loan pool interfaces and real (unmodified) contract bytecode,
//! but with a local disposable node instead of mainnet.
//!
//! Nothing in this crate is capable of reaching a live relay or a real
//! signing key. See `error::SimError::LiveTransportForbidden` — any attempt
//! to route a submission somewhere other than the local fork handle will
//! fail loudly rather than silently degrading into a "test that does
//! nothing."
//!
//! ## What this crate validates
//! - Profitability math under real (forked) pool reserves/prices
//! - Flash loan borrow/fee/repay call path against real pool interfaces
//! - Contract behavior (reentrancy guards, callback decoding) under real
//!   interface constraints
//!
//! ## What this crate does NOT validate
//! - Relay latency or bundle inclusion probability
//! - Competition from other searchers
//! - HSM/execution-key signing flow
//! - Testnet-specific auth wiring
//!
//! Those belong to the `omega-testnet` dry-run layer and, eventually, to
//! staged production rollout — not here.

pub mod error;
pub mod fork;
pub mod submitter;
pub mod harness;
pub mod report;
pub mod traits;

pub use error::SimError;
pub use fork::{ForkConfig, ForkHandle};
pub use harness::{HarnessConfig, SimulationHarness};
pub use report::{CycleResult, SimulationReport};
pub use submitter::SimulationSubmitter;
pub use traits::{
    Bundle, BundleSubmitter, Opportunity, OpportunityDetector, OpportunityKind, Receipt,
};