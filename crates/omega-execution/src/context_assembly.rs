// crates/omega-execution/src/context_assembly.rs
//
// Stage 2 CheckContext production assembly (ProductionIntegrationPlan.md C3).
//
// ## Revision 2 — real sources confirmed this session, replacing rev 1's
// trait-seam placeholders where a real signature is now available.
//
// CONFIRMED against pasted source (this session):
//   - omega_oracle::chainlink::ChainlinkOracle::read(&self, token: &str)
//     -> Option<OraclePrice { price_usd, age_secs, .. }>            (chainlink.rs)
//   - omega_oracle::pyth::PythOracle::read(&self, token: &str)
//     -> Option<OraclePrice>, confidence-gated internally             (pyth.rs)
//   - omega_oracle::twap::TwapOracle::read                            (twap.rs, rev 1)
//   - omega_oracle::per_chain::PerChainOracle::snapshot(&self)
//     -> Arc<EilSnapshot { fee: FeeSnapshot { base_fee_gwei,
//     l1_data_fee_gwei, priority_fee_gwei, block_number }, .. }>      (per_chain.rs)
//   - omega_rpc::client::OmegaRpcClient::fetch_fee_snapshot(&self)
//     -> anyhow::Result<FeeSnapshot>                                  (client.rs)
//   - omega_risk::competition::competition_probability(
//       asset_tier: AssetTier, health_factor: f64, liquidation_eth: f64
//     ) -> f64                                                        (competition.rs)
//   - omega_flashloan::LiquidityRegistry::snapshot                    (rev 1, unchanged)
//   - omega_security::IntegrityRegistry::snapshot / StrategyEntry      (rev 1, unchanged)
//
// STILL NOT CONFIRMED (unchanged from rev 1 — no new source pasted for
// these; still modeled behind traits so nothing here is guessed):
//   - RiskScoreSource: no risk-score producer exists in ANY pasted file.
//     Fails closed to f64::MAX (check 12 always fails until wired).
//   - LatestNonceSource: `omega_security::replay::NonceRegistry` is
//     exported by name from omega-security's lib.rs but its methods were
//     never pasted. Fails closed by erroring (not defaulting to 0).
//   - AccountExposureSource: no exposure tracker exists in pasted code.
//
// ## Two candidate gas sources — this revision picks one, documents why
//
// Both `PerChainOracle::snapshot().fee` (cached, updated by the
// `run_fee_oracle` background task on FeeOracleEvent) and
// `OmegaRpcClient::fetch_fee_snapshot()` (a live, synchronous
// `eth_getBlockByNumber` call per invocation) now have confirmed real
// signatures. This revision uses `PerChainOracle::snapshot().fee` as the
// PRIMARY gas source:
//
//   - Latency: `snapshot()` is an `ArcSwap` load — lock-free, no network
//     round trip. `fetch_fee_snapshot()` is a real RPC call gated by
//     `OmegaRpcClient`'s own read-rate-limiter (client.rs) — meaningfully
//     slower and consumes a rate-limit token on every single blueprint
//     submission, which does not scale the way a lock-free cache read
//     does.
//   - Freshness: bounded by Arbitrum's ~250ms block cadence (per_chain.rs
//     publishes a fresh FeeOracle signal on every FeeOracleEvent, i.e.
//     every block) — well within what C3's "assembled fresh at
//     submission, not from blueprint-construction-time cache" requires;
//     a blueprint is typically only tens of ms to low-seconds old at
//     submission, so a same-block-or-one-block-old gas read is still a
//     materially fresher comparison point than the blueprint's own
//     `l1_data_fee_at_creation`.
//
// `GasPriceSource` (rev 1's trait) is kept as a fallback path — if
// `PerChainOracle`'s cache has never been populated (e.g. process just
// started, no block seen yet), this function falls through to a live
// `fetch_fee_snapshot()` call rather than silently returning `0` for
// "current" gas (which would falsely look like a 100% gas-price DROP
// against any nonzero creation-time fee and could reject good blueprints,
// OR — the more dangerous direction — mask a genuine spike if
// `l1_data_fee_at_creation` also happened to be near 0). Both paths are
// wired in below rather than picking only one.
//
// KNOWN DATA GAP (not fixed here, surfaced instead): both
// `FeeSnapshot.l1_data_fee_gwei` sources — `fetch_fee_snapshot()`
// (client.rs) and `FeeOracleEvent` → `run_fee_oracle` (per_chain.rs) —
// hardcode this field to `0` today ("populated by ArbGasInfo; 0 here as
// default" / "populated by ArbGasInfo" per their own comments). Neither
// pasted file reads the ArbGasInfo precompile. This function passes that
// `0` straight through as `current_l1_gas_price_gwei` rather than
// fabricating a plausible-looking value — meaning check 6 (gas spike)
// is effectively comparing every blueprint's real
// `l1_data_fee_at_creation` against a constant 0 until ArbGasInfo
// integration lands upstream of this file. This is flagged, not hidden:
// see `GasReadout::l1_data_fee_is_real` below, which callers can log or
// alert on.

use std::sync::Arc;

use omega_flashloan::{FlashloanProvider, LiquidityRegistry};
use omega_oracle::chainlink::ChainlinkOracle;
use omega_oracle::per_chain::PerChainOracle;
use omega_oracle::pyth::PythOracle;
use omega_oracle::twap::TwapOracle;
use omega_risk::competition::{competition_probability, AssetTier};
use omega_risk::context::{CheckContext, FlashloanSnapshot, OracleSnapshot};
use omega_rpc::client::OmegaRpcClient;
use omega_security::IntegrityRegistry;

use crate::error::ExecutionError;

// ─── Still-unconfirmed subsystem seams (unchanged from rev 1) ───────────────

/// No risk-score producer exists anywhere in the pasted codebase. Fails
/// closed to `f64::MAX` when unimplemented — see module doc comment.
pub trait RiskScoreSource: Send + Sync {
    fn current_risk_score(&self, strategy_id: &str) -> Option<f64>;
}

/// `omega_security::replay::NonceRegistry`'s real methods were never
/// pasted. Fails closed by erroring, never by defaulting to `0`.
pub trait LatestNonceSource: Send + Sync {
    fn latest_nonce(&self, scope: &str) -> anyhow::Result<u64>;
}

/// No exposure tracker exists anywhere in the pasted codebase.
pub trait AccountExposureSource: Send + Sync {
    fn current_exposure_wei(&self, scope: &str) -> u128;
}

/// Per-strategy limits — no config loader was pasted; caller-supplied.
#[derive(Debug, Clone)]
pub struct StrategyLimits {
    pub max_gas: u64,
    pub max_slippage_bps: u16,
    pub max_risk_score: f64,
    pub max_account_exposure_wei: u128,
    pub max_competition_probability: f64,
    pub rollout_tier: f64,
    pub l1_adaptive_buffer: f64,
}

// ─── Live handle bundle ──────────────────────────────────────────────────────

pub struct LiveContextHandles {
    pub chainlink: Arc<ChainlinkOracle>,
    pub pyth: Arc<PythOracle>,
    pub twap: Arc<TwapOracle>,
    pub per_chain: Arc<PerChainOracle>,
    pub rpc: Arc<OmegaRpcClient>,
    pub flashloan_registry: Arc<LiquidityRegistry>,
    pub integrity_registry: Arc<IntegrityRegistry>,
    pub risk: Arc<dyn RiskScoreSource>,
    pub nonces: Arc<dyn LatestNonceSource>,
    pub exposure: Arc<dyn AccountExposureSource>,
}

/// Optional inputs for the competition model — only meaningful for
/// LA-style blueprints against a specific target position. `None`
/// intentionally fails CLOSED (see `assemble_check_context` below), not
/// open, since a missing competition read is not the same thing as "no
/// competition."
pub struct CompetitionInputs {
    pub health_factor: f64,
    pub liquidation_eth: f64,
}

pub struct AssemblyInput<'a> {
    pub chain_id: u64,
    pub current_block: u64,
    pub strategy_id: &'a str,
    pub token_symbol: &'a str,
    pub limits: StrategyLimits,
    pub flashloan_provider: Option<FlashloanProvider>,
    pub flashloan_contract: Option<alloy_primitives::Address>,
    pub competition: Option<CompetitionInputs>,
}

/// Reports which gas path was actually used and whether the L1 data fee
/// is real or the known-zero placeholder — see module doc comment's
/// "KNOWN DATA GAP" note. Attach this to logs/metrics at the call site;
/// it is not part of `CheckContext` itself.
#[derive(Debug, Clone, Copy)]
pub struct GasReadout {
    pub source: GasSource,
    pub l1_data_fee_is_real: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GasSource {
    PerChainOracleCache,
    LiveRpcFallback,
}

// ─── The assembly function ───────────────────────────────────────────────────

pub async fn assemble_check_context(
    handles: &LiveContextHandles,
    input: &AssemblyInput<'_>,
) -> Result<(CheckContext, GasReadout), ExecutionError> {
    // ── strategy_bytecode_hash: from IntegrityRegistry (unchanged) ───────
    let strategy_bytecode_hash = handles
        .integrity_registry
        .snapshot()
        .into_iter()
        .find(|e| e.strategy_id == input.strategy_id)
        .map(|e| e.bytecode_hash)
        .ok_or_else(|| ExecutionError::UnknownFlashloanProvider {
            address: format!("strategy_id={}", input.strategy_id),
        })?;

    // ── gas: PerChainOracle cache first, live RPC fallback ───────────────
    let cached_fee = handles.per_chain.snapshot().fee.clone();
    let (l2_base_fee, l1_data_fee, gas_source) = if cached_fee.block_number > 0 {
        (
            cached_fee.base_fee_gwei,
            cached_fee.l1_data_fee_gwei,
            GasSource::PerChainOracleCache,
        )
    } else {
        // Cache never populated (no block observed yet) — fall through to
        // a live read rather than silently returning 0/0, which would be
        // indistinguishable from "gas price is genuinely zero."
        let live = handles
            .rpc
            .fetch_fee_snapshot()
            .await
            .map_err(|e| ExecutionError::GasSourceUnavailable(e.to_string()))?;
        (live.base_fee_gwei, live.l1_data_fee_gwei, GasSource::LiveRpcFallback)
    };
    let gas_readout = GasReadout {
        source: gas_source,
        // Both sources hardcode this to 0 today (see module doc comment)
        // — l1_data_fee is real only once ArbGasInfo integration lands.
        // Compare against the literal, not "is nonzero", so this stays
        // correct if some future path legitimately reports a real 0.
        l1_data_fee_is_real: false,
    };

    // ── oracle: real Chainlink + Pyth + TWAP caches ───────────────────────
    let (chainlink_price, chainlink_age_s) = match handles.chainlink.read(input.token_symbol) {
        Some(p) => (p.price_usd, p.age_secs),
        None => (0.0, u64::MAX), // no cache entry -> non-sane + maximally stale, never silently trusted
    };
    let (pyth_price, pyth_age_s) = match handles.pyth.read(input.token_symbol) {
        // PythOracle::read already returns None for a wide confidence
        // interval (pyth.rs's own confidence_ok() gate) as well as a
        // missing entry — both collapse to the same fail-closed sentinel
        // here, which is correct: this function has no way (or need) to
        // distinguish "never updated" from "updated but too uncertain to
        // trust" — oracle_freshness_check/oracle_hierarchy_check treat
        // both identically anyway (not fresh -> not used).
        Some(p) => (p.price_usd, p.age_secs),
        None => (0.0, u64::MAX),
    };
    let (twap_price, twap_age_s) = match handles.twap.read(input.token_symbol) {
        Some(p) => (p.price_usd, p.age_secs),
        None => (0.0, u64::MAX),
    };
    let oracle = OracleSnapshot {
        chainlink_price,
        pyth_price,
        twap_price,
        chainlink_age_s,
        pyth_age_s,
        twap_age_s,
    };

    // ── flashloan: live liquidity snapshot (unchanged from rev 1) ────────
    let (flashloan_available, flashloan_protocol_id) =
        match (input.flashloan_provider, input.flashloan_contract) {
            (Some(provider), Some(contract)) => {
                let snap = handles
                    .flashloan_registry
                    .snapshot(input.chain_id, provider, contract);
                let available: u128 = snap
                    .map(|s| s.available_wei.try_into().unwrap_or(u128::MAX))
                    .unwrap_or(0);
                (available, provider.as_str().to_string())
            }
            _ => (0, "none".to_string()),
        };
    let flashloan = FlashloanSnapshot {
        available: flashloan_available,
        protocol_id: flashloan_protocol_id,
    };

    // ── competition: real competition_probability, fails closed to 1.0 ───
    //
    // Unlike rev 1's inert 0.0 placeholder: `input.competition` missing
    // is now distinguished from "genuinely low competition." Defaulting
    // to 0.0 would fail OPEN (check 11 never fires); defaulting to 1.0
    // instead means check 11 fires whenever real inputs aren't supplied,
    // same fail-closed direction as risk_score below.
    let competition_probability_value = match &input.competition {
        Some(c) => {
            let tier = AssetTier::from_symbol(input.token_symbol);
            competition_probability(tier, c.health_factor, c.liquidation_eth)
        }
        None => 1.0,
    };

    // ── risk score: still no producer exists (unchanged from rev 1) ─────
    let risk_score = handles
        .risk
        .current_risk_score(input.strategy_id)
        .unwrap_or(f64::MAX);

    // ── nonce: fails closed by erroring (unchanged from rev 1) ───────────
    let scope = input.strategy_id;
    let latest_blueprint_nonce = handles
        .nonces
        .latest_nonce(scope)
        .map_err(|e| ExecutionError::NonceSourceUnavailable(e.to_string()))?;

    let current_account_exposure_wei = handles.exposure.current_exposure_wei(scope);

    let ctx = CheckContext {
        expected_chain_id: input.chain_id,
        current_block: input.current_block,
        current_l1_gas_price_gwei: l1_data_fee,
        current_l2_base_fee_gwei: l2_base_fee,
        l1_adaptive_buffer: input.limits.l1_adaptive_buffer,
        oracle,
        flashloan,
        competition_probability: competition_probability_value,
        max_competition_probability: input.limits.max_competition_probability,
        strategy_max_gas: input.limits.max_gas,
        max_slippage_bps: input.limits.max_slippage_bps,
        rollout_tier: input.limits.rollout_tier,
        strategy_bytecode_hash,
        risk_score,
        max_risk_score: input.limits.max_risk_score,
        current_account_exposure_wei,
        max_account_exposure_wei: input.limits.max_account_exposure_wei,
        latest_blueprint_nonce,
    };

    Ok((ctx, gas_readout))
}