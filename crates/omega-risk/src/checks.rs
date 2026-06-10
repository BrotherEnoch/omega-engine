// crates/omega-risk/src/checks.rs
//
// 13 pre-trade checks in FAST-FAIL order (spec Section 5 / S7 / S11 / S12).
//
// Order is mandatory and maps directly to the spec:
//   1.  ChainID          — chain_id mismatch → WrongChain
//   2.  Expiry           — current_block ≥ expiry_block → MissExpiry
//   3.  GasBudget        — total_gas > strategy max_gas_budget → MissGas
//   4.  Whitelist        — bytecode hash not in registry → MissWhitelist
//   5.  DynProfit        — expected_profit_net < dynamic_min_profit → MissProfit
//   6.  GasSpike         — |Δl1_gas_price| > 30 % since creation → MissGasSpike
//   7.  OracleFreshness  — all oracle feeds stale → MissOracle
//   8.  OracleHierarchy  — Chainlink+Pyth fresh but diverge >0.4% → MissOracleDiverge
//   9.  Slippage         — slippage_bps > strategy max → MissSlippage
//   10. Liquidity        — flashloan_available < flashloan_amount×1.20 → MissLiquidity
//   11. Competition      — competition_prob > threshold → MissCompetition
//   12. RiskScore        — composite risk score > max_risk_score → MissRisk
//   13. PriceImpact      — LA only: price_impact_bps > 50 → MissPriceImpact
//
// Fast-fail principle: cheapest checks (no memory allocation, no division) run first.
// The first failing check returns its DropCode immediately; subsequent checks are skipped.
//
// Thread-safety: run_all_checks() is a pure function (no interior state).
// Callers should construct CheckContext once per scoring cycle and reuse across checks.

use omega_core::errors::DropCode;

use crate::context::{
    CheckContext, GAS_SPIKE_THRESHOLD, MAX_PRICE_IMPACT_BPS,
    CHAINLINK_STALENESS_SECS, PYTH_STALENESS_SECS, TWAP_STALENESS_SECS,
    ORACLE_DIVERGE_THRESHOLD, FLASHLOAN_SAFETY_FACTOR,
};
use crate::metrics;

// ─── Public result type ───────────────────────────────────────────────────────

/// Outcome of running all 13 pre-trade checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckResult {
    Pass,
    Fail(DropCode),
}

impl CheckResult {
    pub fn is_pass(&self) -> bool { matches!(self, CheckResult::Pass) }
    pub fn is_fail(&self) -> bool { matches!(self, CheckResult::Fail(_)) }
    pub fn drop_code(&self) -> Option<&DropCode> {
        match self { CheckResult::Fail(c) => Some(c), _ => None }
    }
}

// ─── Blueprint-like trait-object-compatible input ────────────────────────────
//
// We accept a `BlueprintFields` struct (not the full ExecutionBlueprint) to keep
// omega-risk independent of alloy primitives and allow unit-testing without building
// a full blueprint.  omega-strategies calls `BlueprintFields::from_blueprint(&bp)`.

/// Minimal blueprint fields required by the 13 pre-trade checks.
/// Extracted from ExecutionBlueprint by the caller.
#[derive(Debug, Clone)]
pub struct BlueprintFields {
    pub chain_id:                u64,
    pub expiry_block:            u64,
    pub l2_exec_gas_estimate:    u64,
    pub l1_data_gas_estimate:    u64,
    pub extraction_gas:          u64,
    pub expected_profit_net_wei: u128,  // in wei for U256-free comparison
    pub dynamic_min_profit_wei:  u128,
    pub l1_data_fee_at_creation: u64,   // gwei
    pub slippage_bps:            u16,
    pub flashloan_amount:        u128,
    pub flashloan_provider_id:   &'static str,
    pub strategy_id:             &'static str,
    pub strategy_bytecode_hash:  [u8; 32],
    /// Present for LA blueprints only (spec check 13).
    pub price_impact_bps:        Option<u16>,
    pub ofa_compliant:           bool,
}

// ─── Main entry point ─────────────────────────────────────────────────────────

/// Execute all 13 pre-trade checks in fast-fail order.
///
/// Returns `CheckResult::Pass` only when every check passes.
/// The first failing check short-circuits and returns its `DropCode`.
///
/// Emits a Prometheus counter for every drop code encountered.
pub fn run_all_checks(bp: &BlueprintFields, ctx: &CheckContext) -> CheckResult {
    let result = run_checks_inner(bp, ctx);
    match &result {
        CheckResult::Pass => {
            metrics::CHECKS_PASSED.with_label_values(&[bp.strategy_id]).inc();
        }
        CheckResult::Fail(code) => {
            metrics::CHECKS_FAILED
                .with_label_values(&[bp.strategy_id, drop_code_label(code)])
                .inc();
            tracing::debug!(
                strategy = bp.strategy_id,
                drop_code = ?code,
                "blueprint dropped at pre-trade check"
            );
        }
    }
    result
}

#[inline]
fn run_checks_inner(bp: &BlueprintFields, ctx: &CheckContext) -> CheckResult {
    // 1. Chain ID — cheapest possible check; bitwise comparison.
    if let Some(c) = check_chain_id(bp, ctx) { return CheckResult::Fail(c); }

    // 2. Expiry — integer comparison.
    if let Some(c) = check_expiry(bp, ctx) { return CheckResult::Fail(c); }

    // 3. Gas budget — integer addition + comparison.
    if let Some(c) = check_gas_budget(bp, ctx) { return CheckResult::Fail(c); }

    // 4. Whitelist — hash comparison (O(1) hashmap lookup in caller).
    if let Some(c) = check_whitelist(bp, ctx) { return CheckResult::Fail(c); }

    // 5. Dynamic profit — U128 comparison.
    if let Some(c) = check_dynamic_profit(bp, ctx) { return CheckResult::Fail(c); }

    // 6. Gas spike — one float division + comparison.
    if let Some(c) = check_gas_spike(bp, ctx) { return CheckResult::Fail(c); }

    // 7. Oracle freshness — integer comparisons against age thresholds.
    if let Some(c) = check_oracle_freshness(bp, ctx) { return CheckResult::Fail(c); }

    // 8. Oracle hierarchy — one float division (only when both feeds fresh).
    if let Some(c) = check_oracle_hierarchy(bp, ctx) { return CheckResult::Fail(c); }

    // 9. Slippage — integer comparison.
    if let Some(c) = check_slippage(bp, ctx) { return CheckResult::Fail(c); }

    // 10. Flashloan liquidity — one float multiply + comparison.
    if let Some(c) = check_flashloan_liquidity(bp, ctx) { return CheckResult::Fail(c); }

    // 11. Competition — float comparison.
    if let Some(c) = check_competition(bp, ctx) { return CheckResult::Fail(c); }

    // 12. Risk score — float comparison.
    if let Some(c) = check_risk_score(bp, ctx) { return CheckResult::Fail(c); }

    // 13. Price impact — LA only; only evaluated when field is present.
    if bp.price_impact_bps.is_some() {
        if let Some(c) = check_price_impact(bp, ctx) { return CheckResult::Fail(c); }
    }

    CheckResult::Pass
}

// ─── Individual check implementations ────────────────────────────────────────

/// Check 1: chain_id must match expected (spec: Certora C1 / drops WrongChain).
#[inline]
fn check_chain_id(bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    if bp.chain_id != ctx.expected_chain_id {
        return Some(DropCode::WrongChain);
    }
    None
}

/// Check 2: blueprint must not have expired (spec: MissExpiry).
#[inline]
fn check_expiry(bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    if ctx.current_block >= bp.expiry_block {
        return Some(DropCode::MissExpiry);
    }
    None
}

/// Check 3: total gas estimate must not exceed the strategy's gas budget (spec: MissGas).
///
/// Total gas = l2_exec + l1_data + extraction.
#[inline]
fn check_gas_budget(bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    let total_gas = bp.l2_exec_gas_estimate
        .saturating_add(bp.l1_data_gas_estimate)
        .saturating_add(bp.extraction_gas);
    if ctx.strategy_max_gas > 0 && total_gas > ctx.strategy_max_gas {
        return Some(DropCode::MissGas);
    }
    None
}

/// Check 4: strategy bytecode hash must be in the approved whitelist (spec S8 / Certora C4).
///
/// NOTE: This check uses the hash provided in CheckContext (loaded from the whitelist
/// registry) — NOT re-computing the hash here.  The registry is the source of truth.
#[inline]
fn check_whitelist(bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    if ctx.strategy_bytecode_hash != bp.strategy_bytecode_hash {
        return Some(DropCode::MissWhitelist);
    }
    None
}

/// Check 5: expected_profit_net >= dynamic_min_profit (spec S7 / S12: MissProfit).
///
/// Both values are in wei (U128) to preserve precision without alloy dependencies.
#[inline]
fn check_dynamic_profit(bp: &BlueprintFields, _ctx: &CheckContext) -> Option<DropCode> {
    if bp.expected_profit_net_wei < bp.dynamic_min_profit_wei {
        return Some(DropCode::MissProfit);
    }
    None
}

/// Check 6: reject if L1 gas price has moved >30 % since blueprint creation (spec S12: MissGasSpike).
///
/// Delta = |current - at_creation| / at_creation.
/// Handles the case where at_creation = 0 (division-by-zero guard).
#[inline]
fn check_gas_spike(bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    let at_creation = bp.l1_data_fee_at_creation.max(1) as f64;
    let current     = ctx.current_l1_gas_price_gwei as f64;
    let delta       = (current - at_creation).abs() / at_creation;
    if delta > GAS_SPIKE_THRESHOLD {
        return Some(DropCode::MissGasSpike);
    }
    None
}

/// Check 7: at least one oracle must be within its staleness threshold (spec S5: MissOracle).
///
/// Tri-oracle hierarchy: Chainlink primary, Pyth secondary, TWAP tertiary.
/// Blueprint is rejected only when ALL three feeds are stale simultaneously.
#[inline]
fn check_oracle_freshness(_bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    let cl_ok = ctx.oracle.chainlink_age_s < CHAINLINK_STALENESS_SECS;
    let py_ok = ctx.oracle.pyth_age_s     < PYTH_STALENESS_SECS;
    let tw_ok = ctx.oracle.twap_age_s     < TWAP_STALENESS_SECS;

    if !cl_ok && !py_ok && !tw_ok {
        return Some(DropCode::MissOracle);
    }
    None
}

/// Check 8: when Chainlink AND Pyth are both fresh, they must agree within 0.4 % (spec S5).
///
/// Divergence above threshold = oracle manipulation signal → drop with MissOracleDiverge.
/// If only one feed is fresh, skip divergence check (handled by hierarchy resolution).
#[inline]
fn check_oracle_hierarchy(_bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    let cl_ok = ctx.oracle.chainlink_age_s < CHAINLINK_STALENESS_SECS;
    let py_ok = ctx.oracle.pyth_age_s     < PYTH_STALENESS_SECS;

    if cl_ok && py_ok {
        let divergence = ctx.oracle.chainlink_pyth_divergence();
        if divergence > ORACLE_DIVERGE_THRESHOLD {
            return Some(DropCode::MissOracleDiverge);
        }
    }
    None
}

/// Check 9: slippage_bps must not exceed the per-strategy maximum (spec: MissSlippage).
#[inline]
fn check_slippage(bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    if bp.slippage_bps > ctx.max_slippage_bps {
        return Some(DropCode::MissSlippage);
    }
    None
}

/// Check 10: flashloan provider must have enough liquidity (spec S11: MissLiquidity).
///
/// Rule: available >= flashloan_amount × 1.20 (20 % safety margin, spec S11).
/// Also enforces the no-self-flash rule: flashloan provider ≠ liquidation target protocol.
/// (e.g., no Aave-on-Aave; no Euler-on-Euler — spec S11.4 / crates/omega-strategies/la/protocols).
#[inline]
fn check_flashloan_liquidity(bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    // No-self-flash rule (spec: "encoded exclusion_list per protocol").
    if ctx.flashloan.protocol_id.as_str() == bp.flashloan_provider_id
        && bp.strategy_id == "LA"
    {
        // Flashloan provider is the same protocol being liquidated — forbidden.
        return Some(DropCode::MissLiquidity);
    }

    // Safety margin check.
    let required = (bp.flashloan_amount as f64 * FLASHLOAN_SAFETY_FACTOR) as u128;
    if ctx.flashloan.available < required {
        return Some(DropCode::MissLiquidity);
    }
    None
}

/// Check 11: competition probability must be below acceptable threshold (spec S11: MissCompetition).
#[inline]
fn check_competition(_bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    if ctx.competition_probability > ctx.max_competition_probability {
        return Some(DropCode::MissCompetition);
    }
    None
}

/// Check 12: composite risk score must be below threshold (spec S8: MissRisk).
///
/// Risk score is pre-computed by the caller (incorporates gas volatility,
/// oracle freshness, competition, liquidity depth) and passed in CheckContext.
#[inline]
fn check_risk_score(_bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    if ctx.risk_score > ctx.max_risk_score {
        return Some(DropCode::MissRisk);
    }
    None
}

/// Check 13: price impact must not exceed 50 bps for LA blueprints (spec S11: MissPriceImpact).
///
/// Only evaluated when `bp.price_impact_bps` is `Some(...)` (LA strategy only).
#[inline]
fn check_price_impact(bp: &BlueprintFields, _ctx: &CheckContext) -> Option<DropCode> {
    if bp.price_impact_bps.unwrap_or(0) > MAX_PRICE_IMPACT_BPS {
        return Some(DropCode::MissPriceImpact);
    }
    None
}

// ─── Prometheus label helper ──────────────────────────────────────────────────

fn drop_code_label(code: &DropCode) -> &'static str {
    match code {
        DropCode::WrongChain          => "wrong_chain",
        DropCode::MissExpiry          => "miss_expiry",
        DropCode::MissGas             => "miss_gas",
        DropCode::MissWhitelist       => "miss_whitelist",
        DropCode::MissProfit          => "miss_profit",
        DropCode::MissGasSpike        => "miss_gas_spike",
        DropCode::MissOracle          => "miss_oracle",
        DropCode::MissOracleDiverge   => "miss_oracle_diverge",
        DropCode::MissSlippage        => "miss_slippage",
        DropCode::MissLiquidity       => "miss_liquidity",
        DropCode::MissCompetition     => "miss_competition",
        DropCode::MissRisk            => "miss_risk",
        DropCode::MissPriceImpact     => "miss_price_impact",
        DropCode::MissDexLiquidity    => "miss_dex_liquidity",
        DropCode::MissHfNotLiquidatable => "miss_hf_not_liquidatable",
        DropCode::MissFlashCrash      => "miss_flash_crash",
        _                             => "other",
    }
}

#[cfg(test)]
mod checks_tests {
    use super::*;
    use crate::context::{CheckContext, OracleSnapshot, FlashloanSnapshot};

    // ── Test harness ──────────────────────────────────────────────────────────

    fn passing_bp() -> BlueprintFields {
        BlueprintFields {
            chain_id:                42161,
            expiry_block:            1000,
            l2_exec_gas_estimate:    100_000,
            l1_data_gas_estimate:    5_000,
            extraction_gas:          45_000,
            expected_profit_net_wei: 1_000_000_000_000_000_000, // 1 ETH
            dynamic_min_profit_wei:  100_000_000_000_000_000,   // 0.1 ETH
            l1_data_fee_at_creation: 50,
            slippage_bps:            20,
            flashloan_amount:        1_000_000,
            flashloan_provider_id:   "balancer",
            strategy_id:             "SA",
            strategy_bytecode_hash:  [0xaa; 32],
            price_impact_bps:        None,
            ofa_compliant:           true,
        }
    }

    fn passing_ctx() -> CheckContext {
        CheckContext {
            expected_chain_id:       42161,
            current_block:           500,
            current_l1_gas_price_gwei: 50,
            current_l2_base_fee_gwei:  1,
            l1_adaptive_buffer:      1.30,
            oracle: OracleSnapshot {
                chainlink_price: 2000.0,
                pyth_price:      2001.0,  // 0.05 % divergence — within 0.4 %
                twap_price:      1999.0,
                chainlink_age_s: 10,
                pyth_age_s:      10,
                twap_age_s:      60,
            },
            flashloan: FlashloanSnapshot {
                available:   2_000_000,  // 2× the flashloan_amount
                protocol_id: String::from("balancer"),
            },
            competition_probability:     0.50,
            max_competition_probability: 0.90,
            strategy_max_gas:            500_000,
            max_slippage_bps:            30,
            rollout_tier:                1.0,
            strategy_bytecode_hash:      [0xaa; 32],
            risk_score:                  0.30,
            max_risk_score:              0.80,
        }
    }

    // ── Happy path ────────────────────────────────────────────────────────────

    #[test]
    fn all_passing_returns_pass() {
        let bp  = passing_bp();
        let ctx = passing_ctx();
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    // ── Check 1: chain_id ─────────────────────────────────────────────────────

    #[test]
    fn wrong_chain_fails_at_check_1() {
        let mut bp = passing_bp();
        bp.chain_id = 1; // Ethereum mainnet
        let ctx = passing_ctx();
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::WrongChain));
    }

    // ── Check 2: expiry ───────────────────────────────────────────────────────

    #[test]
    fn expired_block_fails_at_check_2() {
        let bp  = passing_bp();
        let mut ctx = passing_ctx();
        ctx.current_block = 1001; // > expiry_block 1000
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::MissExpiry));
    }

    #[test]
    fn block_equal_to_expiry_fails() {
        let bp  = passing_bp();
        let mut ctx = passing_ctx();
        ctx.current_block = 1000; // == expiry_block
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::MissExpiry));
    }

    // ── Check 3: gas budget ───────────────────────────────────────────────────

    #[test]
    fn over_gas_budget_fails_at_check_3() {
        let bp  = passing_bp();
        let mut ctx = passing_ctx();
        // total_gas = 100_000 + 5_000 + 45_000 = 150_000; set budget to 100_000
        ctx.strategy_max_gas = 100_000;
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::MissGas));
    }

    #[test]
    fn zero_gas_budget_skips_check() {
        let bp  = passing_bp();
        let mut ctx = passing_ctx();
        ctx.strategy_max_gas = 0; // 0 = unlimited
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    // ── Check 4: whitelist ────────────────────────────────────────────────────

    #[test]
    fn wrong_bytecode_hash_fails_at_check_4() {
        let bp  = passing_bp();
        let mut ctx = passing_ctx();
        ctx.strategy_bytecode_hash = [0xbb; 32]; // different from bp's 0xaa
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::MissWhitelist));
    }

    // ── Check 5: dynamic profit ───────────────────────────────────────────────

    #[test]
    fn insufficient_profit_fails_at_check_5() {
        let mut bp = passing_bp();
        bp.expected_profit_net_wei = 50_000_000_000_000_000; // 0.05 ETH < min 0.1 ETH
        let ctx = passing_ctx();
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::MissProfit));
    }

    // ── Check 6: gas spike ────────────────────────────────────────────────────

    #[test]
    fn gas_spike_above_30pct_fails_at_check_6() {
        let bp  = passing_bp(); // l1_at_creation = 50
        let mut ctx = passing_ctx();
        ctx.current_l1_gas_price_gwei = 75; // 50 % increase > 30 %
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::MissGasSpike));
    }

    #[test]
    fn gas_spike_exactly_30pct_passes() {
        let bp  = passing_bp(); // l1_at_creation = 50
        let mut ctx = passing_ctx();
        ctx.current_l1_gas_price_gwei = 65; // 30 % = threshold, not strictly >
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    // ── Check 7: oracle freshness ─────────────────────────────────────────────

    #[test]
    fn all_oracles_stale_fails_at_check_7() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.oracle.chainlink_age_s = 100; // > 45s
        ctx.oracle.pyth_age_s      = 100;
        ctx.oracle.twap_age_s      = 200; // > 120s
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::MissOracle));
    }

    #[test]
    fn only_twap_fresh_passes_freshness() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.oracle.chainlink_age_s = 100;
        ctx.oracle.pyth_age_s      = 100;
        ctx.oracle.twap_age_s      = 60; // TWAP fresh
        // No divergence check when only TWAP is fresh → pass check 8 too.
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    // ── Check 8: oracle hierarchy ─────────────────────────────────────────────

    #[test]
    fn oracle_divergence_above_0_4pct_fails_at_check_8() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        // Chainlink 2000, Pyth 2010 → divergence 0.5 % > 0.4 %
        ctx.oracle.chainlink_price = 2000.0;
        ctx.oracle.pyth_price      = 2010.0;
        ctx.oracle.chainlink_age_s = 10;
        ctx.oracle.pyth_age_s      = 10;
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::MissOracleDiverge));
    }

    #[test]
    fn single_fresh_oracle_skips_divergence_check() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.oracle.pyth_age_s = 100; // Pyth stale → only Chainlink fresh
        // Divergence check skipped; check 8 passes.
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    // ── Check 9: slippage ─────────────────────────────────────────────────────

    #[test]
    fn slippage_above_max_fails_at_check_9() {
        let mut bp = passing_bp();
        bp.slippage_bps = 50; // > max_slippage_bps 30
        let ctx = passing_ctx();
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::MissSlippage));
    }

    // ── Check 10: liquidity ───────────────────────────────────────────────────

    #[test]
    fn insufficient_flashloan_liquidity_fails_at_check_10() {
        let bp = passing_bp(); // flashloan_amount = 1_000_000
        let mut ctx = passing_ctx();
        ctx.flashloan.available = 1_100_000; // < 1_000_000 × 1.20 = 1_200_000
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::MissLiquidity));
    }

    #[test]
    fn self_flash_la_fails_at_check_10() {
        let mut bp = passing_bp();
        bp.strategy_id           = "LA";
        bp.flashloan_provider_id = "aave";
        let mut ctx = passing_ctx();
        ctx.flashloan.protocol_id = String::from("aave"); // same as flashloan provider
        // Ensure other fields pass so we reach check 10.
        ctx.strategy_max_gas = 1_000_000;
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::MissLiquidity));
    }

    // ── Check 11: competition ─────────────────────────────────────────────────

    #[test]
    fn high_competition_fails_at_check_11() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.competition_probability     = 0.95;
        ctx.max_competition_probability = 0.90;
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::MissCompetition));
    }

    // ── Check 12: risk score ──────────────────────────────────────────────────

    #[test]
    fn high_risk_score_fails_at_check_12() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.risk_score    = 0.95;
        ctx.max_risk_score = 0.80;
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::MissRisk));
    }

    // ── Check 13: price impact (LA only) ─────────────────────────────────────

    #[test]
    fn price_impact_above_50bps_fails_at_check_13() {
        let mut bp = passing_bp();
        bp.price_impact_bps = Some(51);
        let ctx = passing_ctx();
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::MissPriceImpact));
    }

    #[test]
    fn price_impact_exactly_50bps_passes() {
        let mut bp = passing_bp();
        bp.price_impact_bps = Some(50);
        let ctx = passing_ctx();
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    #[test]
    fn non_la_blueprint_with_none_price_impact_skips_check_13() {
        let mut bp = passing_bp();
        bp.price_impact_bps = None;
        let ctx = passing_ctx();
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    // ── Fast-fail ordering ────────────────────────────────────────────────────

    #[test]
    fn chain_id_fails_before_expiry() {
        let mut bp  = passing_bp();
        bp.chain_id = 1; // check 1 fails
        let mut ctx = passing_ctx();
        ctx.current_block = 9999; // check 2 would also fail
        // Should return WrongChain, not MissExpiry.
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::WrongChain));
    }

    #[test]
    fn expiry_fails_before_gas_budget() {
        let bp  = passing_bp();
        let mut ctx = passing_ctx();
        ctx.current_block    = 9999; // check 2 fails
        ctx.strategy_max_gas = 1;    // check 3 would also fail
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Fail(DropCode::MissExpiry));
    }
}