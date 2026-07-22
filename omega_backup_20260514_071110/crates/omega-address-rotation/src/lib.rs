ï»¿// crates/omega-address-rotation/src/lib.rs
//
// omega-address-rotation â€” Execution address rotation and relay reputation
// carryover for the Omega Engine (spec Â§14, Â§14.1, Â§14.2).
//
// ## Architectural role (Â§22.1)
//
//   omega-address-rotation â† omega-core, omega-gas-war
//
//   This crate is consumed by the relay submission orchestrator.  It
//   does NOT depend on omega-relay directly â€” reputation carryover data
//   is seeded into `LaRelayMetrics` (owned by omega-gas-war) which the
//   relay layer reads.
//
// ## Spec Â§14 â€” Address rotation
//
//   Execution addresses (the wallet that signs and submits bundles) are
//   rotated on two triggers (Â§14):
//
//   1. **Schedule**: every 30 days regardless of performance.
//   2. **Pattern detection**: when `LOST_RACE_SAME_FEE` exceeds 20% of
//      losses over the rolling window.  High same-fee losses indicate
//      the address has been fingerprinted by block builders who are
//      front-running or deprioritising bundles from the known address.
//
// ## Spec Â§14.1 â€” Reputation carryover (fix C4 + I4)
//
//   On rotation, seed the new address with a fraction of the old
//   address's per-relay inclusion rates.  The fraction decays
//   exponentially with months since rotation to prevent stale data
//   contamination.
//
// ## Spec Â§14.2 â€” Round-robin randomisation (fix I2)
//
//   Relay submission order is randomised within the tie-band on each
//   rotation to prevent relay fingerprinting.
//
// ## Module map
//
//   reputation.rs       â€” Carryover formula and `seed_relay_metrics` (Â§14.1)
//   pattern_detector.rs â€” Rolling-window `LOST_RACE_SAME_FEE` detector (Â§14)
//   rotation.rs         â€” `AddressRotationManager`: schedule + trigger (Â§14)

pub mod pattern_detector;
pub mod reputation;
pub mod rotation;

// â”€â”€ Re-exports â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub use pattern_detector::{PatternDetector, PatternDetectorConfig};
pub use reputation::{compute_carryover_pct, seed_relay_metrics, CarryoverParams};
pub use rotation::{
    AddressRotationManager,
    RotationConfig,
    RotationRecord,
    RotationTrigger,
};