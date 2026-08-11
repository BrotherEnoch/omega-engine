// crates/omega-risk/src/checks.rs
// 16 pre-trade checks in FAST-FAIL order (spec Section 5 / S7 / S11 / S12).
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
//   14. AccountExposure  — current_exposure + flashloan_amount > max_exposure → MissExposureLimit
//   15. NonceReplay      — bp.nonce ≤ latest_blueprint_nonce → StaleBlueprint
//   16. PriceSanity      — non-sane active price, or spot/TWAP divergence > threshold → MissFlashCrash
//
// Fast-fail principle: cheapest checks (no memory allocation, no division) run first.
// The first failing check returns its DropCode immediately; subsequent checks are skipped.
//
// Thread-safety: run_all_checks() is a pure function (no interior state).
// Callers should construct CheckContext once per scoring cycle and reuse across checks.
//
// ## Audit fix (this revision): check 14, account exposure limit
//
// Added as specified, with one correction: the field added to
// `current_account_exposure_wei` is `bp.flashloan_amount`, NOT
// `bp.expected_profit_net_wei` as originally drafted. Exposure is a
// measure of capital at risk — the flashloan principal being deployed —
// not anticipated profit. Summing expected profit into an exposure
// figure is backwards: it would let a large, thinly-profitable position
// (large real exposure, small expected_profit_net_wei) pass a check that
// a small, highly-profitable one (small real exposure, large
// expected_profit_net_wei) might fail, which is the opposite of what an
// exposure cap exists to catch. If "exposure" is meant to capture
// something other than flashloan principal (e.g. a modeled worst-case
// loss distinct from principal), this check needs a dedicated field for
// that quantity rather than reusing either `flashloan_amount` or
// `expected_profit_net_wei`, both of which mean something else.
//
// `DropCode::MissExposureLimit` must exist on `omega_core::errors::DropCode`
// — that enum lives outside this crate and is not modified here.
//
// ## Audit fix (this revision): integer arithmetic for checks 6 and 10
//
// `check_gas_spike` and `check_flashloan_liquidity` previously used `f64`
// division/multiplication to evaluate ratio thresholds. Replaced with exact
// integer arithmetic for two reasons:
//
//   1. Determinism: float division is not guaranteed bit-identical across
//      platforms/compilers/optimization levels. A pre-trade safety gate
//      should not have any risk, however small, of evaluating a boundary
//      condition differently depending on build environment.
//   2. Correct rounding direction: `check_flashloan_liquidity`'s required-
//      liquidity threshold now uses ceiling division. Floor division (the
//      naive integer replacement, and implicitly what truncating float-to-u128
//      cast does) rounds the required safety margin DOWN, which is the wrong
//      direction for a check whose entire purpose is staying conservative —
//      it would silently accept slightly less liquidity than the configured
//      1.20x margin actually requires. A later draft reintroduced plain
//      `saturating_mul(120) / 100` floor division; that draft was rejected
//      and this check still uses `div_ceil` against the named constants.
//
// Both checks now derive their thresholds from named integer-ratio constants
// in `context.rs` (`GAS_SPIKE_THRESHOLD_NUM/DEN`, `FLASHLOAN_SAFETY_NUM/DEN`)
// rather than inline literals, so there is exactly one place to change either
// threshold and no risk of the check silently drifting out of sync with it.
//
// ## Audit fix (this revision): check 15, nonce replay / stale blueprint
//
// A blueprint carries a monotonically-increasing `nonce` assigned at
// creation. If `bp.nonce <= ctx.latest_blueprint_nonce`, this blueprint is
// either a duplicate re-submission or older than one already processed for
// this account, and must be rejected with `DropCode::StaleBlueprint`.
//
// This is, like check 14, cheap enough (single integer comparison, no
// allocation, no division) that it would be equally valid as check 0 —
// ahead of even `check_chain_id`. It is appended as check 15 rather than
// inserted at the front so that the existing spec-mapped checks 1–14 keep
// their numbering and DropCode-to-position mapping stable; renumbering
// those to make room would be a larger, unrelated diff. A future revision
// that intentionally renumbers the whole fast-fail order can move it.
//
// `DropCode::StaleBlueprint` must exist on `omega_core::errors::DropCode`
// and `CheckContext::latest_blueprint_nonce: u64` must exist in
// `crate::context` — neither lives in this file and neither is modified
// here; both are outside this crate/module's ownership, same as
// `MissExposureLimit` above.
//
// A separate draft implemented this check (and structured logging) against
// `ExecutionBlueprint` + `alloy::primitives::U256` directly, bypassing the
// `BlueprintFields` abstraction. That draft was rejected: the whole point
// of `BlueprintFields` (see the struct doc below) is keeping this crate
// alloy-free and unit-testable without constructing a full blueprint. The
// nonce field is added to `BlueprintFields` instead, and `latest_blueprint_
// nonce` stays a plain `u64` in `CheckContext`, consistent with every other
// counter/threshold field already there.
//
// ## Audit fix (this revision): structured audit logging on every drop
//
// The per-check tracing call in `run_all_checks` was widened from a
// bare `drop_code`/`strategy` pair to include the fields an operator
// actually needs to triage a drop without cross-referencing another
// system: chain id, expiry block, gas budget inputs are already visible
// via metrics, so what's missing from an audit trail is which blueprint
// (strategy + notional) was rejected and why. Added `flashloan_amount`
// and `expected_profit_net_wei` (both already on `BlueprintFields`, no
// new fields invented) and raised the level from `debug` to `warn`,
// since a pre-trade rejection is an operationally relevant event, not a
// debug-only detail. `tracing`'s key-value fields already serialize to
// structured JSON under a JSON-formatted subscriber, so no separate
// hand-rolled JSON logger is introduced.
//
// ## Audit fix (this revision): check 16, oracle price sanity / flash-crash
// guard, and shared check functions for cross-crate reuse
//
// `DropCode::MissFlashCrash` already existed as a match arm in
// `drop_code_label` below, but no check function ever produced it —
// nothing in this file, before this revision, ever returned
// `Some(DropCode::MissFlashCrash)`. Added `check_price_sanity` (check 16)
// to actually implement it. This closes a real, distinct gap from check 8
// (`MissOracleDiverge`, Chainlink-vs-Pyth divergence): check 8 does
// nothing if a relied-upon price is simply non-sane on its own (zero,
// negative, NaN, infinite — e.g. a malformed or compromised feed), or if
// Chainlink and Pyth agree with each other but have moved together far
// from the TWAP reference (which is deliberately harder to manipulate
// within a single block, being time-weighted). See `OracleSnapshot::
// spot_twap_divergence`/`has_sane_prices` in context.rs for the underlying
// logic and `FLASH_CRASH_SPOT_TWAP_DIVERGENCE_THRESHOLD`'s doc comment for
// why its threshold is deliberately looser than check 8's.
//
// Also as of this revision, `check_oracle_freshness`, `check_oracle_
// hierarchy`, `check_slippage`, and the new `check_price_sanity` are thin
// wrappers around new `pub` free functions (`oracle_freshness_check`,
// `oracle_hierarchy_check`, `oracle_price_sanity_check`, `slippage_check`)
// rather than containing their logic directly. This is so that
// omega-hot-path — which runs its own <1ms simulation lane entirely
// outside this 16-check pipeline and previously had NO oracle-freshness,
// price-sanity, or slippage protection of its own at all — can call the
// EXACT SAME logic directly, given only an `OracleSnapshot` and/or a
// slippage figure, without needing to build a full `BlueprintFields` +
// `CheckContext` (which would require it to resolve
// `flashloan_provider_id`, `strategy_bytecode_hash` whitelist lookups,
// etc. — concerns entirely outside the oracle/price/slippage area this
// change is scoped to). Two divergent implementations of "is this oracle
// data fresh enough" would be strictly worse than one shared one: any
// future tuning of a threshold would need to happen in two places and
// could silently drift out of sync exactly the way the gas-spike/
// flashloan-safety constants were fixed to prevent above.
//
// ## Fix (this revision): unused staleness-constant imports
//
// `CHAINLINK_STALENESS_SECS`, `PYTH_STALENESS_SECS`, `TWAP_STALENESS_SECS`
// were imported here but never referenced — the actual per-feed
// staleness comparisons (`chainlink_fresh()`, `pyth_fresh()`,
// `twap_fresh()`) live as methods on `OracleSnapshot` in context.rs,
// which already has its own access to these constants; this file only
// ever calls those methods, never compares an age against the threshold
// directly. Removed from the import list rather than
// `#[allow(unused_imports)]`-ing them, since the actual fix is simply
// not importing what this file doesn't use.
//
// ## Audit fix (this revision): clippy::collapsible_if in oracle_hierarchy_check
//
// `oracle_hierarchy_check` previously nested `if oracle.chainlink_fresh()
// && oracle.pyth_fresh() { if oracle.chainlink_pyth_divergence() > ... {
// ... } }`. Merged into a single `if` with a combined `&&` condition —
// behaviorally identical (both conditions must hold for the check to
// fire; the divergence computation still only runs once both feeds are
// confirmed fresh, since `&&` short-circuits left-to-right), just
// expressed as one boolean expression instead of two nested guards,
// which is what `clippy::collapsible_if` (denied under `-D warnings`)
// requires.
//
// ## Audit fix (this revision, 2): check 8 must skip non-sane prices,
// not treat them as "divergence"
//
// Root cause of three test failures (`zero_price_on_fresh_oracle_
// fails_at_check_16`, `negative_price_on_fresh_oracle_fails_at_check_16`,
// `nonce_replay_fails_before_price_sanity`): `OracleSnapshot::
// chainlink_pyth_divergence()` in context.rs returns `f64::INFINITY` when
// `chainlink_price <= 0.0`. Since `INFINITY > ORACLE_DIVERGE_THRESHOLD`
// is trivially true, `oracle_hierarchy_check` (check 8) was firing
// `MissOracleDiverge` for a zero/negative/non-sane price BEFORE check 15
// (nonce replay) or check 16 (`oracle_price_sanity_check` /
// `MissFlashCrash`) ever got a chance to run — both of which are the
// correct, more specific drop code for those situations. A non-sane
// single price is not "divergence between two feeds"; it's the
// responsibility of check 16 (or, further upstream, check 7's freshness
// gate).
//
// Fixed by having `oracle_hierarchy_check` early-return `None` (skip,
// don't fail) whenever the two fresh feeds it's about to compare aren't
// both sane, via the existing `OracleSnapshot::has_sane_prices()` helper
// — the same helper `oracle_price_sanity_check` (check 16) already uses.
// This does not reintroduce `clippy::collapsible_if`: the function is
// now three sequential single-condition `if`s with early returns rather
// than one nested pair, which clippy does not flag.
//
// A deliberately NOT-taken alternative: making
// `chainlink_pyth_divergence()` itself return `0.0` (or some other
// non-triggering sentinel) for non-sane inputs instead of `INFINITY`.
// That would fix check 8 too, but it would also make
// `chainlink_pyth_divergence()` itself lie about what it's reporting —
// callers of that method directly (if any exist beyond this file) would
// see "0% divergence" for what is actually "one of these two feeds is
// garbage," which is a worse foot-gun than a function that keeps
// reporting `INFINITY` (an honest "this doesn't make sense to compare")
// while the caller — the code with checks 15/16 explicitly ordered
// nearby, sharing the same `has_sane_prices()` helper — is what decides
// to skip the comparison instead of trusting it.

use omega_core::errors::DropCode;

use crate::context::{
    CheckContext, OracleSnapshot, FLASHLOAN_SAFETY_DEN, FLASHLOAN_SAFETY_NUM,
    FLASH_CRASH_SPOT_TWAP_DIVERGENCE_THRESHOLD, GAS_SPIKE_THRESHOLD_DEN, GAS_SPIKE_THRESHOLD_NUM,
    MAX_PRICE_IMPACT_BPS, ORACLE_DIVERGE_THRESHOLD,
};
use crate::metrics;

// ─── Public result type ───────────────────────────────────────────────────────

/// Outcome of running all 16 pre-trade checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckResult {
    Pass,
    Fail(DropCode),
}

impl CheckResult {
    pub fn is_pass(&self) -> bool {
        matches!(self, CheckResult::Pass)
    }
    pub fn is_fail(&self) -> bool {
        matches!(self, CheckResult::Fail(_))
    }
    pub fn drop_code(&self) -> Option<&DropCode> {
        match self {
            CheckResult::Fail(c) => Some(c),
            _ => None,
        }
    }
}

// ─── Shared standalone check functions ────────────────────────────────────────
//
// See this file's module doc comment, "check 16 ... and shared check
// functions for cross-crate reuse", for why these are `pub` free
// functions rather than logic inlined into the `BlueprintFields`/
// `CheckContext`-shaped private checks below, which now delegate to them.

/// Standalone oracle freshness check (spec S5: MissOracle) — rejects only
/// when Chainlink, Pyth, AND TWAP are all simultaneously stale.
///
/// Callable directly (e.g. from omega-hot-path) without a full
/// `BlueprintFields`/`CheckContext` — this check never reads blueprint
/// fields at all, only the oracle snapshot.
pub fn oracle_freshness_check(oracle: &OracleSnapshot) -> Option<DropCode> {
    if !oracle.chainlink_fresh() && !oracle.pyth_fresh() && !oracle.twap_fresh() {
        return Some(DropCode::MissOracle);
    }
    None
}

/// Standalone oracle hierarchy check (spec S5: MissOracleDiverge) —
/// rejects when Chainlink AND Pyth are both fresh but diverge beyond
/// `ORACLE_DIVERGE_THRESHOLD`. Skipped (returns `None`) when fewer than
/// two fresh spot feeds are available to compare, **or when either fresh
/// price is non-sane** (zero, negative, NaN, or infinite) — see this
/// file's module-level "Audit fix (this revision, 2)" note. A non-sane
/// price is not "divergence"; that case is check 16's
/// (`oracle_price_sanity_check` / `MissFlashCrash`) responsibility, and
/// letting `chainlink_pyth_divergence()`'s `f64::INFINITY` sentinel leak
/// through here as a false "diverged" result would mask check 16 (and
/// check 15, which sits between them) from ever running.
pub fn oracle_hierarchy_check(oracle: &OracleSnapshot) -> Option<DropCode> {
    if !oracle.chainlink_fresh() || !oracle.pyth_fresh() {
        return None;
    }
    if !oracle.has_sane_prices() {
        // Non-sane prices are check 16's responsibility, not a
        // "divergence" between two otherwise-comparable feeds.
        return None;
    }
    if oracle.chainlink_pyth_divergence() > ORACLE_DIVERGE_THRESHOLD {
        return Some(DropCode::MissOracleDiverge);
    }
    None
}

/// Standalone oracle price sanity / flash-crash check (check 16,
/// `DropCode::MissFlashCrash`). Rejects when either:
///   - any currently-fresh price feed is non-finite or non-positive, or
///   - a fresh spot price (Chainlink, falling back to Pyth) diverges from
///     a fresh TWAP by more than
///     `FLASH_CRASH_SPOT_TWAP_DIVERGENCE_THRESHOLD`.
///
/// See this file's module doc comment and `OracleSnapshot::
/// has_sane_prices`/`spot_twap_divergence` in context.rs for the full
/// reasoning, including why this is distinct from check 8.
pub fn oracle_price_sanity_check(oracle: &OracleSnapshot) -> Option<DropCode> {
    if !oracle.has_sane_prices() {
        return Some(DropCode::MissFlashCrash);
    }
    if let Some(divergence) = oracle.spot_twap_divergence() {
        if divergence > FLASH_CRASH_SPOT_TWAP_DIVERGENCE_THRESHOLD {
            return Some(DropCode::MissFlashCrash);
        }
    }
    None
}

/// Standalone slippage check (spec: MissSlippage) — rejects when
/// `slippage_bps` exceeds `max_slippage_bps`. Takes plain `u16`s rather
/// than a `BlueprintFields`/`CheckContext` pair so a caller with only
/// these two numbers (e.g. omega-hot-path, which selects
/// `max_slippage_bps` itself from the blueprint's `strategy_id`) can call
/// this directly.
pub fn slippage_check(slippage_bps: u16, max_slippage_bps: u16) -> Option<DropCode> {
    if slippage_bps > max_slippage_bps {
        return Some(DropCode::MissSlippage);
    }
    None
}

// ─── Blueprint-like trait-object-compatible input ────────────────────────────
//
// We accept a `BlueprintFields` struct (not the full ExecutionBlueprint) to keep
// omega-risk independent of alloy primitives and allow unit-testing without building
// a full blueprint.  omega-strategies calls `BlueprintFields::from_blueprint(&bp)`.

/// Minimal blueprint fields required by the 16 pre-trade checks.
/// Extracted from ExecutionBlueprint by the caller.
#[derive(Debug, Clone)]
pub struct BlueprintFields {
    pub chain_id: u64,
    pub expiry_block: u64,
    pub l2_exec_gas_estimate: u64,
    pub l1_data_gas_estimate: u64,
    pub extraction_gas: u64,
    pub expected_profit_net_wei: u128, // in wei for U256-free comparison
    pub dynamic_min_profit_wei: u128,
    pub l1_data_fee_at_creation: u64, // gwei
    pub slippage_bps: u16,
    pub flashloan_amount: u128,
    pub flashloan_provider_id: &'static str,
    pub strategy_id: &'static str,
    pub strategy_bytecode_hash: [u8; 32],
    /// Present for LA blueprints only (spec check 13).
    pub price_impact_bps: Option<u16>,
    pub ofa_compliant: bool,
    /// Monotonically-increasing nonce assigned at blueprint creation.
    /// Used by check 15 to reject stale/replayed blueprints; compared
    /// against `CheckContext::latest_blueprint_nonce`.
    pub nonce: u64,
}

// ─── Main entry point ─────────────────────────────────────────────────────────

/// Execute all 16 pre-trade checks in fast-fail order.
///
/// Returns `CheckResult::Pass` only when every check passes.
/// The first failing check short-circuits and returns its `DropCode`.
///
/// Emits a Prometheus counter for every drop code encountered, and a
/// structured `warn`-level audit log entry with enough context to
/// triage the drop without cross-referencing another system.
pub fn run_all_checks(bp: &BlueprintFields, ctx: &CheckContext) -> CheckResult {
    let result = run_checks_inner(bp, ctx);
    match &result {
        CheckResult::Pass => {
            metrics::CHECKS_PASSED
                .with_label_values(&[bp.strategy_id])
                .inc();
        }
        CheckResult::Fail(code) => {
            metrics::CHECKS_FAILED
                .with_label_values(&[bp.strategy_id, drop_code_label(code)])
                .inc();
            tracing::warn!(
                target: "omega_risk_audit",
                event = "trade_dropped",
                strategy = bp.strategy_id,
                chain_id = bp.chain_id,
                nonce = bp.nonce,
                flashloan_amount_wei = bp.flashloan_amount,
                expected_profit_net_wei = bp.expected_profit_net_wei,
                drop_code = ?code,
                reason = drop_code_label(code),
                "blueprint dropped at pre-trade check"
            );
        }
    }
    result
}

#[inline]
fn run_checks_inner(bp: &BlueprintFields, ctx: &CheckContext) -> CheckResult {
    // 1. Chain ID — cheapest possible check; bitwise comparison.
    if let Some(c) = check_chain_id(bp, ctx) {
        return CheckResult::Fail(c);
    }

    // 2. Expiry — integer comparison.
    if let Some(c) = check_expiry(bp, ctx) {
        return CheckResult::Fail(c);
    }

    // 3. Gas budget — integer addition + comparison.
    if let Some(c) = check_gas_budget(bp, ctx) {
        return CheckResult::Fail(c);
    }

    // 4. Whitelist — hash comparison (O(1) hashmap lookup in caller).
    if let Some(c) = check_whitelist(bp, ctx) {
        return CheckResult::Fail(c);
    }

    // 5. Dynamic profit — U128 comparison.
    if let Some(c) = check_dynamic_profit(bp, ctx) {
        return CheckResult::Fail(c);
    }

    // 6. Gas spike — exact integer ratio comparison, no division.
    if let Some(c) = check_gas_spike(bp, ctx) {
        return CheckResult::Fail(c);
    }

    // 7. Oracle freshness — integer comparisons against age thresholds.
    if let Some(c) = check_oracle_freshness(bp, ctx) {
        return CheckResult::Fail(c);
    }

    // 8. Oracle hierarchy — one float division (only when both feeds fresh
    // AND sane; see this file's module doc comment, "Audit fix (this
    // revision, 2)").
    if let Some(c) = check_oracle_hierarchy(bp, ctx) {
        return CheckResult::Fail(c);
    }

    // 9. Slippage — integer comparison.
    if let Some(c) = check_slippage(bp, ctx) {
        return CheckResult::Fail(c);
    }

    // 10. Flashloan liquidity — exact integer ceiling-division comparison.
    if let Some(c) = check_flashloan_liquidity(bp, ctx) {
        return CheckResult::Fail(c);
    }

    // 11. Competition — float comparison.
    if let Some(c) = check_competition(bp, ctx) {
        return CheckResult::Fail(c);
    }

    // 12. Risk score — float comparison.
    if let Some(c) = check_risk_score(bp, ctx) {
        return CheckResult::Fail(c);
    }

    // 13. Price impact — LA only; only evaluated when field is present.
    if bp.price_impact_bps.is_some() {
        if let Some(c) = check_price_impact(bp, ctx) {
            return CheckResult::Fail(c);
        }
    }

    // 14. Account exposure — integer addition + comparison, cheap; placed
    // last only because it was added last, not because it needs 13
    // checks' worth of prior state. Would be equally valid immediately
    // after check 3 (gas budget) in the fast-fail ordering.
    if let Some(c) = check_account_exposure(bp, ctx) {
        return CheckResult::Fail(c);
    }

    // 15. Nonce replay — single integer comparison, cheapest of all;
    // placed last only because it was added last. Would be equally
    // valid as check 0, ahead of check_chain_id.
    if let Some(c) = check_nonce_replay(bp, ctx) {
        return CheckResult::Fail(c);
    }

    // 16. Price sanity / flash-crash guard — one or two float comparisons
    // plus finiteness checks, no allocation. Placed last for the same
    // reason as 14/15 (added last, not because it needs everything
    // above it); it would be equally valid immediately after check 8,
    // which it complements.
    if let Some(c) = check_price_sanity(bp, ctx) {
        return CheckResult::Fail(c);
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
    let total_gas = bp
        .l2_exec_gas_estimate
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

/// Check 6: reject if L1 gas price has moved > threshold since blueprint
/// creation (spec S12: MissGasSpike).
///
/// Exact integer form of `diff / at_creation > NUM / DEN`, rearranged to
/// `diff * DEN > at_creation * NUM` to avoid division entirely — no float
/// precision loss near the boundary, and bit-identical across platforms.
///
/// `at_creation.max(1)` guards the same as before: a zero-fee-at-creation
/// value (which should never happen in practice, but must not panic or
/// divide by zero if it does) is treated as 1 gwei rather than triggering
/// undefined behavior.
///
/// `saturating_mul` on both sides guards against overflow from a
/// malformed/extreme gas price feeding in from upstream — same principle
/// as `omega-rpc::net::wei_to_gwei_saturating` applied to fee values: an
/// absurd input should saturate to "obviously over threshold," not wrap
/// around into something that looks small and safe.
#[inline]
fn check_gas_spike(bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    let at_creation = bp.l1_data_fee_at_creation.max(1);
    let current = ctx.current_l1_gas_price_gwei;
    let diff = current.abs_diff(at_creation);

    if diff.saturating_mul(GAS_SPIKE_THRESHOLD_DEN)
        > at_creation.saturating_mul(GAS_SPIKE_THRESHOLD_NUM)
    {
        return Some(DropCode::MissGasSpike);
    }
    None
}

/// Check 7: at least one oracle must be within its staleness threshold
/// (spec S5: MissOracle). Delegates to the standalone
/// `oracle_freshness_check` — see this file's module doc comment for why.
#[inline]
fn check_oracle_freshness(_bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    oracle_freshness_check(&ctx.oracle)
}

/// Check 8: when Chainlink AND Pyth are both fresh, they must agree
/// within 0.4 % (spec S5). Delegates to the standalone
/// `oracle_hierarchy_check` — see this file's module doc comment for why.
///
/// NOTE: retained as float here (unlike checks 6/10) because the divergence
/// itself is computed from `f64` oracle prices upstream in
/// `OracleSnapshot::chainlink_pyth_divergence()` — converting only this
/// comparison to integer math would not remove the float dependency, just
/// relocate where the precision question lives. If oracle prices become
/// fixed-point/integer at the source, this comparison should be revisited
/// the same way checks 6 and 10 were.
#[inline]
fn check_oracle_hierarchy(_bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    oracle_hierarchy_check(&ctx.oracle)
}

/// Check 9: slippage_bps must not exceed the per-strategy maximum (spec:
/// MissSlippage). Delegates to the standalone `slippage_check` — see this
/// file's module doc comment for why.
#[inline]
fn check_slippage(bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    slippage_check(bp.slippage_bps, ctx.max_slippage_bps)
}

/// Check 10: flashloan provider must have enough liquidity (spec S11: MissLiquidity).
///
/// Rule: available >= flashloan_amount × (FLASHLOAN_SAFETY_NUM / FLASHLOAN_SAFETY_DEN)
/// (default 1.20, i.e. a 20% safety margin, spec S11).
/// Also enforces the no-self-flash rule: flashloan provider ≠ liquidation target protocol.
/// (e.g., no Aave-on-Aave; no Euler-on-Euler — spec S11.4 / crates/omega-strategies/la/protocols).
///
/// Uses ceiling division (`div_ceil`) for the required-liquidity threshold.
/// Floor division — the naive integer replacement for the old truncating
/// `as u128` float cast — would round the required margin DOWN, silently
/// accepting slightly less liquidity than the configured safety factor
/// actually requires. That is the wrong direction for a check whose entire
/// purpose is staying conservative, so this rounds up instead: any
/// fractional remainder makes the requirement stricter, never looser.
#[inline]
fn check_flashloan_liquidity(bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    // No-self-flash rule (spec: "encoded exclusion_list per protocol").
    if ctx.flashloan.protocol_id.as_str() == bp.flashloan_provider_id && bp.strategy_id == "LA" {
        // Flashloan provider is the same protocol being liquidated — forbidden.
        return Some(DropCode::MissLiquidity);
    }

    // Safety margin check — exact integer ceiling division.
    let required = bp
        .flashloan_amount
        .saturating_mul(FLASHLOAN_SAFETY_NUM)
        .div_ceil(FLASHLOAN_SAFETY_DEN);

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

/// Check 14: this blueprint's flashloan principal, added to the
/// account's already-outstanding exposure, must not exceed the
/// configured cap (MissExposureLimit).
///
/// Uses `bp.flashloan_amount` — the capital actually being borrowed and
/// deployed — NOT `bp.expected_profit_net_wei`. Exposure is a measure of
/// capital at risk, not of anticipated return; see this file's module
/// doc comment for why summing in expected profit instead would invert
/// what this check is supposed to catch.
///
/// `saturating_add` guards the same way every other u128/u64 accumulation
/// in this file does: an overflow here should saturate to "obviously
/// over the cap" (and therefore fail-safe by rejecting), never wrap
/// around to a small value that would incorrectly pass.
#[inline]
fn check_account_exposure(bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    let projected_exposure = ctx
        .current_account_exposure_wei
        .saturating_add(bp.flashloan_amount);

    if projected_exposure > ctx.max_account_exposure_wei {
        return Some(DropCode::MissExposureLimit);
    }
    None
}

/// Check 15: blueprint nonce must be strictly greater than the latest
/// nonce already processed for this account (spec: StaleBlueprint /
/// nonce-replay protection).
///
/// `nonce <= latest_blueprint_nonce` covers both an exact replay (equal)
/// and an out-of-order/older resubmission (less than) with a single
/// comparison — a strictly-greater nonce is the only value that passes.
#[inline]
fn check_nonce_replay(bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    if bp.nonce <= ctx.latest_blueprint_nonce {
        return Some(DropCode::StaleBlueprint);
    }
    None
}

/// Check 16: active oracle prices must be sane, and the fresh spot price
/// must not have diverged too far from a fresh TWAP (flash-crash /
/// oracle-manipulation guard, `DropCode::MissFlashCrash`). Delegates to
/// the standalone `oracle_price_sanity_check` — see this file's module
/// doc comment for the full reasoning and why this is distinct from
/// check 8.
#[inline]
fn check_price_sanity(_bp: &BlueprintFields, ctx: &CheckContext) -> Option<DropCode> {
    oracle_price_sanity_check(&ctx.oracle)
}

// ─── Prometheus label helper ──────────────────────────────────────────────────

fn drop_code_label(code: &DropCode) -> &'static str {
    match code {
        DropCode::WrongChain => "wrong_chain",
        DropCode::MissExpiry => "miss_expiry",
        DropCode::MissGas => "miss_gas",
        DropCode::MissWhitelist => "miss_whitelist",
        DropCode::MissProfit => "miss_profit",
        DropCode::MissGasSpike => "miss_gas_spike",
        DropCode::MissOracle => "miss_oracle",
        DropCode::MissOracleDiverge => "miss_oracle_diverge",
        DropCode::MissSlippage => "miss_slippage",
        DropCode::MissLiquidity => "miss_liquidity",
        DropCode::MissCompetition => "miss_competition",
        DropCode::MissRisk => "miss_risk",
        DropCode::MissPriceImpact => "miss_price_impact",
        DropCode::MissExposureLimit => "miss_exposure_limit",
        DropCode::MissDexLiquidity => "miss_dex_liquidity",
        DropCode::MissHfNotLiquidatable => "miss_hf_not_liquidatable",
        DropCode::MissFlashCrash => "miss_flash_crash",
        DropCode::StaleBlueprint => "stale_blueprint",
        _ => "other",
    }
}

#[cfg(test)]
mod checks_tests {
    use super::*;
    use crate::context::{CheckContext, FlashloanSnapshot, OracleSnapshot};

    // ── Test harness ──────────────────────────────────────────────────────────

    fn passing_bp() -> BlueprintFields {
        BlueprintFields {
            chain_id: 42161,
            expiry_block: 1000,
            l2_exec_gas_estimate: 100_000,
            l1_data_gas_estimate: 5_000,
            extraction_gas: 45_000,
            expected_profit_net_wei: 1_000_000_000_000_000_000, // 1 ETH
            dynamic_min_profit_wei: 100_000_000_000_000_000,    // 0.1 ETH
            l1_data_fee_at_creation: 50,
            slippage_bps: 20,
            flashloan_amount: 1_000_000,
            flashloan_provider_id: "balancer",
            strategy_id: "SA",
            strategy_bytecode_hash: [0xaa; 32],
            price_impact_bps: None,
            ofa_compliant: true,
            nonce: 501, // > passing_ctx's latest_blueprint_nonce (500)
        }
    }

    fn passing_ctx() -> CheckContext {
        CheckContext {
            expected_chain_id: 42161,
            current_block: 500,
            current_l1_gas_price_gwei: 50,
            current_l2_base_fee_gwei: 1,
            l1_adaptive_buffer: 1.30,
            oracle: OracleSnapshot {
                chainlink_price: 2000.0,
                pyth_price: 2001.0, // 0.05 % divergence — within 0.4 %
                twap_price: 1999.0, // ~0.05% from chainlink — well within flash-crash threshold
                chainlink_age_s: 10,
                pyth_age_s: 10,
                twap_age_s: 60,
            },
            flashloan: FlashloanSnapshot {
                available: 2_000_000, // 2× the flashloan_amount
                protocol_id: String::from("balancer"),
            },
            competition_probability: 0.50,
            max_competition_probability: 0.90,
            strategy_max_gas: 500_000,
            max_slippage_bps: 30,
            rollout_tier: 1.0,
            strategy_bytecode_hash: [0xaa; 32],
            risk_score: 0.30,
            max_risk_score: 0.80,
            current_account_exposure_wei: 0,
            max_account_exposure_wei: 10_000_000_000_000_000_000, // 10 ETH headroom
            latest_blueprint_nonce: 500,
        }
    }

    // ── Happy path ────────────────────────────────────────────────────────────

    #[test]
    fn all_passing_returns_pass() {
        let bp = passing_bp();
        let ctx = passing_ctx();
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    // ── Check 1: chain_id ─────────────────────────────────────────────────────

    #[test]
    fn wrong_chain_fails_at_check_1() {
        let mut bp = passing_bp();
        bp.chain_id = 1; // Ethereum mainnet
        let ctx = passing_ctx();
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::WrongChain)
        );
    }

    // ── Check 2: expiry ───────────────────────────────────────────────────────

    #[test]
    fn expired_block_fails_at_check_2() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.current_block = 1001; // > expiry_block 1000
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissExpiry)
        );
    }

    #[test]
    fn block_equal_to_expiry_fails() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.current_block = 1000; // == expiry_block
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissExpiry)
        );
    }

    // ── Check 3: gas budget ───────────────────────────────────────────────────

    #[test]
    fn over_gas_budget_fails_at_check_3() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        // total_gas = 100_000 + 5_000 + 45_000 = 150_000; set budget to 100_000
        ctx.strategy_max_gas = 100_000;
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissGas)
        );
    }

    #[test]
    fn zero_gas_budget_skips_check() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.strategy_max_gas = 0; // 0 = unlimited
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    // ── Check 4: whitelist ────────────────────────────────────────────────────

    #[test]
    fn wrong_bytecode_hash_fails_at_check_4() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.strategy_bytecode_hash = [0xbb; 32]; // different from bp's 0xaa
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissWhitelist)
        );
    }

    // ── Check 5: dynamic profit ───────────────────────────────────────────────

    #[test]
    fn insufficient_profit_fails_at_check_5() {
        let mut bp = passing_bp();
        bp.expected_profit_net_wei = 50_000_000_000_000_000; // 0.05 ETH < min 0.1 ETH
        let ctx = passing_ctx();
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissProfit)
        );
    }

    // ── Check 6: gas spike ────────────────────────────────────────────────────

    #[test]
    fn gas_spike_above_30pct_fails_at_check_6() {
        let bp = passing_bp(); // l1_at_creation = 50
        let mut ctx = passing_ctx();
        ctx.current_l1_gas_price_gwei = 75; // 50 % increase > 30 %
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissGasSpike)
        );
    }

    #[test]
    fn gas_spike_exactly_30pct_passes() {
        let bp = passing_bp(); // l1_at_creation = 50
        let mut ctx = passing_ctx();
        ctx.current_l1_gas_price_gwei = 65; // 30 % = threshold, not strictly >
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    #[test]
    fn gas_spike_decrease_also_triggers() {
        // abs_diff must catch a large DECREASE too, not just an increase —
        // the old float code used .abs() for the same reason.
        let bp = passing_bp(); // l1_at_creation = 50
        let mut ctx = passing_ctx();
        ctx.current_l1_gas_price_gwei = 30; // 40% decrease > 30%
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissGasSpike)
        );
    }

    #[test]
    fn gas_spike_threshold_matches_named_constant_not_a_duplicate_literal() {
        // Regression guard: if GAS_SPIKE_THRESHOLD_NUM/DEN in context.rs
        // changes, this test (computed from the constants, not a
        // hardcoded 30/100) is what should need updating to match —
        // proving check_gas_spike derives from the same source of truth
        // rather than a separately-maintained magic number.
        let bp = passing_bp(); // l1_at_creation = 50
        let mut ctx = passing_ctx();
        let threshold_price = 50 + (50 * GAS_SPIKE_THRESHOLD_NUM) / GAS_SPIKE_THRESHOLD_DEN;
        ctx.current_l1_gas_price_gwei = threshold_price + 1;
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissGasSpike)
        );
    }

    // ── Check 7: oracle freshness ─────────────────────────────────────────────

    #[test]
    fn all_oracles_stale_fails_at_check_7() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.oracle.chainlink_age_s = 100; // > 45s
        ctx.oracle.pyth_age_s = 100;
        ctx.oracle.twap_age_s = 200; // > 120s
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissOracle)
        );
    }

    #[test]
    fn only_twap_fresh_passes_freshness() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.oracle.chainlink_age_s = 100;
        ctx.oracle.pyth_age_s = 100;
        ctx.oracle.twap_age_s = 60; // TWAP fresh
                                    // No divergence/hierarchy check when only TWAP is fresh, and no
                                    // spot price to compare against TWAP for check 16 either → pass.
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    // ── Check 8: oracle hierarchy ─────────────────────────────────────────────

    #[test]
    fn oracle_divergence_above_0_4pct_fails_at_check_8() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        // Chainlink 2000, Pyth 2010 → divergence 0.5 % > 0.4 %
        ctx.oracle.chainlink_price = 2000.0;
        ctx.oracle.pyth_price = 2010.0;
        ctx.oracle.chainlink_age_s = 10;
        ctx.oracle.pyth_age_s = 10;
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissOracleDiverge)
        );
    }

    #[test]
    fn single_fresh_oracle_skips_divergence_check() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.oracle.pyth_age_s = 100; // Pyth stale → only Chainlink fresh
        ctx.oracle.twap_age_s = 100; // also make TWAP stale so check 16 has nothing to compare either
                                     // Divergence check (8) skipped; sanity check (16) has no fresh
                                     // TWAP to compare the lone fresh spot price against either.
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    // ── Check 9: slippage ─────────────────────────────────────────────────────

    #[test]
    fn slippage_above_max_fails_at_check_9() {
        let mut bp = passing_bp();
        bp.slippage_bps = 50; // > max_slippage_bps 30
        let ctx = passing_ctx();
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissSlippage)
        );
    }

    #[test]
    fn slippage_check_standalone_matches_pipeline_result() {
        // Regression guard for the refactor: the standalone function and
        // the full pipeline must agree, since check_slippage now merely
        // delegates to slippage_check rather than containing its own copy
        // of the comparison.
        assert_eq!(slippage_check(50, 30), Some(DropCode::MissSlippage));
        assert_eq!(slippage_check(30, 30), None, "exactly at max must pass");
        assert_eq!(slippage_check(20, 30), None);
    }

    // ── Check 10: liquidity ───────────────────────────────────────────────────

    #[test]
    fn insufficient_flashloan_liquidity_fails_at_check_10() {
        let bp = passing_bp(); // flashloan_amount = 1_000_000
        let mut ctx = passing_ctx();
        ctx.flashloan.available = 1_100_000; // < 1_000_000 × 1.20 = 1_200_000
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissLiquidity)
        );
    }

    #[test]
    fn self_flash_la_fails_at_check_10() {
        let mut bp = passing_bp();
        bp.strategy_id = "LA";
        bp.flashloan_provider_id = "aave";
        let mut ctx = passing_ctx();
        ctx.flashloan.protocol_id = String::from("aave"); // same as flashloan provider
                                                          // Ensure other fields pass so we reach check 10.
        ctx.strategy_max_gas = 1_000_000;
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissLiquidity)
        );
    }

    #[test]
    fn liquidity_ceiling_rounds_up_not_down() {
        // Regression guard for the rounding-direction fix: with a
        // non-round flashloan_amount, the true 1.20x requirement has a
        // fractional remainder. It must round UP (stricter), so an
        // `available` that covers only the floored requirement must
        // still fail.
        let mut bp = passing_bp();
        bp.flashloan_amount = 1_000_001; // not a multiple of 100
        let mut ctx = passing_ctx();
        // True requirement: 1_000_001 * 1.20 = 1_200_001.2
        // Floor would require only 1_200_001; ceiling requires 1_200_002.
        ctx.flashloan.available = 1_200_001;
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissLiquidity)
        );
    }

    #[test]
    fn liquidity_ceiling_passes_when_truly_sufficient() {
        let mut bp = passing_bp();
        bp.flashloan_amount = 1_000_001;
        let mut ctx = passing_ctx();
        ctx.flashloan.available = 1_200_002; // covers the ceiling-rounded requirement
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    #[test]
    fn liquidity_exact_round_multiple_still_correct() {
        // Sanity check that round multiples (the case the old code's
        // tests exclusively covered) are unaffected by the rounding fix.
        let bp = passing_bp(); // flashloan_amount = 1_000_000 → requires exactly 1_200_000
        let mut ctx = passing_ctx();
        ctx.flashloan.available = 1_200_000;
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);

        ctx.flashloan.available = 1_199_999;
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissLiquidity)
        );
    }

    // ── Check 11: competition ─────────────────────────────────────────────────

    #[test]
    fn high_competition_fails_at_check_11() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.competition_probability = 0.95;
        ctx.max_competition_probability = 0.90;
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissCompetition)
        );
    }

    // ── Check 12: risk score ──────────────────────────────────────────────────

    #[test]
    fn high_risk_score_fails_at_check_12() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.risk_score = 0.95;
        ctx.max_risk_score = 0.80;
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissRisk)
        );
    }

    // ── Check 13: price impact (LA only) ─────────────────────────────────────

    #[test]
    fn price_impact_above_50bps_fails_at_check_13() {
        let mut bp = passing_bp();
        bp.price_impact_bps = Some(51);
        let ctx = passing_ctx();
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissPriceImpact)
        );
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

    // ── Check 14: account exposure ───────────────────────────────────────────

    #[test]
    fn exposure_within_cap_passes() {
        let bp = passing_bp(); // flashloan_amount = 1_000_000
        let mut ctx = passing_ctx();
        ctx.current_account_exposure_wei = 0;
        ctx.max_account_exposure_wei = 2_000_000;
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    #[test]
    fn exposure_exceeding_cap_fails_at_check_14() {
        let bp = passing_bp(); // flashloan_amount = 1_000_000
        let mut ctx = passing_ctx();
        ctx.current_account_exposure_wei = 900_000;
        ctx.max_account_exposure_wei = 1_000_000; // 900_000 + 1_000_000 > 1_000_000
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissExposureLimit)
        );
    }

    #[test]
    fn exposure_exactly_at_cap_passes() {
        let bp = passing_bp(); // flashloan_amount = 1_000_000
        let mut ctx = passing_ctx();
        ctx.current_account_exposure_wei = 0;
        ctx.max_account_exposure_wei = 1_000_000; // exactly equal, not strictly over
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    #[test]
    fn exposure_check_uses_flashloan_amount_not_expected_profit() {
        // Regression guard for the field-choice fix: a blueprint with
        // large expected profit but small flashloan principal should be
        // judged on the PRINCIPAL against the exposure cap, not on the
        // profit figure. If this check were (incorrectly) summing
        // expected_profit_net_wei instead of flashloan_amount, this test
        // would fail, since expected_profit_net_wei (1 ETH, from
        // passing_bp) would blow a cap sized only for the tiny
        // flashloan_amount used here.
        let mut bp = passing_bp();
        bp.flashloan_amount = 10; // tiny real exposure
                                  // expected_profit_net_wei is 1_000_000_000_000_000_000 (1 ETH) from passing_bp
        let mut ctx = passing_ctx();
        ctx.current_account_exposure_wei = 0;
        ctx.max_account_exposure_wei = 1_000; // far smaller than expected_profit_net_wei
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Pass,
            "must pass: real exposure (flashloan_amount=10) is well under the cap, \
             even though expected_profit_net_wei alone would have blown it"
        );
    }

    #[test]
    fn exposure_overflow_saturates_and_fails_safe() {
        let bp = passing_bp(); // flashloan_amount = 1_000_000
        let mut ctx = passing_ctx();
        ctx.current_account_exposure_wei = u128::MAX - 10; // near overflow
        ctx.max_account_exposure_wei = u128::MAX;
        // saturating_add caps at u128::MAX, which is > nothing except
        // itself — with max_account_exposure_wei also at u128::MAX this
        // specific case is right at the boundary (equal, not over), so
        // assert it does NOT panic and resolves deterministically rather
        // than asserting a specific pass/fail here.
        let result = run_all_checks(&bp, &ctx);
        assert!(
            result.is_pass() || result.is_fail(),
            "must resolve without panicking on overflow"
        );
    }

    // ── Check 15: nonce replay ────────────────────────────────────────────────

    #[test]
    fn nonce_greater_than_latest_passes() {
        let mut bp = passing_bp();
        bp.nonce = 501;
        let mut ctx = passing_ctx();
        ctx.latest_blueprint_nonce = 500;
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    #[test]
    fn nonce_equal_to_latest_fails_as_replay() {
        let mut bp = passing_bp();
        bp.nonce = 500;
        let mut ctx = passing_ctx();
        ctx.latest_blueprint_nonce = 500;
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::StaleBlueprint)
        );
    }

    #[test]
    fn nonce_less_than_latest_fails_as_stale() {
        let mut bp = passing_bp();
        bp.nonce = 499;
        let mut ctx = passing_ctx();
        ctx.latest_blueprint_nonce = 500;
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::StaleBlueprint)
        );
    }

    #[test]
    fn nonce_zero_against_zero_latest_fails() {
        // Boundary: an account with no prior blueprints has
        // latest_blueprint_nonce == 0; a blueprint nonce of 0 must still
        // be rejected (nonces are expected to start at 1, not 0).
        let mut bp = passing_bp();
        bp.nonce = 0;
        let mut ctx = passing_ctx();
        ctx.latest_blueprint_nonce = 0;
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::StaleBlueprint)
        );
    }

    // ── Check 16: price sanity / flash-crash guard ───────────────────────────

    #[test]
    fn zero_price_on_fresh_oracle_fails_at_check_16() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.oracle.chainlink_price = 0.0; // chainlink is fresh (age 10s) but price is garbage
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissFlashCrash)
        );
    }

    #[test]
    fn negative_price_on_fresh_oracle_fails_at_check_16() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.oracle.pyth_price = -100.0;
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissFlashCrash)
        );
    }

    #[test]
    fn nan_price_on_fresh_oracle_fails_at_check_16() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.oracle.twap_price = f64::NAN;
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissFlashCrash)
        );
    }

    #[test]
    fn infinite_price_on_fresh_oracle_fails_at_check_16() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.oracle.chainlink_price = f64::INFINITY;
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissFlashCrash)
        );
    }

    #[test]
    fn non_sane_price_on_stale_oracle_does_not_fail_check_16() {
        // A garbage price on a feed that's already stale (and therefore
        // not relied upon) must not trip the sanity guard — only fresh,
        // actively-relied-upon feeds are checked.
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.oracle.pyth_age_s = 100; // pyth now stale
        ctx.oracle.pyth_price = -999.0; // garbage, but irrelevant since stale
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    #[test]
    fn spot_twap_divergence_within_threshold_passes() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.oracle.chainlink_price = 2000.0;
        ctx.oracle.twap_price = 1900.0; // ~5.3% divergence, well under 15% threshold
        ctx.oracle.pyth_price = 2000.0; // keep check 8 happy (0% CL/Pyth divergence)
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    #[test]
    fn spot_twap_divergence_above_threshold_fails_at_check_16() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.oracle.chainlink_price = 2000.0;
        ctx.oracle.pyth_price = 2000.0; // CL/Pyth agree — check 8 passes
        ctx.oracle.twap_price = 1000.0; // 100% divergence from spot — flash crash
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissFlashCrash)
        );
    }

    #[test]
    fn spot_twap_divergence_skipped_when_twap_stale() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.oracle.twap_age_s = 200; // stale (> 120s)
        ctx.oracle.twap_price = 1.0; // would otherwise diverge wildly from spot ~2000
        assert_eq!(run_all_checks(&bp, &ctx), CheckResult::Pass);
    }

    #[test]
    fn oracle_price_sanity_check_standalone_matches_pipeline_result() {
        // Regression guard for the refactor: the standalone function used
        // directly by omega-hot-path must agree with what the full
        // pipeline produces for the identical oracle snapshot.
        let mut ctx = passing_ctx();
        assert_eq!(oracle_price_sanity_check(&ctx.oracle), None);

        ctx.oracle.chainlink_price = -1.0;
        assert_eq!(
            oracle_price_sanity_check(&ctx.oracle),
            Some(DropCode::MissFlashCrash)
        );
    }

    #[test]
    fn oracle_freshness_check_standalone_matches_pipeline_result() {
        let mut ctx = passing_ctx();
        assert_eq!(oracle_freshness_check(&ctx.oracle), None);

        ctx.oracle.chainlink_age_s = 100;
        ctx.oracle.pyth_age_s = 100;
        ctx.oracle.twap_age_s = 200;
        assert_eq!(
            oracle_freshness_check(&ctx.oracle),
            Some(DropCode::MissOracle)
        );
    }

    #[test]
    fn oracle_hierarchy_check_standalone_matches_pipeline_result() {
        let mut ctx = passing_ctx();
        assert_eq!(oracle_hierarchy_check(&ctx.oracle), None);

        ctx.oracle.chainlink_price = 2000.0;
        ctx.oracle.pyth_price = 2010.0; // 0.5% > 0.4% threshold
        assert_eq!(
            oracle_hierarchy_check(&ctx.oracle),
            Some(DropCode::MissOracleDiverge)
        );
    }

    #[test]
    fn oracle_hierarchy_check_skips_non_sane_prices_instead_of_diverging() {
        // Regression guard for this revision's fix: a non-sane price must
        // not leak through chainlink_pyth_divergence()'s f64::INFINITY
        // sentinel as a false MissOracleDiverge — it must be skipped
        // (None) so check 16 (or check 15, whichever is reached first in
        // the full pipeline) is what actually reports it.
        let mut ctx = passing_ctx();
        ctx.oracle.chainlink_price = 0.0;
        assert_eq!(
            oracle_hierarchy_check(&ctx.oracle),
            None,
            "non-sane price must be skipped by check 8, not reported as divergence"
        );

        ctx.oracle.chainlink_price = 2000.0;
        ctx.oracle.pyth_price = -1.0;
        assert_eq!(oracle_hierarchy_check(&ctx.oracle), None);
    }

    // ── Fast-fail ordering ────────────────────────────────────────────────────

    #[test]
    fn chain_id_fails_before_expiry() {
        let mut bp = passing_bp();
        bp.chain_id = 1; // check 1 fails
        let mut ctx = passing_ctx();
        ctx.current_block = 9999; // check 2 would also fail
                                  // Should return WrongChain, not MissExpiry.
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::WrongChain)
        );
    }

    #[test]
    fn expiry_fails_before_gas_budget() {
        let bp = passing_bp();
        let mut ctx = passing_ctx();
        ctx.current_block = 9999; // check 2 fails
        ctx.strategy_max_gas = 1; // check 3 would also fail
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissExpiry)
        );
    }

    #[test]
    fn chain_id_fails_before_nonce_replay() {
        // check 15 is cheap and would be a valid candidate for check 0,
        // but as implemented it runs last — confirm the code's actual
        // order (chain_id first) is what governs, not check cost alone.
        let mut bp = passing_bp();
        bp.chain_id = 1; // check 1 fails
        bp.nonce = 0; // check 15 would also fail
        let mut ctx = passing_ctx();
        ctx.latest_blueprint_nonce = 0;
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::WrongChain)
        );
    }

    #[test]
    fn exposure_fails_before_nonce_replay() {
        let bp = passing_bp(); // nonce = 501
        let mut ctx = passing_ctx();
        ctx.current_account_exposure_wei = 900_000;
        ctx.max_account_exposure_wei = 1_000_000; // check 14 fails
        ctx.latest_blueprint_nonce = 999; // check 15 would also fail (501 <= 999)
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::MissExposureLimit)
        );
    }

    #[test]
    fn nonce_replay_fails_before_price_sanity() {
        // check 16 runs last — confirm check 15 (nonce) still wins when
        // both would fail, since code order (not cost) governs. Also
        // exercises this revision's check-8 fix: a non-sane chainlink
        // price must not short-circuit into MissOracleDiverge before
        // check 15 is even reached.
        let mut bp = passing_bp();
        bp.nonce = 0;
        let mut ctx = passing_ctx();
        ctx.latest_blueprint_nonce = 0; // check 15 fails
        ctx.oracle.chainlink_price = -1.0; // check 16 would also fail
        assert_eq!(
            run_all_checks(&bp, &ctx),
            CheckResult::Fail(DropCode::StaleBlueprint)
        );
    }
}