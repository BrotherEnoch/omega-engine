// crates/omega-core/src/config.rs
//
// OmegaConfig â€” runtime configuration for the Omega Engine.
//
// ## Governance tiers (Â§5)
//
// Every field is annotated with the governance tier required to change it
// in production.  The tiers and their properties are:
//
//   L1 (Operator)    Hot-reload via POST /api/v1/config.  No timelock.
//                    Risk: low.  Scope: operational knobs (log levels,
//                    metrics endpoints, relay timeouts).
//
//   L2 (Fast-Approve) Signed by â‰¥2/5 governance keys.  Effective
//                    immediately after signature validation.
//                    Risk: medium.  Scope: strategy parameters, fee
//                    ceilings, ML learning rate.
//
//   L3 (Timelock)   48-hour timelock + 3/5 multisig.  Risk: high.
//                   Scope: phase gates, Vault parameters, DAO fee bps.
//                   Emergency L3 path: 24h + 3/5, qualifying criteria
//                   only (Â§5.1).
//
// Fields that are IMMUTABLE after deployment (chain_id, contract
// addresses that are Certora-verified invariants) are marked IMMUTABLE.
// They can only change via a full redeployment.
//
// ## Serialisation contract
//
// OmegaConfig is loaded from a TOML file at startup and can be
// hot-reloaded via the control-plane API (Â§17).  All defaults are set
// via serde's `default` attribute so that a minimal config file works
// in development.  Production deployments must provide all L3 fields
// explicitly â€” missing L3 fields cause a `OmegaError::Config` halt.
//
// `#[serde(deny_unknown_fields)]` is applied at every level, including
// the top-level `OmegaConfig` itself (previously missing here â€” an
// unrecognised top-level TOML key would have been silently ignored
// rather than rejected, the one place in this file that didn't match
// its own stated strictness policy).
//
// ## WeiAmount â€” u128 amounts that survive TOML round-trips
//
// The `toml` crate (v0.5/v0.8) represents all TOML integers as `i64`
// internally and rejects values that don't fit â€” `i64::MAX` â‰ˆ
// 9.223 Ã— 10^18, which is only ~9.223 ETH in wei. Neither a plain `u64`
// nor a plain `u128` Rust field changes this: the TOML *parser* rejects
// the literal before serde ever sees it, regardless of what Rust type
// is on the receiving end. `WeiAmount` (defined below) sidesteps this
// by serializing as a decimal STRING in TOML (TOML strings have no
// magnitude limit) while storing the value as `u128` internally.
//
// This replaces a previous `u64`-typed workaround whose defaults were
// both hardcoded to ~9 ETH â€” not merely an approximation of the spec's
// 50 ETH / 500 ETH values, but numerically IDENTICAL to each other,
// which collapsed the per-transfer and daily caps into one limit in
// practice (a single transfer could already exhaust the entire "daily"
// budget). `WeiAmount` restores the actual spec values and `validate()`
// now enforces `per_transfer_cap_wei <= daily_cap_wei` explicitly rather
// than relying on the defaults happening to make sense.
//
// ## Fix (this revision): dead_code on tests::Wrapper.amount
//
// `cargo clippy -- -D warnings` promotes `dead_code` (a rustc lint, not
// even a clippy one) to a hard error. Of the two local
// `struct Wrapper { amount: WeiAmount }` test fixtures near the bottom
// of this file, `wei_amount_deserializes_from_plain_integer_too` reads
// `parsed.amount` and was never flagged. `wei_amount_rejects_garbage_string`
// only asserts `result.is_err()` â€” since deserialization always fails in
// that test, `Wrapper` is never successfully constructed, so `.amount`
// is genuinely, permanently unread there: the field's TYPE (driving
// `WeiAmount`'s custom `Deserialize` impl) is what's under test, not any
// value read from it. Fixed with a scoped `#[allow(dead_code)]` on that
// one local struct rather than changing the test's behavior â€” reading
// `.amount` after confirming `is_err()` isn't possible (there's no `Ok`
// value to read it from), so there's no way to make the field
// "genuinely used" without testing something this test isn't about.
//
// Spec references:
//   Â§1.1  â€” phase gates â†’ active_phase
//   Â§5    â€” governance tiers
//   Â§5.1  â€” emergency L3 criteria
//   Â§7    â€” dual-component gas model â†’ GasConfig
//   Â§11.1 â€” LA tier thresholds â†’ LaConfig
//   Â§11.2 â€” cascade backpressure â†’ RelayConfig
//   Â§12.1 â€” emergency bundle profit check â†’ GasConfig
//   Â§12.2 â€” 500 gwei priority fee ceiling â†’ GasConfig
//   Â§13   â€” ML online learner â†’ MlConfig
//   Â§14   â€” address rotation â†’ RotationConfig
//   Â§15   â€” Vault parameters â†’ VaultConfig
//   Â§15.2 â€” per-transfer / daily caps â†’ VaultConfig::{per_transfer_cap_wei, daily_cap_wei}
//   Â§17.1 â€” WebSocket rate limits â†’ ApiConfig
//   Â§18   â€” CHI/GST gas tokens (Phase 4+ L1 only) â†’ GasConfig

use serde::{Deserialize, Serialize};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// WeiAmount
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Wei-denominated amount that can express values larger than
/// `i64::MAX` in TOML config files. See the module doc comment above
/// for why this exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WeiAmount(u128);

impl WeiAmount {
    pub const ZERO: WeiAmount = WeiAmount(0);

    #[inline]
    pub const fn from_wei(wei: u128) -> Self {
        WeiAmount(wei)
    }

    #[inline]
    pub const fn as_wei(self) -> u128 {
        self.0
    }

    /// Construct from a whole-ETH amount, converting to wei internally.
    /// Panics on overflow â€” unreachable with any realistic config value;
    /// even 1 million ETH fits comfortably under `u128::MAX`.
    #[inline]
    pub fn from_eth(eth: u64) -> Self {
        WeiAmount(
            (eth as u128)
                .checked_mul(1_000_000_000_000_000_000)
                .expect("from_eth overflow: value exceeds representable wei range"),
        )
    }
}

impl std::fmt::Display for WeiAmount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for WeiAmount {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Always serialize as a decimal string. This is what makes the
        // type TOML-safe (see module doc comment) and is also
        // unambiguous in JSON, which silently loses precision for
        // integers beyond 2^53 in many JSON consumers (e.g.
        // JavaScript's Number type) â€” a string sidesteps that too.
        serializer.serialize_str(&self.0.to_string())
    }
}

/// Accepts either a decimal string (the canonical wire format â€” see
/// `Serialize` above) or a plain integer (so config constructed
/// programmatically, or via a JSON source with a small-enough value,
/// still deserializes without requiring the string form).
#[derive(Deserialize)]
#[serde(untagged)]
enum WeiAmountRepr {
    String(String),
    Number(u64),
}

impl<'de> Deserialize<'de> for WeiAmount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match WeiAmountRepr::deserialize(deserializer)? {
            WeiAmountRepr::String(s) => s
                .trim()
                .parse::<u128>()
                .map(WeiAmount)
                .map_err(|e| serde::de::Error::custom(format!("invalid wei amount {s:?}: {e}"))),
            WeiAmountRepr::Number(n) => Ok(WeiAmount(n as u128)),
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Top-level config
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Full runtime configuration for the Omega Engine.
///
/// Loaded from `config/omega.toml` at startup.  Hot-reloaded via
/// `POST /api/v1/config` (L1 fields only; L2/L3 fields require the
/// appropriate governance signature).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OmegaConfig {
    /// Currently active system phase (Â§1.1, Â§20).
    ///
    /// GOVERNANCE: L3 (48h timelock).  Phase activation is a
    /// high-stakes irreversible operation â€” once Phase 3 is active,
    /// Phase 2 strategies continue running alongside it.
    ///
    /// Valid values: 0 (Shadow/Backtest), 1 (SA), 2 (MSA), 3 (LA), 4 (MEV).
    #[serde(default = "defaults::active_phase")]
    pub active_phase: u8,

    /// Gas model configuration (Â§7, Â§12.1, Â§12.2).
    #[serde(default)]
    pub gas: GasConfig,

    /// LA-specific configuration (Â§11).
    #[serde(default)]
    pub la: LaConfig,

    /// Relay submission configuration (Â§11.2, Â§12).
    #[serde(default)]
    pub relay: RelayConfig,

    /// ML online learner configuration (Â§13).
    #[serde(default)]
    pub ml: MlConfig,

    /// Address rotation configuration (Â§14).
    #[serde(default)]
    pub rotation: RotationConfig,

    /// Vault and PIL treasury configuration (Â§15).
    #[serde(default)]
    pub vault: VaultConfig,

    /// Control-plane API configuration (Â§17).
    #[serde(default)]
    pub api: ApiConfig,
}

impl Default for OmegaConfig {
    fn default() -> Self {
        Self {
            active_phase: defaults::active_phase(),
            gas: GasConfig::default(),
            la: LaConfig::default(),
            relay: RelayConfig::default(),
            ml: MlConfig::default(),
            rotation: RotationConfig::default(),
            vault: VaultConfig::default(),
            api: ApiConfig::default(),
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// GasConfig
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Arbitrum dual-component gas model parameters (Â§7, Â§12.1, Â§12.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GasConfig {
    /// L2 execution gas estimate buffer factor.
    ///
    /// Applied as: `l2_gas_budget = l2_exec_estimate Ã— l2_buffer_factor`.
    /// Default 1.15 = 15% headroom.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::l2_buffer_factor")]
    pub l2_buffer_factor: f64,

    /// L1 data gas estimate buffer factor.
    ///
    /// Applied to the calldata-bytes Ã— 16 estimate.  Default 1.10 = 10%.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::l1_data_buffer_factor")]
    pub l1_data_buffer_factor: f64,

    /// Maximum priority fee submitted to the Arbitrum sequencer, in gwei.
    ///
    /// Spec Â§12.2: 500 gwei ceiling.  At Arbitrum's 250ms block time
    /// this is ~0.0105 ETH per block â€” comparable to 50 gwei on L1.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::max_priority_fee_gwei")]
    pub max_priority_fee_gwei: u64,

    /// Conservative bundle fee as a fraction of the cap (0.0â€“1.0).
    ///
    /// Spec Â§12: conservative_fee = cap Ã— conservative_fee_fraction.
    /// Default 0.70 = 70% of cap.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::conservative_fee_fraction")]
    pub conservative_fee_fraction: f64,

    /// Whether to enable emergency bundle emission (Â§12.1).
    ///
    /// When true, a third bundle at 2Ã— cap is emitted IFF
    /// `expected_profit_net > emergency_gas_cost + dynamic_min_profit`.
    /// The profit check is MANDATORY â€” bundles are never submitted at a
    /// loss (fix M2).
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::emergency_bundle_enabled")]
    pub emergency_bundle_enabled: bool,

    /// Whether to evaluate CHI/GST gas token redemption (Â§18).
    ///
    /// Applicable on Ethereum L1 (Phase 4+) only.  Must be `false` on
    /// Arbitrum and Base â€” EVM storage refunds do not exist there.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::gas_token_enabled")]
    pub gas_token_enabled: bool,

    /// Minimum L1 base fee in gwei above which CHI/GST redemption is
    /// evaluated (Â§18).  Spec recommendation: 80 gwei.
    ///
    /// Ignored when `gas_token_enabled` is false.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::gas_token_min_base_fee_gwei")]
    pub gas_token_min_base_fee_gwei: u64,
}

impl Default for GasConfig {
    fn default() -> Self {
        Self {
            l2_buffer_factor: defaults::l2_buffer_factor(),
            l1_data_buffer_factor: defaults::l1_data_buffer_factor(),
            max_priority_fee_gwei: defaults::max_priority_fee_gwei(),
            conservative_fee_fraction: defaults::conservative_fee_fraction(),
            emergency_bundle_enabled: defaults::emergency_bundle_enabled(),
            gas_token_enabled: defaults::gas_token_enabled(),
            gas_token_min_base_fee_gwei: defaults::gas_token_min_base_fee_gwei(),
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// LaConfig
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Liquidation Arbitrage configuration (Â§11).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaConfig {
    /// Maximum number of positions in the hot-tier index.
    ///
    /// Spec Â§11.1: ~2,000â€“5,000 on Arbitrum in normal markets.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::la_hot_tier_max_positions")]
    pub hot_tier_max_positions: usize,

    /// Maximum number of positions in the warm-tier index.
    ///
    /// Spec Â§11.1: ~15,000â€“30,000 on Arbitrum.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::la_warm_tier_max_positions")]
    pub warm_tier_max_positions: usize,

    /// Maximum number of positions in the cold-tier index.
    ///
    /// Spec Â§11.1: ~100,000â€“200,000.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::la_cold_tier_max_positions")]
    pub cold_tier_max_positions: usize,

    /// Total position index capacity (all tiers combined).
    ///
    /// Spec Â§11.1: ~500,000 total.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::la_total_position_capacity")]
    pub total_position_capacity: usize,

    /// Warm-tier oracle price move threshold that triggers immediate
    /// recompute (Â§11.1).  In basis points.  Default 50 = 0.5%.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::la_warm_price_move_bps")]
    pub warm_price_move_threshold_bps: u16,

    /// Warm-tier batch recompute interval in milliseconds (Â§11.1).
    /// Default 200ms.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::la_warm_batch_interval_ms")]
    pub warm_batch_interval_ms: u64,

    /// Cold-tier lazy recompute interval in milliseconds (Â§11.1).
    /// Default 2,000ms (2 seconds).
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::la_cold_recompute_interval_ms")]
    pub cold_recompute_interval_ms: u64,

    /// Archived-tier cycle length in blocks (Â§11.1).  Default 500 blocks.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::la_archived_cycle_blocks")]
    pub archived_cycle_blocks: u64,

    /// Sequencer restart deduplication window in blocks (Â§11.3).
    /// Default 60 blocks (~15s on Arbitrum).
    ///
    /// NOTE: this is intentionally kept in config (not only in ChainId)
    /// so that it can be tuned per-deployment without a code change.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::la_sequencer_restart_window_blocks")]
    pub sequencer_restart_window_blocks: u64,
}

impl Default for LaConfig {
    fn default() -> Self {
        Self {
            hot_tier_max_positions: defaults::la_hot_tier_max_positions(),
            warm_tier_max_positions: defaults::la_warm_tier_max_positions(),
            cold_tier_max_positions: defaults::la_cold_tier_max_positions(),
            total_position_capacity: defaults::la_total_position_capacity(),
            warm_price_move_threshold_bps: defaults::la_warm_price_move_bps(),
            warm_batch_interval_ms: defaults::la_warm_batch_interval_ms(),
            cold_recompute_interval_ms: defaults::la_cold_recompute_interval_ms(),
            archived_cycle_blocks: defaults::la_archived_cycle_blocks(),
            sequencer_restart_window_blocks: defaults::la_sequencer_restart_window_blocks(),
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// RelayConfig
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Relay submission configuration (Â§11.2, Â§12).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    /// Maximum bundles submitted per relay per second (Â§11.2, fix C2).
    /// Default 4.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::relay_max_per_second")]
    pub max_bundles_per_relay_per_second: usize,

    /// Stagger delay between sequential bundle submissions in
    /// cascade mode, in milliseconds (Â§11.2).  Default 10ms.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::relay_stagger_ms")]
    pub cascade_stagger_ms: u64,

    /// Maximum number of relays in the cascade submission set (Â§11.2).
    /// Default 4.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::relay_cascade_max")]
    pub cascade_max_relays: usize,

    /// Tie-band width for LA-inclusion-rate ranking (Â§11.2).
    ///
    /// Relays within this fraction of the best inclusion rate are
    /// eligible for the randomised round-robin (anti-fingerprinting).
    /// Default 0.05 = 5%.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::relay_tie_band_fraction")]
    pub inclusion_rate_tie_band_fraction: f64,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            max_bundles_per_relay_per_second: defaults::relay_max_per_second(),
            cascade_stagger_ms: defaults::relay_stagger_ms(),
            cascade_max_relays: defaults::relay_cascade_max(),
            inclusion_rate_tie_band_fraction: defaults::relay_tie_band_fraction(),
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// MlConfig
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Gas model ML online learner configuration (Â§13).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlConfig {
    /// Learning rate for the online fee multiplier updates (Â§13).
    /// Default 0.01.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_learning_rate")]
    pub learning_rate: f64,

    /// Fraction of loss events held out for validation (Â§13.1, fix C1).
    /// Default 0.20 = 20%.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_validation_ratio")]
    pub validation_ratio: f64,

    /// Number of loss events between validation passes and checkpoint
    /// saves (Â§13.1).  Default 1,000.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_checkpoint_interval")]
    pub checkpoint_interval: u64,

    /// Maximum win-rate degradation below the last checkpoint before
    /// automatic model revert (Â§13.1, fix C1).
    ///
    /// Default 0.05 = 5 percentage points.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_revert_threshold")]
    pub revert_threshold: f64,

    /// Upper bound on fee multiplier (Â§13.3, fix I5).  Default 5.0.
    ///
    /// GOVERNANCE: L3 (48h timelock) â€” ceiling changes affect maximum
    /// gas spend; requires careful analysis.
    #[serde(default = "defaults::ml_multiplier_ceiling")]
    pub multiplier_ceiling: f64,

    /// Lower bound on fee multiplier (Â§13).  Default 0.3.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_multiplier_floor")]
    pub multiplier_floor: f64,

    /// Consecutive LOST_GAS_LOW events at the ceiling before triggering
    /// DEGRADED alert and model pause (Â§13.3, fix I5).  Default 100.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_ceiling_escalation_threshold")]
    pub ceiling_escalation_threshold: u64,

    /// Number of checkpoint files to retain on disk (Â§13.2, fix I1).
    /// Older files are pruned.  Default 10.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::ml_checkpoint_retention")]
    pub checkpoint_retention: usize,

    /// Directory for checkpoint files (Â§13.2).
    /// Default `/var/omega`.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::ml_checkpoint_dir")]
    pub checkpoint_dir: String,
}

impl Default for MlConfig {
    fn default() -> Self {
        Self {
            learning_rate: defaults::ml_learning_rate(),
            validation_ratio: defaults::ml_validation_ratio(),
            checkpoint_interval: defaults::ml_checkpoint_interval(),
            revert_threshold: defaults::ml_revert_threshold(),
            multiplier_ceiling: defaults::ml_multiplier_ceiling(),
            multiplier_floor: defaults::ml_multiplier_floor(),
            ceiling_escalation_threshold: defaults::ml_ceiling_escalation_threshold(),
            checkpoint_retention: defaults::ml_checkpoint_retention(),
            checkpoint_dir: defaults::ml_checkpoint_dir(),
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// RotationConfig
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Address rotation and relay reputation carryover configuration (Â§14).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RotationConfig {
    /// Exponential decay time constant for reputation carryover, in months
    /// (Â§14.1, fix C4 + I4).  Default 3.
    ///
    /// The authoritative formula from the Â§14.1 code block is:
    ///   `carryover_pct = base_carryover Ã— exp(-months_since_rotation / decay_rate_months)`
    ///
    /// With the default of 3 this produces:
    ///   0 months â†’ 50.0%,  3 months â†’ 18.4%
    ///
    /// NOTE: Â§14.1 refers to this as "half-life" but the spec's illustrative
    /// table values do not match a true half-life formula.  The CODE BLOCK in
    /// Â§14.1 is authoritative; this field is the divisor in that formula.
    /// The true half-life of the default configuration is 3 Ã— ln(2) â‰ˆ 2.08
    /// months, not 3 months.
    ///
    /// This value is a DIVISOR in the formula above â€” `validate()` now
    /// rejects a value â‰¤ 0.0, since zero would divide by zero (NaN) and
    /// a negative value would invert decay into growth, both silently
    /// corrupting every reputation-carryover calculation with no error
    /// raised anywhere near the actual computation.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::rotation_decay_rate_months")]
    pub reputation_decay_rate_months: f64,

    /// Base carryover fraction immediately after rotation (Â§14.1).
    /// Default 0.50 = 50%.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::rotation_base_carryover")]
    pub base_carryover_fraction: f64,
}

impl Default for RotationConfig {
    fn default() -> Self {
        Self {
            reputation_decay_rate_months: defaults::rotation_decay_rate_months(),
            base_carryover_fraction: defaults::rotation_base_carryover(),
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// VaultConfig
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Vault and PIL treasury parameters (Â§15).
///
/// ## WeiAmount instead of u64
///
/// `per_transfer_cap_wei` and `daily_cap_wei` are `WeiAmount`, not a raw
/// integer â€” see the module doc comment for why a plain integer field
/// (of any width) can't survive a TOML round-trip at the magnitude the
/// spec actually requires (50 ETH / 500 ETH), and why the previous `u64`
/// workaround silently collapsed both caps to the same reduced value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultConfig {
    /// DAO fee in basis points (Â§15.1).  Default 500 = 5%.
    /// Range: 0â€“1,000 bps (0â€“10%).  Certora invariant C9 enforces
    /// the 10% ceiling on-chain.
    ///
    /// GOVERNANCE: L3 (48h timelock).
    #[serde(default = "defaults::vault_dao_fee_bps")]
    pub dao_fee_bps: u16,

    /// Required on-chain confirmation depth before Vault releases profit
    /// (Â§15).  Minimum 12 â€” enforced by the Vault contract.
    ///
    /// IMMUTABLE (Vault contract enforces this; config value must
    /// match the deployed contract or OmegaError::Config is emitted).
    #[serde(default = "defaults::vault_confirmation_depth")]
    pub confirmation_depth: u8,

    /// Maximum profit released per single Vault transfer, in ETH wei.
    /// Default: 50 ETH (Â§15.2), expressed as the full spec value now
    /// that `WeiAmount` removes the TOML-integer magnitude limitation.
    ///
    /// GOVERNANCE: L3 (48h timelock).
    #[serde(default = "defaults::vault_per_transfer_cap_wei")]
    pub per_transfer_cap_wei: WeiAmount,

    /// Maximum aggregate profit released per 24h rolling window, in ETH
    /// wei. Default: 500 ETH (Â§15.2).
    ///
    /// GOVERNANCE: L3 (48h timelock).
    #[serde(default = "defaults::vault_daily_cap_wei")]
    pub daily_cap_wei: WeiAmount,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            dao_fee_bps: defaults::vault_dao_fee_bps(),
            confirmation_depth: defaults::vault_confirmation_depth(),
            per_transfer_cap_wei: defaults::vault_per_transfer_cap_wei(),
            daily_cap_wei: defaults::vault_daily_cap_wei(),
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ApiConfig
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Control-plane API configuration (Â§17, Â§17.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiConfig {
    /// WebSocket messages per minute for authenticated connections (Â§17.1,
    /// fix M4).  Default 300.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::api_ws_authed_rate")]
    pub ws_authenticated_msgs_per_min: u32,

    /// WebSocket messages per minute for anonymous connections (Â§17.1).
    /// Default 100.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::api_ws_anon_rate")]
    pub ws_anonymous_msgs_per_min: u32,

    /// TCP bind address for the control-plane HTTP server.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::api_bind_addr")]
    pub bind_addr: String,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            ws_authenticated_msgs_per_min: defaults::api_ws_authed_rate(),
            ws_anonymous_msgs_per_min: defaults::api_ws_anon_rate(),
            bind_addr: defaults::api_bind_addr(),
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Default value functions
//
// Each constant is a named function rather than a bare literal so that
// serde's `default = "..."` attribute can reference it and so that the
// value appears exactly once â€” no risk of the struct Default and the serde
// default drifting apart.
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

mod defaults {
    use super::WeiAmount;

    // â”€â”€ Top-level â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    pub fn active_phase() -> u8 {
        0
    }

    // â”€â”€ GasConfig â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    pub fn l2_buffer_factor() -> f64 {
        1.15
    }
    pub fn l1_data_buffer_factor() -> f64 {
        1.10
    }
    /// Spec Â§12.2: 500 gwei ceiling.
    pub fn max_priority_fee_gwei() -> u64 {
        500
    }
    pub fn conservative_fee_fraction() -> f64 {
        0.70
    }
    pub fn emergency_bundle_enabled() -> bool {
        true
    }
    /// Spec Â§18: disabled by default; evaluate for Phase 4+ L1 only.
    pub fn gas_token_enabled() -> bool {
        false
    }
    /// Spec Â§18: revisit if L1 base fee routinely exceeds 80 gwei.
    pub fn gas_token_min_base_fee_gwei() -> u64 {
        80
    }

    // â”€â”€ LaConfig â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Spec Â§11.1: ~2,000â€“5,000; use midpoint as default.
    pub fn la_hot_tier_max_positions() -> usize {
        5_000
    }
    /// Spec Â§11.1: ~15,000â€“30,000.
    pub fn la_warm_tier_max_positions() -> usize {
        30_000
    }
    /// Spec Â§11.1: ~100,000â€“200,000.
    pub fn la_cold_tier_max_positions() -> usize {
        200_000
    }
    /// Spec Â§11.1: ~500,000 total.
    pub fn la_total_position_capacity() -> usize {
        500_000
    }
    /// Spec Â§11.1: >0.5% price move triggers warm recompute.
    pub fn la_warm_price_move_bps() -> u16 {
        50
    }
    /// Spec Â§11.1: 200ms warm batch interval.
    pub fn la_warm_batch_interval_ms() -> u64 {
        200
    }
    /// Spec Â§11.1: 2s cold lazy interval.
    pub fn la_cold_recompute_interval_ms() -> u64 {
        2_000
    }
    /// Spec Â§11.1: 500-block archived cycle.
    pub fn la_archived_cycle_blocks() -> u64 {
        500
    }
    /// Spec Â§11.3: 60 blocks â‰ˆ 15s on Arbitrum.
    pub fn la_sequencer_restart_window_blocks() -> u64 {
        60
    }

    // â”€â”€ RelayConfig â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Spec Â§11.2 fix C2: max 4 bundles/relay/second.
    pub fn relay_max_per_second() -> usize {
        4
    }
    /// Spec Â§11.2: 10ms stagger between bundles.
    pub fn relay_stagger_ms() -> u64 {
        10
    }
    /// Spec Â§11.2: up to 4 relays in cascade.
    pub fn relay_cascade_max() -> usize {
        4
    }
    /// Spec Â§11.2 fix I2: 5% tie band for round-robin randomisation.
    pub fn relay_tie_band_fraction() -> f64 {
        0.05
    }

    // â”€â”€ MlConfig â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Spec Â§13.1: online learner learning rate.
    pub fn ml_learning_rate() -> f64 {
        0.01
    }
    /// Spec Â§13.1 fix C1: 20% holdout for validation.
    pub fn ml_validation_ratio() -> f64 {
        0.20
    }
    /// Spec Â§13.1: validate every 1,000 losses.
    pub fn ml_checkpoint_interval() -> u64 {
        1_000
    }
    /// Spec Â§13.1 fix C1: revert if holdout win rate drops >5%.
    pub fn ml_revert_threshold() -> f64 {
        0.05
    }
    /// Spec Â§13.3 fix I5: 5.0Ã— ceiling.
    pub fn ml_multiplier_ceiling() -> f64 {
        5.0
    }
    /// Spec Â§13: 0.3Ã— floor.
    pub fn ml_multiplier_floor() -> f64 {
        0.3
    }
    /// Spec Â§13.3 fix I5: 100 consecutive ceiling hits â†’ DEGRADED.
    pub fn ml_ceiling_escalation_threshold() -> u64 {
        100
    }
    /// Spec Â§13.2 fix I1: retain last 10 checkpoints.
    pub fn ml_checkpoint_retention() -> usize {
        10
    }
    /// Spec Â§13.2: checkpoint directory.
    pub fn ml_checkpoint_dir() -> String {
        "/var/omega".to_string()
    }

    // â”€â”€ RotationConfig â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Spec Â§14.1 code block: divisor in exp(-months/decay_rate).
    pub fn rotation_decay_rate_months() -> f64 {
        3.0
    }
    /// Spec Â§14.1 fix C4: 50% base carryover at rotation time.
    pub fn rotation_base_carryover() -> f64 {
        0.50
    }

    // â”€â”€ VaultConfig â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Spec Â§15.1: 500 bps = 5% DAO fee.
    pub fn vault_dao_fee_bps() -> u16 {
        500
    }
    /// Spec Â§15.2: minimum 12 confirmations.
    pub fn vault_confirmation_depth() -> u8 {
        12
    }
    /// Spec Â§15.2: 50 ETH per-transfer cap â€” the actual spec value, not
    /// an i64-representable approximation, now that WeiAmount stores
    /// this as a TOML string rather than a plain integer.
    pub fn vault_per_transfer_cap_wei() -> WeiAmount {
        WeiAmount::from_eth(50)
    }
    /// Spec Â§15.2: 500 ETH daily cap â€” the actual spec value.
    pub fn vault_daily_cap_wei() -> WeiAmount {
        WeiAmount::from_eth(500)
    }

    // â”€â”€ ApiConfig â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Spec Â§17.1 fix M4: 300/min authenticated.
    pub fn api_ws_authed_rate() -> u32 {
        300
    }
    /// Spec Â§17.1 fix M4: 100/min anonymous.
    pub fn api_ws_anon_rate() -> u32 {
        100
    }
    pub fn api_bind_addr() -> String {
        "0.0.0.0:8080".to_string()
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Validation
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

impl OmegaConfig {
    /// Validate that all config fields satisfy their invariants.
    ///
    /// Called at startup and after every hot-reload.  Returns a list of
    /// validation errors; an empty list means the config is valid.
    /// Callers should emit `OmegaError::Config` and halt if any errors
    /// are returned.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        // Phase gate
        if self.active_phase > 4 {
            errors.push(format!(
                "active_phase {} is invalid â€” must be 0â€“4",
                self.active_phase
            ));
        }

        // Gas model
        if !(1.0..=2.0).contains(&self.gas.l2_buffer_factor) {
            errors.push(format!(
                "gas.l2_buffer_factor {} out of range [1.0, 2.0]",
                self.gas.l2_buffer_factor
            ));
        }
        // Previously unvalidated: l1_data_buffer_factor feeds directly
        // into the same dual-component gas cost estimate as
        // l2_buffer_factor (Â§7) but had no range check at all â€” a
        // misconfigured value here (e.g. 0.0, or negative) would
        // silently under-cost every blueprint's L1 data component.
        if !(1.0..=2.0).contains(&self.gas.l1_data_buffer_factor) {
            errors.push(format!(
                "gas.l1_data_buffer_factor {} out of range [1.0, 2.0]",
                self.gas.l1_data_buffer_factor
            ));
        }
        if self.gas.max_priority_fee_gwei > 500 {
            errors.push(format!(
                "gas.max_priority_fee_gwei {} exceeds 500 gwei ceiling (Â§12.2)",
                self.gas.max_priority_fee_gwei
            ));
        }
        if !(0.0..=1.0).contains(&self.gas.conservative_fee_fraction) {
            errors.push(format!(
                "gas.conservative_fee_fraction {} out of range [0.0, 1.0]",
                self.gas.conservative_fee_fraction
            ));
        }

        // LA tier capacity â€” previously unchecked: nothing stopped
        // hot + warm + cold from exceeding total_position_capacity,
        // which is the kind of misconfiguration that produces an
        // inconsistent/unbounded index at runtime rather than a clear
        // startup error.
        let tier_sum = self
            .la
            .hot_tier_max_positions
            .saturating_add(self.la.warm_tier_max_positions)
            .saturating_add(self.la.cold_tier_max_positions);
        if tier_sum > self.la.total_position_capacity {
            errors.push(format!(
                "la: hot_tier_max_positions + warm_tier_max_positions + cold_tier_max_positions \
                 ({tier_sum}) exceeds total_position_capacity ({}) â€” the tier hierarchy assumes \
                 the sum leaves room for the archived tier within the total (Â§11.1)",
                self.la.total_position_capacity
            ));
        }

        // ML model
        if !(0.0..1.0).contains(&self.ml.validation_ratio) {
            errors.push(format!(
                "ml.validation_ratio {} out of range (0.0, 1.0)",
                self.ml.validation_ratio
            ));
        }
        if self.ml.multiplier_ceiling > 5.0 {
            errors.push(format!(
                "ml.multiplier_ceiling {} exceeds 5.0 (Â§13.3)",
                self.ml.multiplier_ceiling
            ));
        }
        if self.ml.multiplier_floor < 0.1 {
            errors.push(format!(
                "ml.multiplier_floor {} below 0.1 â€” would suppress all gas bids",
                self.ml.multiplier_floor
            ));
        }
        // Previously unchecked: floor and ceiling were each validated
        // independently, but nothing stopped floor > ceiling as a pair
        // (e.g. ceiling overridden to 2.0 while floor stays at a
        // default that's individually valid but now above it) â€” an
        // inverted [floor, ceiling] range downstream (e.g. a
        // `.clamp(floor, ceiling)` call) either panics or silently
        // produces a nonsensical multiplier.
        if self.ml.multiplier_floor > self.ml.multiplier_ceiling {
            errors.push(format!(
                "ml.multiplier_floor ({}) exceeds ml.multiplier_ceiling ({}) â€” \
                 this range is inverted",
                self.ml.multiplier_floor, self.ml.multiplier_ceiling
            ));
        }
        if self.ml.checkpoint_interval == 0 {
            errors.push("ml.checkpoint_interval must be > 0".to_string());
        }
        // Previously unvalidated.
        if !(0.0..=1.0).contains(&self.ml.learning_rate) {
            errors.push(format!(
                "ml.learning_rate {} out of range [0.0, 1.0]",
                self.ml.learning_rate
            ));
        }
        if !(0.0..=1.0).contains(&self.ml.revert_threshold) {
            errors.push(format!(
                "ml.revert_threshold {} out of range [0.0, 1.0]",
                self.ml.revert_threshold
            ));
        }

        // Rotation â€” previously unvalidated. reputation_decay_rate_months
        // is a DIVISOR in the carryover formula (see RotationConfig doc
        // comment); zero or negative silently corrupts every carryover
        // calculation (division by zero â†’ NaN, or inverted decay into
        // growth) with no error anywhere near the actual computation.
        if self.rotation.reputation_decay_rate_months <= 0.0 {
            errors.push(format!(
                "rotation.reputation_decay_rate_months {} must be > 0.0 â€” it is a divisor \
                 in the carryover formula (Â§14.1); zero or negative corrupts every \
                 reputation calculation silently (NaN or inverted decay)",
                self.rotation.reputation_decay_rate_months
            ));
        }
        if !(0.0..=1.0).contains(&self.rotation.base_carryover_fraction) {
            errors.push(format!(
                "rotation.base_carryover_fraction {} out of range [0.0, 1.0]",
                self.rotation.base_carryover_fraction
            ));
        }

        // Vault
        if self.vault.dao_fee_bps > 1_000 {
            errors.push(format!(
                "vault.dao_fee_bps {} exceeds 1,000 bps (10%) ceiling (Â§15.1, Certora C9)",
                self.vault.dao_fee_bps
            ));
        }
        if self.vault.confirmation_depth < 12 {
            errors.push(format!(
                "vault.confirmation_depth {} is below 12 â€” Vault contract will reject (Â§15.2)",
                self.vault.confirmation_depth
            ));
        }
        // Previously unvalidated: nothing stopped either cap from being
        // zero, and nothing stopped per_transfer_cap_wei from exceeding
        // daily_cap_wei â€” a single transfer capped higher than the
        // supposed daily aggregate limit defeats the purpose of having
        // two separate limits (Â§15.2).
        if self.vault.per_transfer_cap_wei == WeiAmount::ZERO {
            errors.push("vault.per_transfer_cap_wei must be > 0".to_string());
        }
        if self.vault.daily_cap_wei == WeiAmount::ZERO {
            errors.push("vault.daily_cap_wei must be > 0".to_string());
        }
        if self.vault.per_transfer_cap_wei > self.vault.daily_cap_wei {
            errors.push(format!(
                "vault.per_transfer_cap_wei ({} wei) exceeds vault.daily_cap_wei ({} wei) â€” \
                 a single transfer could not legitimately exceed the daily aggregate cap (Â§15.2)",
                self.vault.per_transfer_cap_wei, self.vault.daily_cap_wei
            ));
        }

        // Relay
        if self.relay.cascade_max_relays == 0 {
            errors.push("relay.cascade_max_relays must be â‰¥ 1".to_string());
        }
        if self.relay.cascade_stagger_ms == 0 {
            errors.push(
                "relay.cascade_stagger_ms must be > 0 â€” zero stagger re-introduces C2 (Â§11.2)"
                    .to_string(),
            );
        }
        // Previously unvalidated.
        if !(0.0..=1.0).contains(&self.relay.inclusion_rate_tie_band_fraction) {
            errors.push(format!(
                "relay.inclusion_rate_tie_band_fraction {} out of range [0.0, 1.0]",
                self.relay.inclusion_rate_tie_band_fraction
            ));
        }

        // API
        if self.api.ws_authenticated_msgs_per_min == 0 {
            errors.push("api.ws_authenticated_msgs_per_min must be > 0".to_string());
        }
        // Previously only the authenticated rate was checked for zero;
        // the anonymous rate had the identical failure mode unchecked.
        if self.api.ws_anonymous_msgs_per_min == 0 {
            errors.push("api.ws_anonymous_msgs_per_min must be > 0".to_string());
        }

        errors
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let cfg = OmegaConfig::default();
        let errors = cfg.validate();
        assert!(
            errors.is_empty(),
            "Default config failed validation: {:?}",
            errors
        );
    }

    #[test]
    fn dao_fee_ceiling_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.vault.dao_fee_bps = 1_001;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("dao_fee_bps")));
    }

    #[test]
    fn priority_fee_ceiling_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.gas.max_priority_fee_gwei = 501;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("max_priority_fee_gwei")));
    }

    #[test]
    fn ml_ceiling_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.ml.multiplier_ceiling = 5.1;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("multiplier_ceiling")));
    }

    #[test]
    fn vault_confirmation_depth_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.vault.confirmation_depth = 11;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("confirmation_depth")));
    }

    #[test]
    fn zero_cascade_stagger_rejected() {
        let mut cfg = OmegaConfig::default();
        cfg.relay.cascade_stagger_ms = 0;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("cascade_stagger_ms")));
    }

    #[test]
    fn l1_data_buffer_factor_range_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.gas.l1_data_buffer_factor = 0.5;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("l1_data_buffer_factor")));
    }

    #[test]
    fn ml_floor_exceeding_ceiling_rejected() {
        let mut cfg = OmegaConfig::default();
        cfg.ml.multiplier_ceiling = 2.0;
        cfg.ml.multiplier_floor = 3.0; // individually >= 0.1, but now > ceiling
        let errors = cfg.validate();
        assert!(errors
            .iter()
            .any(|e| e.contains("multiplier_floor") && e.contains("exceeds")));
    }

    #[test]
    fn ml_learning_rate_range_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.ml.learning_rate = 1.5;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("learning_rate")));
    }

    #[test]
    fn ml_revert_threshold_range_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.ml.revert_threshold = -0.1;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("revert_threshold")));
    }

    #[test]
    fn rotation_decay_rate_must_be_positive() {
        let mut cfg = OmegaConfig::default();
        cfg.rotation.reputation_decay_rate_months = 0.0;
        let errors = cfg.validate();
        assert!(errors
            .iter()
            .any(|e| e.contains("reputation_decay_rate_months")));

        let mut cfg2 = OmegaConfig::default();
        cfg2.rotation.reputation_decay_rate_months = -1.0;
        let errors2 = cfg2.validate();
        assert!(errors2
            .iter()
            .any(|e| e.contains("reputation_decay_rate_months")));
    }

    #[test]
    fn rotation_carryover_fraction_range_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.rotation.base_carryover_fraction = 1.5;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("base_carryover_fraction")));
    }

    #[test]
    fn relay_tie_band_range_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.relay.inclusion_rate_tie_band_fraction = 1.2;
        let errors = cfg.validate();
        assert!(errors
            .iter()
            .any(|e| e.contains("inclusion_rate_tie_band_fraction")));
    }

    #[test]
    fn la_tier_capacity_ordering_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.la.total_position_capacity = 1_000; // far below hot+warm+cold defaults
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("total_position_capacity")));
    }

    #[test]
    fn api_anonymous_rate_enforced() {
        let mut cfg = OmegaConfig::default();
        cfg.api.ws_anonymous_msgs_per_min = 0;
        let errors = cfg.validate();
        assert!(errors
            .iter()
            .any(|e| e.contains("ws_anonymous_msgs_per_min")));
    }

    #[test]
    fn vault_cap_zero_rejected() {
        let mut cfg = OmegaConfig::default();
        cfg.vault.per_transfer_cap_wei = WeiAmount::ZERO;
        let errors = cfg.validate();
        assert!(errors.iter().any(|e| e.contains("per_transfer_cap_wei")));
    }

    #[test]
    fn vault_per_transfer_exceeding_daily_rejected() {
        let mut cfg = OmegaConfig::default();
        cfg.vault.per_transfer_cap_wei = WeiAmount::from_eth(600);
        cfg.vault.daily_cap_wei = WeiAmount::from_eth(500);
        let errors = cfg.validate();
        assert!(errors
            .iter()
            .any(|e| e.contains("exceeds vault.daily_cap_wei")));
    }

    #[test]
    fn vault_cap_defaults_match_full_spec_values() {
        // Regression test for the bug this fix addresses: the previous
        // u64-typed defaults were both silently reduced to ~9 ETH (the
        // largest TOML-representable magnitude) instead of the spec's
        // 50 ETH / 500 ETH â€” and, worse, were numerically IDENTICAL to
        // each other. WeiAmount removes that constraint entirely.
        let cfg = OmegaConfig::default();
        assert_eq!(
            cfg.vault.per_transfer_cap_wei.as_wei(),
            50_000_000_000_000_000_000
        );
        assert_eq!(
            cfg.vault.daily_cap_wei.as_wei(),
            500_000_000_000_000_000_000
        );
        assert!(cfg.vault.per_transfer_cap_wei < cfg.vault.daily_cap_wei);
    }

    #[test]
    fn wei_amount_round_trips_through_real_toml() {
        // This is the test that actually proves the original bug is
        // fixed: a plain u64/u128 field would fail this exact
        // round-trip for any value beyond i64::MAX, since the TOML
        // *parser* rejects the literal before serde even runs.
        let cfg = OmegaConfig::default();
        let toml_str = toml::to_string(&cfg).expect("serialize to TOML");
        let parsed: OmegaConfig = toml::from_str(&toml_str).expect("deserialize from TOML");
        assert_eq!(
            parsed.vault.per_transfer_cap_wei,
            cfg.vault.per_transfer_cap_wei
        );
        assert_eq!(parsed.vault.daily_cap_wei, cfg.vault.daily_cap_wei);
    }

    #[test]
    fn wei_amount_deserializes_from_plain_integer_too() {
        #[derive(Deserialize)]
        struct Wrapper {
            amount: WeiAmount,
        }
        let parsed: Wrapper = serde_json::from_str(r#"{"amount": 12345}"#).unwrap();
        assert_eq!(parsed.amount.as_wei(), 12345);
    }

    #[test]
    fn wei_amount_rejects_garbage_string() {
        // `amount` is intentionally unread below: this test only proves
        // deserialization FAILS for a malformed string, so `Wrapper` is
        // never successfully constructed â€” there is no `Ok` value to
        // read `.amount` from. The field's presence is what drives
        // `WeiAmount`'s custom `Deserialize` impl under test here, not
        // any value read from it. See this file's module-level "Fix
        // (this revision)" note for why `#[allow(dead_code)]` is the
        // correct fix rather than changing this test's behavior.
        #[allow(dead_code)]
        #[derive(Deserialize)]
        struct Wrapper {
            amount: WeiAmount,
        }
        let result: Result<Wrapper, _> = serde_json::from_str(r#"{"amount": "not_a_number"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn unknown_top_level_field_rejected() {
        // Regression test for the top-level deny_unknown_fields gap:
        // every sub-config already rejected unknown fields, but
        // OmegaConfig itself did not.
        let bad_toml = r#"
            active_phase = 1
            bogus_top_level_field = true
        "#;
        let result: Result<OmegaConfig, _> = toml::from_str(bad_toml);
        assert!(
            result.is_err(),
            "unknown top-level field must now be rejected"
        );
    }

    #[test]
    fn default_values_match_spec() {
        let cfg = OmegaConfig::default();
        // Â§12.2
        assert_eq!(cfg.gas.max_priority_fee_gwei, 500);
        // Â§11.2 fix C2
        assert_eq!(cfg.relay.max_bundles_per_relay_per_second, 4);
        assert_eq!(cfg.relay.cascade_stagger_ms, 10);
        // Â§13.1 fix C1
        assert!((cfg.ml.validation_ratio - 0.20).abs() < f64::EPSILON);
        assert_eq!(cfg.ml.checkpoint_interval, 1_000);
        // Â§13.3 fix I5
        assert_eq!(cfg.ml.ceiling_escalation_threshold, 100);
        // Â§14.1 fix C4
        assert!((cfg.rotation.base_carryover_fraction - 0.50).abs() < f64::EPSILON);
        assert!((cfg.rotation.reputation_decay_rate_months - 3.0).abs() < f64::EPSILON);
        // Â§15.1
        assert_eq!(cfg.vault.dao_fee_bps, 500);
        assert_eq!(cfg.vault.confirmation_depth, 12);
        // Â§17.1 fix M4
        assert_eq!(cfg.api.ws_authenticated_msgs_per_min, 300);
        assert_eq!(cfg.api.ws_anonymous_msgs_per_min, 100);
        // Â§11.1
        assert_eq!(cfg.la.warm_batch_interval_ms, 200);
        assert_eq!(cfg.la.archived_cycle_blocks, 500);
        // Â§11.3
        assert_eq!(cfg.la.sequencer_restart_window_blocks, 60);
    }
}
