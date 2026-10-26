// crates/omega-execution/src/context_assembly.rs
//
// Stage 2 CheckContext production assembly (ProductionIntegrationPlan.md C3).
//
// ## Revision 3 — fix: non-LA strategies were failing closed to competition=1.0
//
// Revision 2 implemented fail-closed competition (1.0 when
// `input.competition` is None) to distinguish "no inputs supplied" from
// "genuinely low competition." That was correct for LA blueprints: an LA
// blueprint without a target position is a construction error and should
// not pass check 11.
//
// BUT SA/MSA/MEV blueprints NEVER supply `CompetitionInputs` — they score
// pure DEX arbitrage, not target-position liquidations. There is no
// health-factor or per-position liquidation size. The rev 2 code therefore
// set `competition_probability_value = 1.0` for every single non-LA
// blueprint, causing check 11 to fire MISS_COMPETITION on 100 % of SA/MSA/
// MEV output. This was the primary cause of the "every blueprint dropped"
// symptom observed on mainnet at nonces 48610+.
//
// Fix: `AssemblyInput` now carries `strategy_class: StrategyClass` so
// `assemble_check_context` can route to the correct competition model:
//
//   - `StrategyClass::La`  → existing path: `competition_probability()` with
//                            LA-specific health-factor and liquidation-size
//                            inputs; fails closed to 1.0 when `input.competition`
//                            is absent (still correct — that IS a construction
//                            error for LA).
//
//   - `StrategyClass::NonLa(class)` → new path: `non_la_competition_probability()`
//                                     with the strategy's class and the MEV-Share
//                                     event count from the activity tracker.
//                                     No health-factor or liquidation-size needed.
//
// `StrategyClass::NonLa` paths NEVER fail closed to 1.0 — a missing MEV-Share
// count is benign (default 0 = quiet market = use the class's base rate), and
// there is no "construction error" analogue for a non-LA blueprint.
//
// ## Revision 2 — real sources confirmed, replacing rev 1's placeholders
//
// CONFIRMED against pasted source (rev 2):
//   - omega_oracle::chainlink::ChainlinkOracle::read(&self, token: &str)
//     -> Option<OraclePrice { price_usd, age_secs, .. }>
//   - omega_oracle::pyth::PythOracle::read(&self, token: &str)
//     -> Option<OraclePrice>, confidence-gated internally
//   - omega_oracle::twap::TwapOracle::read                          (rev 1)
//   - omega_oracle::per_chain::PerChainOracle::snapshot(&self)
//     -> Arc<EilSnapshot { fee: FeeSnapshot { base_fee_gwei,
//     l1_data_fee_gwei, priority_fee_gwei, block_number }, .. }>
//   - omega_rpc::client::OmegaRpcClient::fetch_fee_snapshot(&self)
//     -> anyhow::Result<FeeSnapshot>
//   - omega_risk::competition::competition_probability(...)           (rev 2)
//   - omega_risk::competition::non_la_competition_probability(...)    (rev 3, this)
//   - omega_flashloan::LiquidityRegistry::snapshot                   (rev 1)
//   - omega_security::IntegrityRegistry::snapshot / StrategyEntry     (rev 1)
//
// STILL NOT CONFIRMED (unchanged from rev 2):
//   - RiskScoreSource: no risk-score producer exists in any pasted file.
//     Fails closed to f64::MAX (check 12 always fails until wired).
//   - LatestNonceSource: omega_security::replay::NonceRegistry methods
//     never pasted. Fails closed by erroring, not defaulting to 0.
//   - AccountExposureSource: no exposure tracker in pasted code.
//
// ## Gas source selection (unchanged from rev 2)
//
// Uses `PerChainOracle::snapshot().fee` as primary (lock-free ArcSwap
// load, bounded by Arbitrum's ~250 ms block cadence), with a live
// `fetch_fee_snapshot()` fallback when the cache has never been populated.
//
// KNOWN DATA GAP (unchanged from rev 2): both gas sources hardcode
// `l1_data_fee_gwei` to `0` until ArbGasInfo precompile integration
// lands. `GasReadout::l1_data_fee_is_real` surfaces this for callers.

use std::sync::Arc;

use omega_flashloan::{FlashloanProvider, LiquidityRegistry};
use omega_oracle::chainlink::ChainlinkOracle;
use omega_oracle::per_chain::PerChainOracle;
use omega_oracle::pyth::PythOracle;
use omega_oracle::twap::TwapOracle;
use omega_risk::competition::{
    competition_probability, non_la_competition_probability, AssetTier, StrategyCompetitionClass,
};
use omega_risk::context::{CheckContext, FlashloanSnapshot, OracleSnapshot};
use omega_rpc::client::OmegaRpcClient;
use omega_security::IntegrityRegistry;

use crate::error::ExecutionError;

// ─── Still-unconfirmed subsystem seams (unchanged from rev 2) ────────────────

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

// ─── Strategy class routing for competition model ─────────────────────────────

/// Routes `assemble_check_context` to the correct competition model.
///
/// LA blueprints target a specific on-chain position and require real
/// health-factor and liquidation-size inputs. Non-LA strategies (SA, MSA,
/// MEV) score DEX arbitrage opportunities that have no target position —
/// they use `non_la_competition_probability()` with a strategy class
/// instead.
#[derive(Debug, Clone, Copy)]
pub enum StrategyClass {
    /// Liquidation Arbitrage — requires `CompetitionInputs` (health-factor,
    /// liquidation size). Fails closed to competition = 1.0 when inputs are
    /// absent, because an LA blueprint without a target is a construction
    /// error, not a "quiet market."
    La,
    /// SA/MSA/MEV — DEX arbitrage with no target position. Uses
    /// `non_la_competition_probability()`. A missing MEV-Share count is
    /// benign and defaults to 0 (quiet market), never to 1.0.
    NonLa(StrategyCompetitionClass),
}

// ─── Live handle bundle ───────────────────────────────────────────────────────

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

/// LA-specific target-position inputs for `competition_probability()`.
///
/// Only meaningful for LA blueprints. Present in `AssemblyInput` via
/// `input.competition: Option<CompetitionInputs>`. When `None` and
/// `strategy_class` is `La`, assembly fails closed (competition = 1.0).
/// For non-LA strategies the field is ignored entirely.
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
    pub flashloan_asset: Option<alloy_primitives::Address>,

    /// Routes the competition model. Set to `StrategyClass::La` for LA
    /// blueprints (which also need `competition` below); set to
    /// `StrategyClass::NonLa(class)` for SA/MSA/MEV.
    pub strategy_class: StrategyClass,

    /// LA-specific inputs. Ignored when `strategy_class` is `NonLa`.
    pub competition: Option<CompetitionInputs>,

    /// Count of competing MEV-Share bundles in the current activity window.
    /// Used by the non-LA path (`non_la_competition_probability`). `None`
    /// is treated as 0 (no observed competition in window = quiet market).
    pub mev_share_events_in_window: Option<u32>,
}

/// Reports which gas path was actually used and whether the L1 data fee
/// is real or the known-zero placeholder — see module doc comment's
/// "KNOWN DATA GAP" note.
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

// ─── The assembly function ────────────────────────────────────────────────────

pub async fn assemble_check_context(
    handles: &LiveContextHandles,
    input: &AssemblyInput<'_>,
) -> Result<(CheckContext, GasReadout), ExecutionError> {
    // ── strategy_bytecode_hash: from IntegrityRegistry ────────────────────
    let strategy_bytecode_hash = handles
        .integrity_registry
        .snapshot()
        .into_iter()
        .find(|e| e.strategy_id == input.strategy_id)
        .map(|e| e.bytecode_hash)
        .ok_or_else(|| ExecutionError::UnknownFlashloanProvider {
            address: format!("strategy_id={}", input.strategy_id),
        })?;

    // ── gas: PerChainOracle cache first, live RPC fallback ────────────────
    let cached_fee = handles.per_chain.snapshot().fee.clone();
    let (l2_base_fee, l1_data_fee, gas_source) = if cached_fee.block_number > 0 {
        (
            cached_fee.base_fee_gwei,
            cached_fee.l1_data_fee_gwei,
            GasSource::PerChainOracleCache,
        )
    } else {
        let live = handles
            .rpc
            .fetch_fee_snapshot()
            .await
            .map_err(|e| ExecutionError::GasSourceUnavailable(e.to_string()))?;
        (
            live.base_fee_gwei,
            live.l1_data_fee_gwei,
            GasSource::LiveRpcFallback,
        )
    };
    let gas_readout = GasReadout {
        source: gas_source,
        // Both gas sources hardcode l1_data_fee_gwei to 0 until ArbGasInfo
        // integration lands upstream. See module doc comment "KNOWN DATA GAP."
        l1_data_fee_is_real: false,
    };

    // ── oracle: real Chainlink + Pyth + TWAP caches ───────────────────────
    let (chainlink_price, chainlink_age_s) = match handles.chainlink.read(input.token_symbol) {
        Some(p) => (p.price_usd, p.age_secs),
        None => (0.0, u64::MAX), // non-sane + maximally stale → fails checks 7/16 cleanly
    };
    let (pyth_price, pyth_age_s) = match handles.pyth.read(input.token_symbol) {
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

    // ── flashloan: live liquidity snapshot ────────────────────────────────
    let (flashloan_available, flashloan_protocol_id) = match (
        input.flashloan_provider,
        input.flashloan_contract,
        input.flashloan_asset,
    ) {
        (Some(provider), Some(contract), Some(asset)) => {
            let snap =
                handles
                    .flashloan_registry
                    .snapshot(input.chain_id, provider, asset, contract);
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

    // ── competition: strategy-class-aware routing ─────────────────────────
    //
    // LA blueprints: use the position-specific competition model. Absent
    // CompetitionInputs for an LA blueprint is a construction error → fail
    // closed to 1.0 (check 11 will drop it, which is correct behaviour).
    //
    // Non-LA blueprints (SA/MSA/MEV): use the class-based model that requires
    // only the strategy class and optional MEV-Share activity count. These
    // paths NEVER return 1.0 for a missing optional input — a quiet MEV-Share
    // window genuinely means lower competition, not a construction error.
    let competition_probability_value = match input.strategy_class {
        StrategyClass::La => match &input.competition {
            Some(c) => {
                let tier = AssetTier::from_symbol(input.token_symbol);
                competition_probability(tier, c.health_factor, c.liquidation_eth)
            }
            None => {
                // LA blueprint with no target position: construction error.
                // Fail closed — check 11 will drop this with MISS_COMPETITION,
                // which is the correct outcome.
                1.0
            }
        },
        StrategyClass::NonLa(class) => {
            // `mev_share_events_in_window` absent = quiet window = 0 events.
            // This is not a construction error; it just means no competing
            // bundles were observed. Never defaults to 1.0.
            let events = input.mev_share_events_in_window.unwrap_or(0);
            non_la_competition_probability(class, events)
        }
    };

    // ── risk score: still no producer exists ──────────────────────────────
    let risk_score = handles
        .risk
        .current_risk_score(input.strategy_id)
        .unwrap_or(f64::MAX);

    // ── nonce: fails closed by erroring ───────────────────────────────────
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
