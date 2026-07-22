ï»¿// crates/omega-gas-war/src/lib.rs
//
// omega-gas-war â€” Gas War Engine for the Omega Engine.
//
// ## Architectural role (Â§22.1)
//
//   omega-gas-war â† omega-core
//   omega-relay   â† omega-gas-war   (not the reverse)
//
//   The Gas War Engine produces BundleVariants and relay ordering
//   metadata.  The relay submission loop in omega-relay consumes them.
//   omega-gas-war does NOT call relay submission directly â€” this prevents
//   a circular dependency.
//
// ## Module map
//
//   adaptive_cap.rs        â€” Adaptive priority fee cap (Â§12.2)
//                            Formula: 5% of liquidation bonus / GAS_PER_BUNDLE
//                            Ã— urgency multiplier Ã— win-rate multiplier
//                            Clamped to [2, 500] gwei.
//
//   bundle_variants.rs     â€” 3-variant fee strategy: conservative (0.7Ã—),
//                            aggressive (1.0Ã—), emergency (2.0Ã—, profit-gated)
//                            Fix M2: emergency bundle only when profitable (Â§12.1)
//
//   relay_la_metrics.rs    â€” Per-relay rolling-window LA inclusion rate.
//                            Drives cascade submission order (Â§11.2).
//                            Fix I2: randomised round-robin in tie band.
//
//   builder_blacklist.rs   â€” MEV-Boost builder blacklist (Â§12.3, Â§V1).
//                            Phase 4+ L1 only.  Hot-reloadable via
//                            POST /api/v1/builders/blacklist/update.
//                            NOT applicable on Arbitrum or Base.

pub mod adaptive_cap;
pub mod builder_blacklist;
pub mod bundle_variants;
pub mod relay_la_metrics;

// â”€â”€ Convenience re-exports â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub use adaptive_cap::{
    adaptive_gas_cap_gwei,
    CapComponents,
    GAS_PER_BUNDLE,
    MAX_PRIORITY_FEE_GWEI,
    MIN_PRIORITY_FEE_GWEI,
    UrgencyTier,
    WinRateTier,
};

pub use bundle_variants::{
    compute_variants,
    BundleVariants,
    EmergencySkipReason,
};

pub use relay_la_metrics::{
    LaRelayMetrics,
    RelayRank,
    DEFAULT_WINDOW,
    MIN_SAMPLE_COUNT,
    NEUTRAL_PRIOR,
};

pub use builder_blacklist::{
    filter_relays_for_bundle,
    BuilderBlacklist,
    RelayDescriptor,
};