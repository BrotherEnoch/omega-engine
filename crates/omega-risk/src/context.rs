// crates/omega-risk/src/context.rs
//
// CheckContext: all live market state required to execute the 13 pre-trade checks.
//
// Design:
//   • Constructed once per scoring cycle by the EIL / oracle layer.
//   • Passed by shared reference into run_all_checks(); zero allocation inside checks.
//   • All fields are plain Copy/Clone types — no Arc/lock inside the hot path.
//   • The oracle-age tuple matches the spec tri-oracle order: (chainlink, pyth, twap).
//
// Spec references:
//   S5  — oracle staleness thresholds: Chainlink 45s, Pyth 45s, TWAP 120s.
//   S7  — L1 data buffer adaptive window; L2 base fee.
//   S8  — strategy whitelist check (bytecode hash).
//   S11 — LA: price impact threshold 50 bps, flashloan exclusion list.
//   S12 — gas spike guard: 30 % L1 delta threshold.
//   S19 — EV ratio monitoring for rollout.

use serde::{Deserialize, Serialize};

/// Oracle staleness thresholds in seconds (spec S5).
pub const CHAINLINK_STALENESS_SECS: u64 = 45;
pub const PYTH_STALENESS_SECS:      u64 = 45;
pub const TWAP_STALENESS_SECS:      u64 = 120;

/// Maximum acceptable L1 gas price delta before rejecting blueprint (spec S12: 30 %).
pub const GAS_SPIKE_THRESHOLD: f64 = 0.30;

/// Maximum price impact in basis points for LA blueprints (spec S11: 50 bps).
pub const MAX_PRICE_IMPACT_BPS: u16 = 50;

/// Maximum slippage in basis points per strategy class.
pub const MAX_SLIPPAGE_BPS_SA:  u16 = 30;
pub const MAX_SLIPPAGE_BPS_MSA: u16 = 50;
pub const MAX_SLIPPAGE_BPS_LA:  u16 = 100;
pub const MAX_SLIPPAGE_BPS_MEV: u16 = 30;

/// Flashloan safety margin: available liquidity must cover amount × this factor (spec S11).
pub const FLASHLOAN_SAFETY_FACTOR: f64 = 1.20;

/// Maximum oracle divergence between Chainlink and Pyth (spec S5: 0.4 %).
pub const ORACLE_DIVERGE_THRESHOLD: f64 = 0.004;

/// Live oracle price snapshot used in checks 7–8.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct OracleSnapshot {
    /// Chainlink price (USD, 18-decimal fixed point expressed as f64 for comparison).
    pub chainlink_price:  f64,
    /// Pyth price.
    pub pyth_price:       f64,
    /// Uniswap v3 TWAP price.
    pub twap_price:       f64,
    /// Age of each oracle feed in seconds at check time.
    pub chainlink_age_s:  u64,
    pub pyth_age_s:       u64,
    pub twap_age_s:       u64,
}

impl OracleSnapshot {
    /// True if Chainlink feed is within staleness threshold.
    pub fn chainlink_fresh(&self) -> bool {
        self.chainlink_age_s < CHAINLINK_STALENESS_SECS
    }

    /// True if Pyth feed is within staleness threshold.
    pub fn pyth_fresh(&self) -> bool {
        self.pyth_age_s < PYTH_STALENESS_SECS
    }

    /// True if TWAP feed is within staleness threshold.
    pub fn twap_fresh(&self) -> bool {
        self.twap_age_s < TWAP_STALENESS_SECS
    }

    /// Relative divergence between Chainlink and Pyth prices.
    pub fn chainlink_pyth_divergence(&self) -> f64 {
        if self.chainlink_price <= 0.0 {
            return f64::INFINITY;
        }
        (self.chainlink_price - self.pyth_price).abs() / self.chainlink_price
    }
}

/// Flashloan liquidity snapshot used in check 10.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashloanSnapshot {
    /// Maximum available flashloan in the same units as the blueprint's
    /// `flashloan_amount` field (raw token units).
    pub available:    u128,
    /// Protocol identifier string — used to enforce the no-self-flash rule.
    /// e.g., "aave", "balancer", "euler", "morpho".
    pub protocol_id:  String,
}

/// Full live market context passed to run_all_checks().
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckContext {
    // ── Chain identity ──────────────────────────────────────────────────────
    /// Expected chain ID — check 1.
    pub expected_chain_id: u64,

    // ── Block state ─────────────────────────────────────────────────────────
    /// Current block number at check time — check 2 (expiry).
    pub current_block: u64,

    // ── Gas state ───────────────────────────────────────────────────────────
    /// Current L1 Ethereum gas price in gwei — checks 6 (spike) + 5 (profit).
    pub current_l1_gas_price_gwei: u64,
    /// Current Arbitrum L2 base fee in gwei — check 5 (profit).
    pub current_l2_base_fee_gwei:  u64,
    /// Current L1 adaptive buffer (output of l1_adaptive_buffer()) — check 5.
    pub l1_adaptive_buffer: f64,

    // ── Oracle state ────────────────────────────────────────────────────────
    /// Per-asset oracle snapshot for the primary asset — checks 7–8.
    pub oracle: OracleSnapshot,

    // ── Flashloan state ─────────────────────────────────────────────────────
    /// Real-time flashloan provider liquidity — check 10.
    pub flashloan: FlashloanSnapshot,

    // ── Competition state ───────────────────────────────────────────────────
    /// Probabilistic competition probability [0.0, 1.0] — check 11.
    /// Computed by omega-risk::competition before entering the check pipeline.
    pub competition_probability: f64,
    /// Maximum acceptable competition probability before abandoning blueprint.
    pub max_competition_probability: f64,

    // ── Strategy limits ─────────────────────────────────────────────────────
    /// Strategy maximum gas budget (units) — check 3.
    pub strategy_max_gas: u64,
    /// Maximum slippage tolerance in basis points for this strategy — check 9.
    pub max_slippage_bps: u16,
    /// Rollout tier [0.0, 1.0] — gates blueprint scoring volume (spec S19).
    pub rollout_tier: f64,
    /// Strategy bytecode hash for whitelist check — check 4.
    pub strategy_bytecode_hash: [u8; 32],

    // ── Risk score ──────────────────────────────────────────────────────────
    /// Composite risk score [0.0, 1.0] — check 12.
    /// Incorporates: gas volatility, oracle freshness, competition, liquidity depth.
    pub risk_score: f64,
    /// Maximum acceptable risk score.
    pub max_risk_score: f64,
}