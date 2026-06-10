// crates/omega-gas-war/src/lib.rs
//
// omega-gas-war — Gas War Engine for the Omega Engine.
//
// ## Architectural role (§22.1)
//
//   omega-gas-war ← omega-core
//   omega-relay   ← omega-gas-war   (not the reverse)
//
//   The Gas War Engine produces BundleVariants and relay ordering
//   metadata.  The relay submission loop in omega-relay consumes them.
//   omega-gas-war does NOT call relay submission directly — this prevents
//   a circular dependency.
//
// ## Module map
//
//   adaptive_cap.rs        — Adaptive priority fee cap (§12.2)
//                            Formula: 5% of liquidation bonus / GAS_PER_BUNDLE
//                            × urgency multiplier × win-rate multiplier
//                            Clamped to [2, 500] gwei.
//
//   bundle_variants.rs     — 3-variant fee strategy: conservative (0.7×),
//                            aggressive (1.0×), emergency (2.0×, profit-gated)
//                            Fix M2: emergency bundle only when profitable (§12.1)
//
//   relay_la_metrics.rs    — Per-relay rolling-window LA inclusion rate.
//                            Drives cascade submission order (§11.2).
//                            Fix I2: randomised round-robin in tie band.
//
//   builder_blacklist.rs   — MEV-Boost builder blacklist (§12.3, §V1).
//                            Phase 4+ L1 only.  Hot-reloadable via
//                            POST /api/v1/builders/blacklist/update.
//                            NOT applicable on Arbitrum or Base.

pub mod adaptive_cap;
pub mod builder_blacklist;
pub mod bundle_variants;
pub mod relay_la_metrics;

// ── Convenience re-exports ────────────────────────────────────────────────────

pub use adaptive_cap::{
    adaptive_gas_cap_gwei, CapComponents, UrgencyTier, WinRateTier, GAS_PER_BUNDLE,
    MAX_PRIORITY_FEE_GWEI, MIN_PRIORITY_FEE_GWEI,
};

pub use bundle_variants::{compute_variants, BundleVariants, EmergencySkipReason};

pub use relay_la_metrics::{
    LaRelayMetrics, RelayRank, DEFAULT_WINDOW, MIN_SAMPLE_COUNT, NEUTRAL_PRIOR,
};

pub use builder_blacklist::{filter_relays_for_bundle, BuilderBlacklist, RelayDescriptor};
