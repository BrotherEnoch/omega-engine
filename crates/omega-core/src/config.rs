// crates/omega-core/src/config.rs
// crates/omega-core/src/config.rs
//
// OmegaConfig — runtime configuration for the Omega Engine.
//
// ## Governance tiers (§5)
//
// Every field is annotated with the governance tier required to change it
// in production.  The tiers and their properties are:
//
//   L1 (Operator)    Hot-reload via POST /api/v1/config.  No timelock.
//                    Risk: low.  Scope: operational knobs (log levels,
//                    metrics endpoints, relay timeouts).
//
//   L2 (Fast-Approve) Signed by ≥2/5 governance keys.  Effective
//                    immediately after signature validation.
//                    Risk: medium.  Scope: strategy parameters, fee
//                    ceilings, ML learning rate.
//
//   L3 (Timelock)   48-hour timelock + 3/5 multisig.  Risk: high.
//                   Scope: phase gates, Vault parameters, DAO fee bps.
//                   Emergency L3 path: 24h + 3/5, qualifying criteria
//                   only (§5.1).
//
// Fields that are IMMUTABLE after deployment (chain_id, contract
// addresses that are Certora-verified invariants) are marked IMMUTABLE.
// They can only change via a full redeployment.
//
// ## Serialisation contract
//
// OmegaConfig is loaded from a TOML file at startup and can be
// hot-reloaded via the control-plane API (§17).  All defaults are set
// via serde's `default` attribute so that a minimal config file works
// in development.  Production deployments must provide all L3 fields
// explicitly — missing L3 fields cause a `OmegaError::Config` halt.
//
// Spec references:
//   §1.1  — phase gates → active_phase
//   §5    — governance tiers
//   §5.1  — emergency L3 criteria
//   §7    — dual-component gas model → GasConfig
//   §11.1 — LA tier thresholds → LaConfig
//   §11.2 — cascade backpressure → RelayConfig
//   §12.1 — emergency bundle profit check → GasConfig
//   §12.2 — 500 gwei priority fee ceiling → GasConfig
//   §13   — ML online learner → MlConfig
//   §14   — address rotation → RotationConfig
//   §15   — Vault parameters → VaultConfig
//   §17.1 — WebSocket rate limits → ApiConfig
//   §18   — CHI/GST gas tokens (Phase 4+ L1 only) → GasConfig

use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// Top-level config
// ─────────────────────────────────────────────────────────────────────────────

/// Full runtime configuration for the Omega Engine.
///
/// Loaded from `config/omega.toml` at startup.  Hot-reloaded via
/// `POST /api/v1/config` (L1 fields only; L2/L3 fields require the
/// appropriate governance signature).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OmegaConfig {
    /// Currently active system phase (§1.1, §20).
    ///
    /// GOVERNANCE: L3 (48h timelock).  Phase activation is a
    /// high-stakes irreversible operation — once Phase 3 is active,
    /// Phase 2 strategies continue running alongside it.
    ///
    /// Valid values: 0 (Shadow/Backtest), 1 (SA), 2 (MSA), 3 (LA), 4 (MEV).
    #[serde(default = "defaults::active_phase")]
    pub active_phase: u8,

    /// Gas model configuration (§7, §12.1, §12.2).
    #[serde(default)]
    pub gas: GasConfig,

    /// LA-specific configuration (§11).
    #[serde(default)]
    pub la: LaConfig,

    /// Relay submission configuration (§11.2, §12).
    #[serde(default)]
    pub relay: RelayConfig,

    /// ML online learner configuration (§13).
    #[serde(default)]
    pub ml: MlConfig,

    /// Address rotation configuration (§14).
    #[serde(default)]
    pub rotation: RotationConfig,

    /// Vault and PIL treasury configuration (§15).
    #[serde(default)]
    pub vault: VaultConfig,

    /// Control-plane API configuration (§17).
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

// ─────────────────────────────────────────────────────────────────────────────
// GasConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Arbitrum dual-component gas model parameters (§7, §12.1, §12.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GasConfig {
    /// L2 execution gas estimate buffer factor.
    ///
    /// Applied as: `l2_gas_budget = l2_exec_estimate × l2_buffer_factor`.
    /// Default 1.15 = 15% headroom.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::l2_buffer_factor")]
    pub l2_buffer_factor: f64,

    /// L1 data gas estimate buffer factor.
    ///
    /// Applied to the calldata-bytes × 16 estimate.  Default 1.10 = 10%.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::l1_data_buffer_factor")]
    pub l1_data_buffer_factor: f64,

    /// Maximum priority fee submitted to the Arbitrum sequencer, in gwei.
    ///
    /// Spec §12.2: 500 gwei ceiling.  At Arbitrum's 250ms block time
    /// this is ~0.0105 ETH per block — comparable to 50 gwei on L1.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::max_priority_fee_gwei")]
    pub max_priority_fee_gwei: u64,

    /// Conservative bundle fee as a fraction of the cap (0.0–1.0).
    ///
    /// Spec §12: conservative_fee = cap × conservative_fee_fraction.
    /// Default 0.70 = 70% of cap.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::conservative_fee_fraction")]
    pub conservative_fee_fraction: f64,

    /// Whether to enable emergency bundle emission (§12.1).
    ///
    /// When true, a third bundle at 2× cap is emitted IFF
    /// `expected_profit_net > emergency_gas_cost + dynamic_min_profit`.
    /// The profit check is MANDATORY — bundles are never submitted at a
    /// loss (fix M2).
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::emergency_bundle_enabled")]
    pub emergency_bundle_enabled: bool,

    /// Whether to evaluate CHI/GST gas token redemption (§18).
    ///
    /// Applicable on Ethereum L1 (Phase 4+) only.  Must be `false` on
    /// Arbitrum and Base — EVM storage refunds do not exist there.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::gas_token_enabled")]
    pub gas_token_enabled: bool,

    /// Minimum L1 base fee in gwei above which CHI/GST redemption is
    /// evaluated (§18).  Spec recommendation: 80 gwei.
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

// ─────────────────────────────────────────────────────────────────────────────
// LaConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Liquidation Arbitrage configuration (§11).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaConfig {
    /// Maximum number of positions in the hot-tier index.
    ///
    /// Spec §11.1: ~2,000–5,000 on Arbitrum in normal markets.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::la_hot_tier_max_positions")]
    pub hot_tier_max_positions: usize,

    /// Maximum number of positions in the warm-tier index.
    ///
    /// Spec §11.1: ~15,000–30,000 on Arbitrum.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::la_warm_tier_max_positions")]
    pub warm_tier_max_positions: usize,

    /// Maximum number of positions in the cold-tier index.
    ///
    /// Spec §11.1: ~100,000–200,000.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::la_cold_tier_max_positions")]
    pub cold_tier_max_positions: usize,

    /// Total position index capacity (all tiers combined).
    ///
    /// Spec §11.1: ~500,000 total.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::la_total_position_capacity")]
    pub total_position_capacity: usize,

    /// Warm-tier oracle price move threshold that triggers immediate
    /// recompute (§11.1).  In basis points.  Default 50 = 0.5%.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::la_warm_price_move_bps")]
    pub warm_price_move_threshold_bps: u16,

    /// Warm-tier batch recompute interval in milliseconds (§11.1).
    /// Default 200ms.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::la_warm_batch_interval_ms")]
    pub warm_batch_interval_ms: u64,

    /// Cold-tier lazy recompute interval in milliseconds (§11.1).
    /// Default 2,000ms (2 seconds).
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::la_cold_recompute_interval_ms")]
    pub cold_recompute_interval_ms: u64,

    /// Archived-tier cycle length in blocks (§11.1).  Default 500 blocks.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::la_archived_cycle_blocks")]
    pub archived_cycle_blocks: u64,

    /// Sequencer restart deduplication window in blocks (§11.3).
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

// ─────────────────────────────────────────────────────────────────────────────
// RelayConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Relay submission configuration (§11.2, §12).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    /// Maximum bundles submitted per relay per second (§11.2, fix C2).
    /// Default 4.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::relay_max_per_second")]
    pub max_bundles_per_relay_per_second: usize,

    /// Stagger delay between sequential bundle submissions in
    /// cascade mode, in milliseconds (§11.2).  Default 10ms.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::relay_stagger_ms")]
    pub cascade_stagger_ms: u64,

    /// Maximum number of relays in the cascade submission set (§11.2).
    /// Default 4.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::relay_cascade_max")]
    pub cascade_max_relays: usize,

    /// Tie-band width for LA-inclusion-rate ranking (§11.2).
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

// ─────────────────────────────────────────────────────────────────────────────
// MlConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Gas model ML online learner configuration (§13).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlConfig {
    /// Learning rate for the online fee multiplier updates (§13).
    /// Default 0.01.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_learning_rate")]
    pub learning_rate: f64,

    /// Fraction of loss events held out for validation (§13.1, fix C1).
    /// Default 0.20 = 20%.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_validation_ratio")]
    pub validation_ratio: f64,

    /// Number of loss events between validation passes and checkpoint
    /// saves (§13.1).  Default 1,000.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_checkpoint_interval")]
    pub checkpoint_interval: u64,

    /// Maximum win-rate degradation below the last checkpoint before
    /// automatic model revert (§13.1, fix C1).
    ///
    /// Default 0.05 = 5 percentage points.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_revert_threshold")]
    pub revert_threshold: f64,

    /// Upper bound on fee multiplier (§13.3, fix I5).  Default 5.0.
    ///
    /// GOVERNANCE: L3 (48h timelock) — ceiling changes affect maximum
    /// gas spend; requires careful analysis.
    #[serde(default = "defaults::ml_multiplier_ceiling")]
    pub multiplier_ceiling: f64,

    /// Lower bound on fee multiplier (§13).  Default 0.3.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_multiplier_floor")]
    pub multiplier_floor: f64,

    /// Consecutive LOST_GAS_LOW events at the ceiling before triggering
    /// DEGRADED alert and model pause (§13.3, fix I5).  Default 100.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::ml_ceiling_escalation_threshold")]
    pub ceiling_escalation_threshold: u64,

    /// Number of checkpoint files to retain on disk (§13.2, fix I1).
    /// Older files are pruned.  Default 10.
    ///
    /// GOVERNANCE: L1 (operator).
    #[serde(default = "defaults::ml_checkpoint_retention")]
    pub checkpoint_retention: usize,

    /// Directory for checkpoint files (§13.2).
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

// ─────────────────────────────────────────────────────────────────────────────
// RotationConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Address rotation and relay reputation carryover configuration (§14).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RotationConfig {
    /// Exponential decay time constant for reputation carryover, in months
    /// (§14.1, fix C4 + I4).  Default 3.
    ///
    /// The authoritative formula from the §14.1 code block is:
    ///   `carryover_pct = base_carryover × exp(-months_since_rotation / decay_rate_months)`
    ///
    /// With the default of 3 this produces:
    ///   0 months → 50.0%,  3 months → 18.4%
    ///
    /// NOTE: §14.1 refers to this as "half-life" but the spec's illustrative
    /// table values do not match a true half-life formula.  The CODE BLOCK in
    /// §14.1 is authoritative; this field is the divisor in that formula.
    /// The true half-life of the default configuration is 3 × ln(2) ≈ 2.08
    /// months, not 3 months.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::rotation_decay_rate_months")]
    pub reputation_decay_rate_months: f64,

    /// Base carryover fraction immediately after rotation (§14.1).
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

// ─────────────────────────────────────────────────────────────────────────────
// VaultConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Vault and PIL treasury parameters (§15).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultConfig {
    /// DAO fee in basis points (§15.1).  Default 500 = 5%.
    /// Range: 0–1,000 bps (0–10%).  Certora invariant C9 enforces
    /// the 10% ceiling on-chain.
    ///
    /// GOVERNANCE: L3 (48h timelock).
    #[serde(default = "defaults::vault_dao_fee_bps")]
    pub dao_fee_bps: u16,

    /// Required on-chain confirmation depth before Vault releases profit
    /// (§15).  Minimum 12 — enforced by the Vault contract.
    ///
    /// IMMUTABLE (Vault contract enforces this; config value must
    /// match the deployed contract or OmegaError::Config is emitted).
    #[serde(default = "defaults::vault_confirmation_depth")]
    pub confirmation_depth: u8,

    /// Maximum profit released per single Vault transfer, in ETH wei.
    /// Default 50 ETH = 50_000_000_000_000_000_000 wei.
    ///
    /// GOVERNANCE: L3 (48h timelock).
    #[serde(default = "defaults::vault_per_transfer_cap_wei")]
    pub per_transfer_cap_wei: u128,

    /// Maximum aggregate profit released per 24h rolling window, in ETH
    /// wei.  Default 500 ETH.
    ///
    /// GOVERNANCE: L3 (48h timelock).
    #[serde(default = "defaults::vault_daily_cap_wei")]
    pub daily_cap_wei: u128,
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

// ─────────────────────────────────────────────────────────────────────────────
// ApiConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Control-plane API configuration (§17, §17.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiConfig {
    /// WebSocket messages per minute for authenticated connections (§17.1,
    /// fix M4).  Default 300.
    ///
    /// GOVERNANCE: L2 (fast-approve).
    #[serde(default = "defaults::api_ws_authed_rate")]
    pub ws_authenticated_msgs_per_min: u32,

    /// WebSocket messages per minute for anonymous connections (§17.1).
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

// ─────────────────────────────────────────────────────────────────────────────
// Default value functions
//
// Each constant is a named function rather than a bare literal so that
// serde's `default = "..."` attribute can reference it and so that the
// value appears exactly once — no risk of the struct Default and the serde
// default drifting apart.
// ─────────────────────────────────────────────────────────────────────────────

mod defaults {
    // ── Top-level ─────────────────────────────────────────────────────────
    pub fn active_phase() -> u8 {
        0
    }

    // ── GasConfig ─────────────────────────────────────────────────────────
    pub fn l2_buffer_factor() -> f64 {
        1.15
    }
    pub fn l1_data_buffer_factor() -> f64 {
        1.10
    }
    /// Spec §12.2: 500 gwei ceiling.
    pub fn max_priority_fee_gwei() -> u64 {
        500
    }
    pub fn conservative_fee_fraction() -> f64 {
        0.70
    }
    pub fn emergency_bundle_enabled() -> bool {
        true
    }
    /// Spec §18: disabled by default; evaluate for Phase 4+ L1 only.
    pub fn gas_token_enabled() -> bool {
        false
    }
    /// Spec §18: revisit if L1 base fee routinely exceeds 80 gwei.
    pub fn gas_token_min_base_fee_gwei() -> u64 {
        80
    }

    // ── LaConfig ──────────────────────────────────────────────────────────
    /// Spec §11.1: ~2,000–5,000; use midpoint as default.
    pub fn la_hot_tier_max_positions() -> usize {
        5_000
    }
    /// Spec §11.1: ~15,000–30,000.
    pub fn la_warm_tier_max_positions() -> usize {
        30_000
    }
    /// Spec §11.1: ~100,000–200,000.
    pub fn la_cold_tier_max_positions() -> usize {
        200_000
    }
    /// Spec §11.1: ~500,000 total.
    pub fn la_total_position_capacity() -> usize {
        500_000
    }
    /// Spec §11.1: >0.5% price move triggers warm recompute.
    pub fn la_warm_price_move_bps() -> u16 {
        50
    }
    /// Spec §11.1: 200ms warm batch interval.
    pub fn la_warm_batch_interval_ms() -> u64 {
        200
    }
    /// Spec §11.1: 2s cold lazy interval.
    pub fn la_cold_recompute_interval_ms() -> u64 {
        2_000
    }
    /// Spec §11.1: 500-block archived cycle.
    pub fn la_archived_cycle_blocks() -> u64 {
        500
    }
    /// Spec §11.3: 60 blocks ≈ 15s on Arbitrum.
    pub fn la_sequencer_restart_window_blocks() -> u64 {
        60
    }

    // ── RelayConfig ───────────────────────────────────────────────────────
    /// Spec §11.2 fix C2: max 4 bundles/relay/second.
    pub fn relay_max_per_second() -> usize {
        4
    }
    /// Spec §11.2: 10ms stagger between bundles.
    pub fn relay_stagger_ms() -> u64 {
        10
    }
    /// Spec §11.2: up to 4 relays in cascade.
    pub fn relay_cascade_max() -> usize {
        4
    }
    /// Spec §11.2 fix I2: 5% tie band for round-robin randomisation.
    pub fn relay_tie_band_fraction() -> f64 {
        0.05
    }

    // ── MlConfig ──────────────────────────────────────────────────────────
    /// Spec §13.1: online learner learning rate.
    pub fn ml_learning_rate() -> f64 {
        0.01
    }
    /// Spec §13.1 fix C1: 20% holdout for validation.
    pub fn ml_validation_ratio() -> f64 {
        0.20
    }
    /// Spec §13.1: validate every 1,000 losses.
    pub fn ml_checkpoint_interval() -> u64 {
        1_000
    }
    /// Spec §13.1 fix C1: revert if holdout win rate drops >5%.
    pub fn ml_revert_threshold() -> f64 {
        0.05
    }
    /// Spec §13.3 fix I5: 5.0× ceiling.
    pub fn ml_multiplier_ceiling() -> f64 {
        5.0
    }
    /// Spec §13: 0.3× floor.
    pub fn ml_multiplier_floor() -> f64 {
        0.3
    }
    /// Spec §13.3 fix I5: 100 consecutive ceiling hits → DEGRADED.
    pub fn ml_ceiling_escalation_threshold() -> u64 {
        100
    }
    /// Spec §13.2 fix I1: retain last 10 checkpoints.
    pub fn ml_checkpoint_retention() -> usize {
        10
    }
    /// Spec §13.2: checkpoint directory.
    pub fn ml_checkpoint_dir() -> String {
        "/var/omega".to_string()
    }

    // ── RotationConfig ────────────────────────────────────────────────────
    /// Spec §14.1 code block: divisor in exp(-months/decay_rate).
    /// The spec calls this "half-life" but the formula uses it as a
    /// time constant — see RotationConfig doc for full explanation.
    pub fn rotation_decay_rate_months() -> f64 {
        3.0
    }
    /// Spec §14.1 fix C4: 50% base carryover at rotation time.
    pub fn rotation_base_carryover() -> f64 {
        0.50
    }

    // ── VaultConfig ───────────────────────────────────────────────────────
    /// Spec §15.1: 500 bps = 5% DAO fee.
    pub fn vault_dao_fee_bps() -> u16 {
        500
    }
    /// Spec §15.2: minimum 12 confirmations.
    pub fn vault_confirmation_depth() -> u8 {
        12
    }
    /// Spec §15.2: 50 ETH per-transfer cap.
    pub fn vault_per_transfer_cap_wei() -> u128 {
        50_000_000_000_000_000_000
    }
    /// Spec §15.2: 500 ETH daily cap.
    pub fn vault_daily_cap_wei() -> u128 {
        500_000_000_000_000_000_000
    }

    // ── ApiConfig ─────────────────────────────────────────────────────────
    /// Spec §17.1 fix M4: 300/min authenticated.
    pub fn api_ws_authed_rate() -> u32 {
        300
    }
    /// Spec §17.1 fix M4: 100/min anonymous.
    pub fn api_ws_anon_rate() -> u32 {
        100
    }
    pub fn api_bind_addr() -> String {
        "0.0.0.0:8080".to_string()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Validation
// ─────────────────────────────────────────────────────────────────────────────

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
                "active_phase {} is invalid — must be 0–4",
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
        if self.gas.max_priority_fee_gwei > 500 {
            errors.push(format!(
                "gas.max_priority_fee_gwei {} exceeds 500 gwei ceiling (§12.2)",
                self.gas.max_priority_fee_gwei
            ));
        }
        if !(0.0..=1.0).contains(&self.gas.conservative_fee_fraction) {
            errors.push(format!(
                "gas.conservative_fee_fraction {} out of range [0.0, 1.0]",
                self.gas.conservative_fee_fraction
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
                "ml.multiplier_ceiling {} exceeds 5.0 (§13.3)",
                self.ml.multiplier_ceiling
            ));
        }
        if self.ml.multiplier_floor < 0.1 {
            errors.push(format!(
                "ml.multiplier_floor {} below 0.1 — would suppress all gas bids",
                self.ml.multiplier_floor
            ));
        }
        if self.ml.checkpoint_interval == 0 {
            errors.push("ml.checkpoint_interval must be > 0".to_string());
        }

        // Vault
        if self.vault.dao_fee_bps > 1_000 {
            errors.push(format!(
                "vault.dao_fee_bps {} exceeds 1,000 bps (10%) ceiling (§15.1, Certora C9)",
                self.vault.dao_fee_bps
            ));
        }
        if self.vault.confirmation_depth < 12 {
            errors.push(format!(
                "vault.confirmation_depth {} is below 12 — Vault contract will reject (§15.2)",
                self.vault.confirmation_depth
            ));
        }

        // Relay
        if self.relay.cascade_max_relays == 0 {
            errors.push("relay.cascade_max_relays must be ≥ 1".to_string());
        }
        if self.relay.cascade_stagger_ms == 0 {
            errors.push(
                "relay.cascade_stagger_ms must be > 0 — zero stagger re-introduces C2 (§11.2)"
                    .to_string(),
            );
        }

        // API
        if self.api.ws_authenticated_msgs_per_min == 0 {
            errors.push("api.ws_authenticated_msgs_per_min must be > 0".to_string());
        }

        errors
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

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
    fn default_values_match_spec() {
        let cfg = OmegaConfig::default();
        // §12.2
        assert_eq!(cfg.gas.max_priority_fee_gwei, 500);
        // §11.2 fix C2
        assert_eq!(cfg.relay.max_bundles_per_relay_per_second, 4);
        assert_eq!(cfg.relay.cascade_stagger_ms, 10);
        // §13.1 fix C1
        assert!((cfg.ml.validation_ratio - 0.20).abs() < f64::EPSILON);
        assert_eq!(cfg.ml.checkpoint_interval, 1_000);
        // §13.3 fix I5
        assert_eq!(cfg.ml.ceiling_escalation_threshold, 100);
        // §14.1 fix C4
        assert!((cfg.rotation.base_carryover_fraction - 0.50).abs() < f64::EPSILON);
        assert!((cfg.rotation.reputation_decay_rate_months - 3.0).abs() < f64::EPSILON);
        // §15.1
        assert_eq!(cfg.vault.dao_fee_bps, 500);
        assert_eq!(cfg.vault.confirmation_depth, 12);
        // §17.1 fix M4
        assert_eq!(cfg.api.ws_authenticated_msgs_per_min, 300);
        assert_eq!(cfg.api.ws_anonymous_msgs_per_min, 100);
        // §11.1
        assert_eq!(cfg.la.warm_batch_interval_ms, 200);
        assert_eq!(cfg.la.archived_cycle_blocks, 500);
        // §11.3
        assert_eq!(cfg.la.sequencer_restart_window_blocks, 60);
    }
}
