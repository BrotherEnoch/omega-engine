// crates/omega-risk/src/context.rs
//
// CheckContext: all live market state required to execute the pre-trade checks.
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
//
// ## Audit fix (this revision): account exposure fields for check 14
//
// `current_account_exposure_wei` / `max_account_exposure_wei` back
// `checks::check_account_exposure` (spec-unnumbered addition:
// MissExposureLimit). See checks.rs's module doc comment for why the
// check sums `bp.flashloan_amount` (capital principal) against these
// fields rather than `bp.expected_profit_net_wei` — this file only
// carries the context data, the reasoning lives with the check itself.
//
// ## Audit fix (this revision): integer-ratio constants for checks 6 and 10
//
// `GAS_SPIKE_THRESHOLD` (f64, 0.30) and `FLASHLOAN_SAFETY_FACTOR` (f64, 1.20)
// are replaced by exact integer numerator/denominator pairs
// (`GAS_SPIKE_THRESHOLD_NUM/DEN`, `FLASHLOAN_SAFETY_NUM/DEN`). See
// `checks.rs`'s `check_gas_spike` and `check_flashloan_liquidity` for why:
// float division in a pre-trade safety gate risks non-bit-identical
// evaluation across platforms/builds, and a naive integer replacement using
// floor division would silently round the flashloan safety margin DOWN,
// the wrong direction for a check whose entire purpose is staying
// conservative.
//
// The old f64 constants are removed rather than kept alongside the new
// ones — leaving both in place invites exactly the drift this fix is
// meant to eliminate (someone tunes one and not the other, and the two
// checks that are supposed to enforce the same threshold silently
// disagree). If anything else in this crate still references
// `GAS_SPIKE_THRESHOLD` or `FLASHLOAN_SAFETY_FACTOR` by name, that call
// site needs to be updated to the integer form too — grep the crate for
// both identifiers before merging this change. Nothing in `checks.rs` or
// this file references them anymore as of this revision.
//
// ## Audit fix (this revision): latest_blueprint_nonce field for check 15
//
// `checks.rs`'s `check_nonce_replay` (spec-unnumbered addition:
// StaleBlueprint) compares each blueprint's `nonce` against
// `latest_blueprint_nonce` and rejects anything not strictly greater —
// see that check's doc comment for the exact replay/staleness semantics.
// This field was referenced by `checks.rs` before it existed here
// (`error[E0609]: no field 'latest_blueprint_nonce' on type
// '&CheckContext'`); added below alongside the other per-account state
// (`current_account_exposure_wei`) rather than the per-blueprint fields,
// since — like exposure — this is state the caller must track and update
// across scoring cycles for a given account, not something re-derived
// fresh from oracle/chain state each time.
//
// ## Audit fix (this revision): oracle sanity / flash-crash helpers for check 16
//
// Added to `OracleSnapshot`: `spot_price()`, `spot_twap_divergence()`, and
// `has_sane_prices()`. These back `checks::oracle_price_sanity_check`
// (check 16, `DropCode::MissFlashCrash` — that drop code already existed
// in `checks::drop_code_label`'s match arm, but nothing produced it until
// this revision).
//
// This closes a real gap distinct from the existing Chainlink-vs-Pyth
// divergence check (check 8, `MissOracleDiverge`): check 8 only catches
// the two spot oracles disagreeing WITH EACH OTHER. It does nothing if
// (a) a relied-upon price is simply non-sane on its own — zero, negative,
// NaN, or infinite, e.g. from a malformed or compromised feed — or (b)
// both spot oracles agree with each other but have moved together far
// from the TWAP, which is the harder-to-manipulate-within-one-block
// reference specifically because it's time-weighted. Case (b) is also
// invisible to check 8 by construction (it only compares Chainlink to
// Pyth) and would previously pass straight through as long as the two
// spot feeds happened to agree.
//
// `FLASH_CRASH_SPOT_TWAP_DIVERGENCE_THRESHOLD` is intentionally more
// permissive than `ORACLE_DIVERGE_THRESHOLD` (check 8's 0.4%): TWAP is
// backward-looking over its averaging window by design, so a genuine,
// legitimate fast price move on a volatile asset can and should diverge
// from it by more than two live spot oracles would ever legitimately
// diverge from each other. This is a conservative starting threshold —
// tune it against real asset volatility profiles, not a value derived
// from the spec (no such derivation exists to reference).

use serde::{Deserialize, Serialize};

/// Oracle staleness thresholds in seconds (spec S5).
pub const CHAINLINK_STALENESS_SECS: u64 = 45;
pub const PYTH_STALENESS_SECS: u64 = 45;
pub const TWAP_STALENESS_SECS: u64 = 120;

/// Maximum acceptable L1 gas price delta before rejecting blueprint (spec
/// S12: 30 %), expressed as an exact integer ratio `NUM / DEN` rather than
/// an `f64`. `checks::check_gas_spike` evaluates this as
/// `diff * DEN > at_creation * NUM` — algebraically equivalent to
/// `diff / at_creation > NUM / DEN`, but with no division and no float
/// rounding anywhere in the comparison.
///
/// To change the threshold, edit these two constants together (e.g. 25%
/// would be NUM=25, DEN=100) — there is no other copy of this ratio
/// anywhere in the crate.
pub const GAS_SPIKE_THRESHOLD_NUM: u64 = 30;
pub const GAS_SPIKE_THRESHOLD_DEN: u64 = 100;

/// Maximum price impact in basis points for LA blueprints (spec S11: 50 bps).
pub const MAX_PRICE_IMPACT_BPS: u16 = 50;

/// Maximum slippage in basis points per strategy class.
pub const MAX_SLIPPAGE_BPS_SA: u16 = 30;
pub const MAX_SLIPPAGE_BPS_MSA: u16 = 50;
pub const MAX_SLIPPAGE_BPS_LA: u16 = 100;
pub const MAX_SLIPPAGE_BPS_MEV: u16 = 30;

/// Flashloan safety margin (spec S11): available liquidity must cover
/// `amount × (NUM / DEN)` (default 1.20, i.e. a 20% margin), expressed as
/// an exact integer ratio rather than an `f64`.
/// `checks::check_flashloan_liquidity` evaluates the required threshold as
/// `amount.saturating_mul(NUM).div_ceil(DEN)` — ceiling division, so any
/// fractional remainder makes the requirement stricter (rounds up), never
/// looser. A floor-dividing replacement of the old float cast would have
/// silently accepted slightly less liquidity than the configured margin.
///
/// To change the margin, edit these two constants together (e.g. 1.25x
/// would be NUM=125, DEN=100).
pub const FLASHLOAN_SAFETY_NUM: u128 = 120;
pub const FLASHLOAN_SAFETY_DEN: u128 = 100;

/// Maximum oracle divergence between Chainlink and Pyth (spec S5: 0.4 %).
///
/// Retained as `f64`: `OracleSnapshot::chainlink_pyth_divergence()` (below)
/// is itself computed from `f64` oracle prices, so converting only the
/// threshold constant to an integer ratio wouldn't remove the float
/// dependency — it would just relocate where the precision question lives.
/// Revisit this alongside `chainlink_pyth_divergence()` together if/when
/// oracle prices become fixed-point at the source.
pub const ORACLE_DIVERGE_THRESHOLD: f64 = 0.004;

/// Maximum divergence between the fresh "spot" price (Chainlink, falling
/// back to Pyth) and a fresh TWAP before treating the spot price as
/// untrustworthy — the flash-crash / oracle-manipulation guard (check 16,
/// `DropCode::MissFlashCrash`). See this file's module-level audit note
/// for why this is deliberately looser than `ORACLE_DIVERGE_THRESHOLD`
/// and why it's a conservative starting value rather than a spec-derived
/// one.
pub const FLASH_CRASH_SPOT_TWAP_DIVERGENCE_THRESHOLD: f64 = 0.15;

/// Live oracle price snapshot used in checks 7–8 and 16.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct OracleSnapshot {
    /// Chainlink price (USD, 18-decimal fixed point expressed as f64 for comparison).
    pub chainlink_price: f64,
    /// Pyth price.
    pub pyth_price: f64,
    /// Uniswap v3 TWAP price.
    pub twap_price: f64,
    /// Age of each oracle feed in seconds at check time.
    pub chainlink_age_s: u64,
    pub pyth_age_s: u64,
    pub twap_age_s: u64,
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

    /// The "spot" price used for hierarchy/sanity comparisons: Chainlink
    /// if fresh, else Pyth if fresh, else `None`. Mirrors the tri-oracle
    /// preference order used throughout this crate (Chainlink primary,
    /// Pyth secondary, TWAP tertiary — see this crate's module doc
    /// comments).
    pub fn spot_price(&self) -> Option<f64> {
        if self.chainlink_fresh() {
            Some(self.chainlink_price)
        } else if self.pyth_fresh() {
            Some(self.pyth_price)
        } else {
            None
        }
    }

    /// Relative divergence between the fresh spot price and a fresh TWAP.
    ///
    /// Returns `None` when there is nothing meaningful to compare: TWAP
    /// stale or non-positive, or neither Chainlink nor Pyth fresh. A
    /// `None` here means this particular comparison is skipped — it does
    /// NOT by itself mean the blueprint is safe; `oracle_freshness_check`
    /// (check 7) is what governs the "everything is stale" case
    /// independently.
    pub fn spot_twap_divergence(&self) -> Option<f64> {
        if !self.twap_fresh() || self.twap_price <= 0.0 {
            return None;
        }
        let spot = self.spot_price()?;
        Some((spot - self.twap_price).abs() / self.twap_price)
    }

    /// True when every currently-fresh price feed reports a finite,
    /// strictly-positive value.
    ///
    /// A stale feed's price is deliberately NOT checked here — an
    /// unreliable-but-unused feed doesn't compromise a trade the way an
    /// unreliable-but-relied-upon one does, and whether "unused" is
    /// actually the case for ALL feeds simultaneously is
    /// `oracle_freshness_check`'s job (check 7), not this method's.
    pub fn has_sane_prices(&self) -> bool {
        let chainlink_ok = !self.chainlink_fresh()
            || (self.chainlink_price.is_finite() && self.chainlink_price > 0.0);
        let pyth_ok = !self.pyth_fresh() || (self.pyth_price.is_finite() && self.pyth_price > 0.0);
        let twap_ok = !self.twap_fresh() || (self.twap_price.is_finite() && self.twap_price > 0.0);
        chainlink_ok && pyth_ok && twap_ok
    }
}

/// Flashloan liquidity snapshot used in check 10.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashloanSnapshot {
    /// Maximum available flashloan in the same units as the blueprint's
    /// `flashloan_amount` field (raw token units).
    pub available: u128,
    /// Protocol identifier string — used to enforce the no-self-flash rule.
    /// e.g., "aave", "balancer", "euler", "morpho".
    pub protocol_id: String,
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
    pub current_l2_base_fee_gwei: u64,
    /// Current L1 adaptive buffer (output of l1_adaptive_buffer()) — check 5.
    pub l1_adaptive_buffer: f64,

    // ── Oracle state ────────────────────────────────────────────────────────
    /// Per-asset oracle snapshot for the primary asset — checks 7, 8, 16.
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

    // ── Account exposure ────────────────────────────────────────────────────
    /// Account's already-outstanding exposure in wei, prior to this
    /// blueprint — check 14. This blueprint's `flashloan_amount` (capital
    /// principal, NOT expected profit — see checks.rs's module doc
    /// comment) is added to this and compared against
    /// `max_account_exposure_wei`.
    pub current_account_exposure_wei: u128,
    /// Configured cap on total account exposure, in wei — check 14.
    pub max_account_exposure_wei: u128,

    // ── Nonce replay ─────────────────────────────────────────────────────────
    /// Highest blueprint `nonce` already processed for this account, prior
    /// to the blueprint currently being checked — check 15
    /// (`checks::check_nonce_replay`). A blueprint whose own `nonce` is not
    /// strictly greater than this is rejected as a replay or stale
    /// resubmission (`DropCode::StaleBlueprint`). Like
    /// `current_account_exposure_wei`, this is per-account state the
    /// caller must track and advance across scoring cycles — it is not
    /// re-derived from oracle/chain state each time.
    pub latest_blueprint_nonce: u64,
}
