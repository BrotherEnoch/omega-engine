// src/main.rs — OmegaEngine v12.0 Main Entry Point
//
// Required: ARBITRUM_RPC_URL (WebSocket endpoint)
// Optional: OMEGA_CONFIG (default: config/default.toml)
//
// [ ... all prior revisions' doc comments unchanged above this point;
//   omitted here for brevity — see version control history for the full
//   chain of fixes (HotPathRequest oracle field, real oracle feed wiring,
//   DAG mutex type, connect_with_retry error propagation, PerChainOracle
//   argument count, omega-risk Cargo dependency, clippy fixes) ... ]
//
// ## C1 (this revision): Integration wiring foundation
//
// Wires `omega_execution::ExecutionPipeline` construction into `main()`,
// per ProductionIntegrationPlan.md's C1 task. This is the "make the types
// constructible and the binary compile" slice of Gap 8 — it does NOT
// replace the two `tracing::info!("blueprint ready"/"proof ready")` call
// sites in `score_and_admit` with a real `pipeline.execute(...)` call
// (that is the remainder of Gap 8, left for later, since it needs a real
// `CheckContext` this file has no oracle/flashloan/competition/risk-score
// sources to build honestly yet).
//
// Every object below is a verified-real constructor call against the
// actual source of omega-execution, omega-relay, omega-security, and
// omega-risk::kill_switch (crates/omega-execution/src/{lib,pipeline}.rs,
// crates/omega-relay/src/{lib,config,blacklist,metrics}.rs,
// crates/omega-security/src/{lib,integrity}.rs,
// crates/omega-risk/src/kill_switch.rs — all read directly in this
// revision, not assumed), with ONE exception flagged explicitly below
// (`UnconfiguredSigner`'s exact shape) since `omega-security`'s/
// `omega-execution`'s own `signer.rs` file itself was not read in this
// investigation, only `lib.rs`'s re-export of it.
//
// Fail-closed stand-ins used here, per C1's explicit rule ("no invented
// production values"):
//   - `KillSwitchRegistry`: real struct, non-production threshold
//     numbers (`KillSwitchConfig::validate()` requires every field > 0,
//     so they can't be zero — but they carry no claim to be real risk
//     policy; see the warning logged at construction). Gap 5 replaces
//     these.
//   - `IntegrityRegistry`: real, empty. No strategy is registered, so
//     `full_integrity_check` will reject every strategy_id with
//     `SecurityError::StrategyUnknown` until Gap 6 (a real
//     `DeploymentManifest` -> `strategy_entries_from_manifest` ->
//     `register_all`) is wired in — the whole point: the check runs for
//     real, but nothing is authorized yet.
//   - `MultiRelayClient`: real, with a ZERO-entry `relay_clients` map.
//     Per omega-relay's own `backpressure.rs` behavior (confirmed by
//     `omega-execution/src/pipeline.rs`'s own test-suite audit notes),
//     `LaRelayMetrics` starts with an empty ranking regardless of the
//     (empty) client map, so every submission attempt fails closed
//     inside omega-relay before any network call could occur — Gaps 2-4
//     replace this with real relay clients + secrets + config
//     translation.
//   - `UnconfiguredSigner`: the real fail-closed signer type
//     `omega_execution::lib.rs` names explicitly for exactly this
//     purpose ("no implementation of TransactionSigner exists anywhere
//     in the workspace... ExecutionPipeline is generic over S so it can
//     be built and tested today without fabricating one"). Constructed
//     here as a bare unit value (`Arc::new(UnconfiguredSigner)`) — this
//     revision's `cargo check -p omega-execution`, `cargo check
//     -p omega-engine`, `cargo check --workspace`, and `cargo test
//     -p omega-execution` (66/66 passing) all ran clean against this
//     exact construction, so this is now a compiler-verified fact, not
//     an inference from naming patterns.
//
// `BuilderBlacklist::load` requires an existing, parseable TOML file —
// it has no in-memory/empty-without-a-file constructor. An empty
// blacklist (the `[[blacklisted_builders]]` table simply absent, per
// `blacklist.rs`'s own `empty_blacklist_is_valid` test) is a legitimate
// empty state, not fabricated data, so this revision writes one at the
// conventional `config/builder_blacklist.toml` path (matching
// `GasWarRelayConfig::default()`'s own path in omega-relay's config.rs)
// if it doesn't already exist, rather than inventing blacklist entries.
//
// The `LaReorgRiskEvent` receiver `MultiRelayClient::new` now returns
// (per omega-relay's own "Audit fix: reorg-risk events were silently
// discarded" note) is owned here via a minimal drain-and-log task —
// exactly the "at least hold/drain" treatment C1's own anti-pattern
// table calls for, not a full consumer (that's Gap 12).

// ## C2 (this revision): DAG/execution ownership
//
// Wires `ExecutionPipeline::execute()` into `score_and_admit`, and
// removes that function's own `dag.complete()` call — DAG slot release
// is now owned exclusively by `execute()`'s internal `DagSlotGuard`
// (crates/omega-execution/src/pipeline.rs), which releases the slot on
// every exit path (success, every Stage 0-5 error, and panic-via-Drop)
// exactly once. `score_and_admit` calling `dag.complete()` on top of
// that would be a second, redundant release site — `ExecutionDag::
// complete()` tolerates it numerically (`saturating_sub`, see
// omega-execution's own `property_dag_occupancy_never_goes_negative_
// on_over_release` test), but two call sites racing to release the same
// slot is not "exactly-once ownership," it's ownership that happens to
// not crash. C2 makes `execute()` the single, sole releaser.
//
// This is NOT the same as Gap 8 being complete. `ExecutionPipeline::
// execute()` needs a real `omega_risk::context::CheckContext` — of that
// struct's 17 fields (crates/omega-risk/src/context.rs, read directly
// this revision), exactly four have a genuine live data source anywhere
// in this codebase: `expected_chain_id`, `current_block`,
// `current_l2_base_fee_gwei`, and `oracle` (the same real
// `OracleSnapshot` already built for the hot-path lane). Every other
// field — competition probability, risk score, per-account exposure,
// per-account nonce, flashloan liquidity, l1_adaptive_buffer, rollout
// tier, a second bytecode-whitelist hash distinct from
// `IntegrityRegistry`'s real Stage 2b check — has NO live source
// anywhere in this codebase as of this revision. `build_check_context`
// below sets each such field to a value chosen specifically to make its
// corresponding check in `checks.rs` fail closed, not a guess at a
// plausible real value. See that function's own doc comment for the
// per-field reasoning, including the one field (`strategy_max_gas`)
// where the naive "unknown -> 0" default would have silently DISABLED
// its check instead of failing it closed.
//
// Practical consequence: with `active_phase >= 1`, every real blueprint
// is rejected deterministically at check 3 (`MissGas`) before any later
// check — including the two that DO have real oracle data behind them
// (7/8/16) — is ever reached. That's intentional: one legible,
// always-hit failure point is more auditable than several
// partially-real, partially-fabricated checks that might pass by
// accident. With the default `active_phase == 0`, none of this is
// exercised at all — Stage 0 suppresses `execute()` before any
// `CheckContext` field is read.
//
// OPEN ARCHITECTURAL QUESTION, NOT RESOLVED HERE: `omega-execution/src/
// pipeline.rs`'s own module doc comment lists, as its open question 5,
// "hot-path <-> relay spec/code mismatch: NOT resolved here... left
// open for a separate decision" — i.e. whether `execute()` is meant to
// replace `score_and_admit`'s existing hot-path/ZK-proof dispatch, run
// conditionally on that dispatch's success, or run unconditionally
// alongside it. This revision takes the most conservative reading:
// `execute()` is called unconditionally, AFTER the existing dispatch,
// without gating on or removing any of that already-tested behavior.
// That is a deliberate choice to not resolve an open question this
// investigation has no authority over, not a claim that it's the
// architecturally correct final answer. If the intended relationship is
// different, this is the one call site to revisit.
//
// `current_block_timestamp_secs` (execute()'s fourth positional arg) has
// no real chain-timestamp source wired into this file either —
// wall-clock time is used as the closest available proxy. In practice
// this is inert today, same reasoning as above: the deliberately
// fail-closed `CheckContext` means Stage 2c always rejects before Stage
// 4 (where this timestamp is actually consumed, per pipeline.rs) is
// ever reached.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::Level;

// LayerHealth trait must be in scope for .state(), .layer_id(), .set_state()
// to resolve on Arc<LayerHealthImpl>
use omega_core::{HealthState, LayerHealth, LayerId, OmegaConfig, StrategyId};
use omega_dag::{DagConfig, ExecutionDag};
// C1: ExecutionPipeline + UnconfiguredSigner — see this file's module-level
// "C1" doc comment for the UnconfiguredSigner caveat specifically.
use omega_execution::{ExecutionPipeline, UnconfiguredSigner};
use omega_health::{halt::HaltFlag, LayerHealthImpl};
use omega_hot_path::{HotPathConfig, HotPathRequest, HotPathRunner, MICROTX_GAS_LIMIT};
use omega_observability::{
    EventRingBuffer, ExporterConfig, OmegaExporter, Sampler, DEFAULT_CAPACITY,
};
// ChainlinkOracle/PythOracle/TwapOracle: real feed caches, added this
// revision to fix build_oracle_snapshot() — see that function's own
// doc comment and the module-level "Fix: real oracle feed wiring" note.
use omega_oracle::{ChainlinkOracle, PerChainOracle, PythOracle, TwapOracle};
// C1: MultiRelayClient + friends — real constructor signatures confirmed
// directly against crates/omega-relay/src/{lib,config,blacklist,metrics}.rs
// in this revision (see this file's module-level "C1" doc comment).
use omega_relay::{
    BuilderBlacklist, ExecutionAddress, LaRelayMetrics, MultiRelayClient, RelayClient, RelayConfig,
};
// C2: CheckContext + FlashloanSnapshot — confirmed directly against
// crates/omega-risk/src/context.rs in this revision (see this file's
// module-level "C2" doc comment).
use omega_risk::context::{CheckContext, FlashloanSnapshot, OracleSnapshot};
// C1: KillSwitchConfig/KillSwitchRegistry — confirmed directly against
// crates/omega-risk/src/kill_switch.rs in this revision.
use omega_risk::kill_switch::{KillSwitchConfig, KillSwitchRegistry};
use omega_rpc::{
    rate_limiter::RpcRateLimiter, run_dex_sync_stream, run_fee_oracle_stream,
    run_lending_protocol_stream, run_mev_share_stream, run_pending_tx_stream, OmegaRpcClient,
    RpcClientConfig,
};
// C1: IntegrityRegistry — confirmed directly against
// crates/omega-security/src/{lib,integrity}.rs in this revision.
use omega_security::IntegrityRegistry;
use omega_strategies::{registry::StrategyRegistryBuilder, CnryStrategy, StrategyRegistry};
use omega_zk::{config::ProverTierConfig, ProofQueue, ProofWorkerPool, ZkConfig};
// C1: only used for the relay_clients: HashMap<String, Arc<dyn RelayClient>>
// type annotation below — the map itself is intentionally empty (Gap 2-4).
use std::collections::HashMap;

const CHAIN_ID: u64 = 42_161;
const DEFAULT_RPS: u32 = 500;
const SHUTDOWN_DRAIN_S: u64 = 5;
const DEFAULT_CONFIG: &str = "config/default.toml";

// C1: conventional builder-blacklist path — matches
// omega_relay::config::GasWarRelayConfig::default()'s own
// "config/builder_blacklist.toml" so a real config, once wired in later,
// points at the same file this revision may create as empty.
const BUILDER_BLACKLIST_PATH: &str = "config/builder_blacklist.toml";

// ── Config ────────────────────────────────────────────────────────────────────

fn load_config(path: &str) -> Result<OmegaConfig> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        tracing::warn!(path, "Config not found — using defaults");
        return Ok(OmegaConfig::default());
    }
    let s = std::fs::read_to_string(p).with_context(|| format!("reading {path}"))?;
    let cfg: OmegaConfig = toml::from_str(&s).with_context(|| format!("parsing {path}"))?;
    let errs = cfg.validate();
    if !errs.is_empty() {
        anyhow::bail!("Config errors:\n{}", errs.join("\n"));
    }
    Ok(cfg)
}

// ── Health layers ─────────────────────────────────────────────────────────────

// new_bare() returns Arc<Self> directly — do NOT wrap in Arc::new again
fn make_layers() -> [Arc<LayerHealthImpl>; 16] {
    [
        LayerId::Health,
        LayerId::Rpc,
        LayerId::Oracle,
        LayerId::Security,
        LayerId::Compliance,
        LayerId::Risk,
        LayerId::Dag,
        LayerId::Zk,
        LayerId::FlashLoan,
        LayerId::Relay,
        LayerId::GasWar,
        LayerId::LossAttribution,
        LayerId::AddressRotation,
        LayerId::Strategies,
        LayerId::HotPath,
        LayerId::Observability,
    ]
    .map(LayerHealthImpl::new_bare)
}

fn find_layer(layers: &[Arc<LayerHealthImpl>; 16], id: LayerId) -> Arc<LayerHealthImpl> {
    layers
        .iter()
        .find(|l| l.layer_id() == id)
        .cloned()
        .unwrap_or_else(|| panic!("layer {id:?} not found"))
}

fn as_health(h: Arc<LayerHealthImpl>) -> Arc<dyn LayerHealth> {
    h as Arc<dyn LayerHealth>
}

/// GAP (labelled placeholder, not a verified product decision): the
/// token symbol a per-cycle `OracleSnapshot` represents. `OracleSnapshot`
/// has no token field — it's a single flat six-scalar snapshot, not
/// keyed per asset — and nothing in this codebase specifies which real
/// symbol that should be. "WETH" is a guess at the most likely base
/// asset, not a confirmed value. Cross-check against the actual entries
/// in `chainlink::arbitrum_feeds()` / `pyth::arbitrum_price_ids()` /
/// `twap::arbitrum_pools()` (their bodies haven't been pasted in this
/// session) before treating this as correct, and reconsider whether a
/// single shared token is even the right model once real per-blueprint
/// asset requirements are decided.
const ORACLE_SNAPSHOT_TOKEN: &str = "WETH";

/// Builds a live `OracleSnapshot` for the pre-trade risk checks
/// (`omega_risk::checks`) and the hot-path lane (`omega_hot_path`) from
/// the three real feed caches.
///
/// Reads each cache's real `read(&self, token: &str) -> Option<OraclePrice>`
/// directly — no `resolution::resolve_price()` call here, since that
/// function produces a single resolved price, not the six raw per-source
/// scalars `OracleSnapshot` actually holds (confirmed against both
/// structs' real field lists).
///
/// GAP, deliberately not papered over: nothing in this codebase calls
/// `.update()` on any of these three caches (no ingestion path exists
/// yet — standing-queue item 2a, partially closed for Chainlink this
/// revision via the L2c poll loop below). `read()` will therefore return
/// `None` for pyth/twap (and chainlink, until its poll loop's first
/// successful cycle) until their respective ingestion paths run. A
/// `None` is mapped to `age_secs = u64::MAX` / `price = 0.0` here —
/// "infinitely stale, zero price" — rather than fabricating a plausible
/// number, so that whatever staleness check consumes this snapshot
/// downstream fails closed correctly instead of silently passing on
/// absent data.
fn build_oracle_snapshot(
    chainlink: &ChainlinkOracle,
    pyth: &PythOracle,
    twap: &TwapOracle,
    token: &str,
) -> OracleSnapshot {
    let (chainlink_price, chainlink_age_s) = match chainlink.read(token) {
        Some(p) => (p.price_usd, p.age_secs),
        None => (0.0, u64::MAX),
    };
    let (pyth_price, pyth_age_s) = match pyth.read(token) {
        Some(p) => (p.price_usd, p.age_secs),
        None => (0.0, u64::MAX),
    };
    let (twap_price, twap_age_s) = match twap.read(token) {
        Some(p) => (p.price_usd, p.age_secs),
        None => (0.0, u64::MAX),
    };

    OracleSnapshot {
        chainlink_price,
        pyth_price,
        twap_price,
        chainlink_age_s,
        pyth_age_s,
        twap_age_s,
    }
}

/// Builds the `CheckContext` passed to `ExecutionPipeline::execute`'s
/// Stage 2c (15 pre-trade checks). See this file's module-level "C2" doc
/// comment for the overall status; this function's job is narrower: for
/// each of `CheckContext`'s 17 fields, either use a genuinely real, live
/// value, or set a value chosen specifically to make that field's
/// corresponding check in `checks.rs` fail closed.
fn build_check_context(sig: &omega_core::SignalState, oracle_snapshot: OracleSnapshot) -> CheckContext {
    CheckContext {
        // ── Real, live data ──────────────────────────────────────────
        expected_chain_id: CHAIN_ID,
        current_block: sig.block_number,
        current_l2_base_fee_gwei: sig.base_fee_gwei,
        oracle: oracle_snapshot,

        // ── GAP: no live source anywhere in this codebase — every ────
        // ── field below is a deliberate fail-closed placeholder,    ──
        // ── not a guess at a plausible real value.                  ──

        // No l1_adaptive_buffer computation exists (spec S7). Not read
        // by any check in checks.rs directly (check 5's own comparison
        // doesn't touch ctx at all), so there's no "direction" to fail
        // closed against — 0.0 is the least-committal placeholder.
        l1_adaptive_buffer: 0.0,

        // No L1 gas price feed distinct from the fee-oracle stream
        // already wired into `sig`/`oracle` exists. u64::MAX guarantees
        // check 6 (gas spike) fails closed:
        // `diff.saturating_mul(DEN) > at_creation.saturating_mul(NUM)`
        // for any realistic `l1_data_fee_at_creation`.
        current_l1_gas_price_gwei: u64::MAX,

        // No real flashloan-liquidity feed exists. `available: 0`
        // guarantees check 10 fails closed — `available >= amount *
        // 1.20` can never hold for any nonzero flashloan_amount.
        flashloan: FlashloanSnapshot {
            available: 0,
            protocol_id: String::new(),
        },

        // No competition-probability model exists (spec: computed by
        // omega-risk::competition, not part of this investigation).
        // 1.0 vs. max 0.0 guarantees check 11 fails closed regardless
        // of what a real model would report.
        competition_probability: 1.0,
        max_competition_probability: 0.0,

        // No per-strategy gas-budget config exists. NOTE: 0 here would
        // mean "unlimited" per check_gas_budget's own
        // `if ctx.strategy_max_gas > 0 && ...` guard — the one field on
        // this struct where a naive "unknown -> 0" default would
        // silently DISABLE its check instead of failing it closed. Set
        // to 1 instead: any nonzero total gas estimate (every real
        // blueprint) fails MissGas.
        strategy_max_gas: 1,

        // context.rs defines real per-strategy-class slippage constants
        // (MAX_SLIPPAGE_BPS_SA/MSA/LA/MEV), but mapping bp.strategy_id
        // to the correct one requires matching against
        // omega_core::types::blueprint::StrategyId's exact variants —
        // not independently verified here against the (possibly
        // identical, possibly distinct) top-level omega_core::StrategyId
        // already imported into this file. Rather than guess a match
        // arm against an unconfirmed type, 0 is used: fails closed for
        // any slippage_bps > 0, which is virtually every real
        // blueprint, without needing to resolve that ambiguity here.
        max_slippage_bps: 0,

        // No rollout-tier config exists (spec S19), and no check in
        // checks.rs reads `ctx.rollout_tier` as of this revision —
        // carried on CheckContext for a consumer outside the 15-check
        // pipeline this investigation has no visibility into. 0.0 is
        // the least-committal placeholder, not a fail-closed choice,
        // since there is no check to close against.
        rollout_tier: 0.0,

        // A SEPARATE bytecode-hash mechanism from IntegrityRegistry's
        // real, per-strategy Stage 2b check (already wired in C1) — see
        // this file's module-level "C2" doc comment. Nothing supplies a
        // real expected hash for this second mechanism. [0u8; 32]
        // matches this codebase's own established convention
        // (omega-security's parse_bytecode_hash/parse_contract_address)
        // that an all-zero hash is unambiguous placeholder data,
        // guaranteed to mismatch any real bp.strategy_bytecode_hash and
        // fail MissWhitelist.
        strategy_bytecode_hash: [0u8; 32],

        // No composite risk-scoring model exists (spec: "incorporates
        // gas volatility, oracle freshness, competition, liquidity
        // depth" — none of those four inputs has a real source here
        // either). 1.0 vs. max 0.0 guarantees check 12 fails closed.
        risk_score: 1.0,
        max_risk_score: 0.0,

        // No per-account exposure tracker exists — this is meant to be
        // live state a caller advances across scoring cycles, and
        // nothing here currently persists it. u128::MAX vs. max 0
        // guarantees check 14 fails closed.
        current_account_exposure_wei: u128::MAX,
        max_account_exposure_wei: 0,

        // No per-account nonce tracker exists either, same reasoning as
        // exposure above. u64::MAX guarantees check 15 fails closed: no
        // real bp.nonce (which starts low and increments) will ever be
        // strictly greater than u64::MAX.
        latest_blueprint_nonce: u64::MAX,
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(true)
        .json()
        .init();

    tracing::info!("OmegaEngine v12.0 starting");

    let rpc_url = std::env::var("ARBITRUM_RPC_URL").context("ARBITRUM_RPC_URL must be set")?;
    let config_path = std::env::var("OMEGA_CONFIG").unwrap_or_else(|_| DEFAULT_CONFIG.to_string());

    let config = load_config(&config_path)?;
    let active_phase = config.active_phase;

    tracing::info!(active_phase, chain_id = CHAIN_ID, "Config loaded");
    if active_phase == 0 {
        tracing::info!("Phase 0: shadow mode — relay submission suppressed");
    }

    // ── L0: HaltFlag + 16 health layers ──────────────────────────────────────
    let halt = HaltFlag::new();
    let layers = make_layers();

    // ── L15: Observability ────────────────────────────────────────────────────
    let obs_buffer = EventRingBuffer::new(DEFAULT_CAPACITY);
    let (sd_tx, sd_rx) = tokio::sync::watch::channel(false);
    {
        let buf = obs_buffer.clone();
        let sampler = Sampler::new(1.0);
        let cfg = ExporterConfig::default();
        tokio::spawn(async move {
            OmegaExporter::run(buf, sampler, cfg, sd_rx).await;
        });
    }
    tracing::info!("L15 observability running");

    // ── L1: RPC client ────────────────────────────────────────────────────────
    //
    // FIX (this revision): connect_with_retry(..).await returns
    // Result<OmegaRpcClient, RpcClientError>, not OmegaRpcClient directly
    // — confirmed by the compiler (see this file's module-level doc
    // comment). `.with_health(...)` lives on OmegaRpcClient itself, so
    // the connection error must be propagated with `?` first.
    let rpc = OmegaRpcClient::connect_with_retry(RpcClientConfig::new(
        &rpc_url,
        DEFAULT_RPS,
        CHAIN_ID,
    ))
    .await
    .context("connecting to Arbitrum RPC endpoint")?
    .with_health(as_health(find_layer(&layers, LayerId::Rpc)));

    {
        let r = rpc.clone();
        tokio::spawn(async move { r.run_block_subscription().await });
    }

    // Extract ws_url and rate_limiter for subscription fns
    // Subscription fns take (ws_url, chain_id, limiter, tx) directly
    let ws_url = rpc_url.clone();
    let limiter = Arc::new(RpcRateLimiter::new());

    let (fee_tx, fee_rx) = tokio::sync::broadcast::channel(256);
    let (dex_tx, dex_rx) = tokio::sync::broadcast::channel(1024);
    let (lend_tx, lend_rx) = tokio::sync::broadcast::channel(512);
    let (ptx_tx, _ptx_rx) = tokio::sync::broadcast::channel(512);
    let (mev_tx, _mev_rx) = tokio::sync::broadcast::channel(256);

    {
        let u = ws_url.clone();
        let l = Arc::clone(&limiter);
        let t = ptx_tx.clone();
        tokio::spawn(async move { run_pending_tx_stream(u, CHAIN_ID, l, t).await });
    }
    {
        let u = ws_url.clone();
        let l = Arc::clone(&limiter);
        let t = fee_tx.clone();
        tokio::spawn(async move { run_fee_oracle_stream(u, CHAIN_ID, l, t).await });
    }
    {
        let u = ws_url.clone();
        let l = Arc::clone(&limiter);
        let t = dex_tx.clone();
        tokio::spawn(async move { run_dex_sync_stream(u, CHAIN_ID, l, t).await });
    }
    {
        let u = ws_url.clone();
        let l = Arc::clone(&limiter);
        let t = lend_tx.clone();
        tokio::spawn(async move { run_lending_protocol_stream(u, CHAIN_ID, l, t).await });
    }
    {
        let t = mev_tx.clone();
        tokio::spawn(async move { run_mev_share_stream(t).await });
    }

    tracing::info!("L1 RPC: 5 subscription streams running");

    // ── L2: Oracle ────────────────────────────────────────────────────────────
    //
    // twap_oracle/chainlink_oracle/pyth_oracle are constructed here
    // independently and held so build_oracle_snapshot() can read them.
    //
    // FIX (this revision): PerChainOracle::new takes ONLY chain_id — see
    // this file's module-level "Fix: PerChainOracle::new argument count"
    // doc comment for why the previous two-argument call
    // (`PerChainOracle::new(CHAIN_ID, Arc::clone(&twap_oracle))`) was
    // wrong, and for the still-open question of whether PerChainOracle
    // owns its own separate internal TwapOracle instance that this
    // standalone `twap_oracle` might not actually be wired to.
    let twap_oracle = TwapOracle::new(CHAIN_ID);
    let chainlink_oracle = ChainlinkOracle::new(CHAIN_ID);
    let pyth_oracle = PythOracle::new(CHAIN_ID);

    let oracle =
        PerChainOracle::new(CHAIN_ID).with_health(as_health(find_layer(&layers, LayerId::Oracle)));

    {
        let o = Arc::clone(&oracle);
        tokio::spawn(async move { o.run_fee_oracle(fee_rx).await });
    }
    {
        let o = Arc::clone(&oracle);
        tokio::spawn(async move { o.run_dex_sync(dex_rx).await });
    }
    {
        let o = Arc::clone(&oracle);
        tokio::spawn(async move { o.run_lending_protocol(lend_rx).await });
    }

    tracing::info!("L2 oracle: 3 update streams running");

    // ── L2c: Chainlink polling (this revision) ────────────────────────────────
    //
    // Real ingestion for the Chainlink leg — closes that half of the
    // standing-queue item 2a gap. Uses the already-connected `rpc`
    // client (constructed above for the block/fee/dex/lending streams)
    // rather than opening a second connection.
    match omega_oracle::chainlink_poll::parse_arbitrum_chainlink_feeds() {
        Ok(feeds) => {
            let cl_client = rpc.clone();
            let cl_oracle = Arc::clone(&chainlink_oracle);
            tokio::spawn(async move {
                omega_oracle::chainlink_poll::run_chainlink_poll_loop(
                    cl_client,
                    cl_oracle,
                    feeds,
                    Duration::from_secs(15),
                )
                .await;
            });
            tracing::info!("L2c Chainlink poll loop started");
        }
        Err(e) => {
            // Same treatment as twap.rs's malformed-LINK-address defect:
            // a bad feed table is a real data problem, not something to
            // paper over by silently skipping Chainlink ingestion.
            tracing::error!(
                error = %e,
                "Chainlink feed table malformed — Chainlink ingestion NOT started"
            );
        }
    }

    // GAP still open: Pyth has no ingestion path — only Chainlink (via
    // the poll loop above) and, POSSIBLY, TWAP (via run_dex_sync — see
    // this file's module-level "Fix: PerChainOracle::new argument count"
    // note for why the TWAP leg's actual wiring to THIS standalone
    // twap_oracle is unconfirmed, not just "started") receive real
    // .update() calls as of this revision.
    tracing::warn!(
        "Pyth cache constructed but UNFED — no ingestion path exists yet. \
         Chainlink now receives real updates via the poll loop above; \
         whether TWAP updates reach THIS process's standalone twap_oracle \
         (as opposed to a possibly-separate instance PerChainOracle owns \
         internally) is unconfirmed — see this file's PerChainOracle::new \
         doc comment."
    );

    // ── L6: DAG ───────────────────────────────────────────────────────────────
    let dag = Arc::new(Mutex::new(ExecutionDag::new(DagConfig {
        microtx_slots: 16,
        normal_slots: 4,
        eviction_log_capacity: 1_000,
    })));
    tracing::info!("L6 DAG initialised");

    // ── C1: ExecutionPipeline construction (fail-closed) ─────────────────────
    //
    // See this file's module-level "C1" doc comment for the full
    // rationale and the one unconfirmed item (UnconfiguredSigner's exact
    // shape). Placed here (after DAG, before ZK) per
    // ProductionIntegrationPlan.md's C1 task ordering.

    // C1: KillSwitchRegistry — non-production placeholder thresholds.
    // KillSwitchConfig::validate() requires every field > 0; these are
    // deliberately large/permissive, NOT a risk policy decision. Gap 5
    // replaces these with real operator-approved numbers.
    let kill_switch_cfg = KillSwitchConfig {
        max_cumulative_loss_wei: u128::MAX / 4,
        max_loss_per_window_wei: u128::MAX / 8,
        loss_window: Duration::from_secs(3600),
        max_consecutive_failures: 32,
    };
    // KillSwitchRegistry::new returns Self (it's internally Arc<DashMap>-
    // backed and #[derive(Clone)]), not Arc<Self> — confirmed directly
    // against kill_switch.rs. ExecutionPipeline::new wants an
    // Arc<KillSwitchRegistry> (matching omega-execution's own test helper
    // `make_kill_switches`), so it's wrapped explicitly here.
    let kill_switches =
        Arc::new(KillSwitchRegistry::new(kill_switch_cfg).context("KillSwitchRegistry::new")?);
    tracing::warn!(
        "C1: KillSwitchRegistry constructed with non-production placeholder thresholds (Gap 5)"
    );

    // C1: empty IntegrityRegistry — IntegrityRegistry::new() already
    // returns Arc<Self> (confirmed against integrity.rs), unlike
    // KillSwitchRegistry above. No strategy is registered, so
    // full_integrity_check will reject every strategy_id with
    // SecurityError::StrategyUnknown until Gap 6 (a real
    // DeploymentManifest -> strategy_entries_from_manifest -> register_all)
    // is wired in.
    let integrity_registry = IntegrityRegistry::new();
    tracing::warn!("C1: IntegrityRegistry empty — no deployment manifest loaded (Gap 6)");

    // C1: MultiRelayClient with ZERO live RelayClient implementations.
    // Per omega-relay's own backpressure.rs behavior (confirmed via
    // omega-execution/src/pipeline.rs's own test-suite audit notes),
    // LaRelayMetrics starts with an empty ranking regardless of the
    // (empty) client map, so every submission attempt fails closed
    // inside omega-relay before any network call could occur. Gaps 2-4
    // replace this with real relay clients + secrets + config translation.
    let relay_clients: HashMap<String, Arc<dyn RelayClient>> = HashMap::new();
    let relay_metrics = LaRelayMetrics::new(50, ExecutionAddress("0xC1_UNCONFIGURED".into()));

    // BuilderBlacklist::load requires an existing, parseable TOML file —
    // no in-memory/empty-without-a-file constructor exists. An empty
    // blacklist (the [[blacklisted_builders]] table simply absent, per
    // blacklist.rs's own empty_blacklist_is_valid test) is a legitimate
    // empty state, not fabricated data — write one at the conventional
    // path (matching GasWarRelayConfig::default()'s own path) if it
    // doesn't already exist, rather than inventing blacklist entries.
    if !std::path::Path::new(BUILDER_BLACKLIST_PATH).exists() {
        if let Some(parent) = std::path::Path::new(BUILDER_BLACKLIST_PATH).parent() {
            std::fs::create_dir_all(parent).context("creating config/ directory")?;
        }
        std::fs::write(
            BUILDER_BLACKLIST_PATH,
            "# C1: empty builder blacklist — no entries registered yet\n",
        )
        .context("writing empty builder blacklist")?;
        tracing::warn!(
            path = BUILDER_BLACKLIST_PATH,
            "C1: created empty builder blacklist file (none existed)"
        );
    }
    let blacklist =
        BuilderBlacklist::load(BUILDER_BLACKLIST_PATH).context("BuilderBlacklist::load")?;

    let relay_cfg = RelayConfig {
        confirmation_rpc_url: "http://127.0.0.1:1".to_string(), // unreachable; fails closed
        ..Default::default()
    };
    let (relay, reorg_event_rx) =
        MultiRelayClient::new(relay_clients, relay_metrics, blacklist, &relay_cfg, 0);

    // C1: own the LaReorgRiskEvent receiver so it's not silently dropped —
    // see omega-relay's own "Audit fix: reorg-risk events were silently
    // discarded" note on MultiRelayClient::new. Minimal drain-and-log
    // task; a real consumer (rescoring on reorg risk) is Gap 12.
    tokio::spawn(async move {
        let mut rx = reorg_event_rx;
        while let Some(ev) = rx.recv().await {
            tracing::debug!(?ev, "C1: LaReorgRiskEvent (unhandled beyond log)");
        }
    });
    tracing::warn!("C1: MultiRelayClient has zero relay clients — submissions will fail closed");

    // C1: fail-closed signer — compiler-verified against the real
    // workspace (cargo check -p omega-execution / -p omega-engine /
    // --workspace, plus cargo test -p omega-execution, all clean). See
    // this file's module-level "C1" doc comment.
    let signer = Arc::new(UnconfiguredSigner);

    let execution_pipeline = Arc::new(ExecutionPipeline::new(
        Arc::clone(&kill_switches),
        Arc::clone(&integrity_registry),
        Arc::clone(&relay),
        Arc::clone(&dag),
        Arc::clone(&signer),
        CHAIN_ID,
    ));
    // Real, meaningful use of the constructed pipeline (not a
    // fabricated call) — proves the object is live and reachable, and
    // avoids an "unused variable" warning standing in for actually
    // exercising it. score_and_admit does not call execute() yet (that
    // remains Gap 8's remaining half — see this file's module-level "C1"
    // doc comment).
    tracing::info!(
        idempotency_cache_len = execution_pipeline.idempotency_cache_len(),
        "C1: ExecutionPipeline constructed (fail-closed signer + empty relays)"
    );

    // ── L7: ZK ────────────────────────────────────────────────────────────────
    let zk_cfg = ZkConfig {
        prover_tier: ProverTierConfig::T1Software,
        worker_count: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(8),
        microtx_sla_ms: 1_200,
        normal_sla_ms: 4_000,
        proof_queue_throttle: 128,
        proof_queue_suspend: 256,
        proof_queue_halt: 512,
        allow_skip_in_shadow: active_phase == 0,
        checkpoint_dir: config.ml.checkpoint_dir.clone(),
        max_checkpoints: config.ml.checkpoint_retention,
    };
    let proof_queue = ProofQueue::new(zk_cfg.clone());
    let _pool = ProofWorkerPool::start(zk_cfg, proof_queue.clone());
    tracing::info!("L7 ZK: proof worker pool started");

    // ── L8: Hot-path ──────────────────────────────────────────────────────────
    let (hp_runner, hp_tx) = HotPathRunner::new(HotPathConfig {
        channel_capacity: 64,
        revm_trust_window_blocks: 1,
    });
    {
        let h = find_layer(&layers, LayerId::HotPath);
        tokio::spawn(async move {
            hp_runner.run().await;
            h.set_state(HealthState::Halted, "hot-path runner exited unexpectedly");
        });
    }
    tracing::info!("L8 hot-path: runner started");

    // ── L13: Strategy registry ────────────────────────────────────────────────
    let registry = StrategyRegistryBuilder::new(active_phase)
        .register(CnryStrategy::new(CHAIN_ID, &config))
        .expect("CNRY registration must succeed")
        .build();

    tracing::info!(
        total = registry.len(),
        active = registry.active_strategies().len(),
        phase = active_phase,
        "L13 strategy registry built",
    );

    // ── Canary loop ───────────────────────────────────────────────────────────
    {
        let cnry = registry.get(StrategyId::Cnry).expect("CNRY in registry");
        let ora2 = Arc::clone(&oracle);
        let halt2 = halt.clone();
        tokio::spawn(async move { run_canary_loop(cnry, ora2, halt2, 500).await });
    }

    // ── Scoring loop ──────────────────────────────────────────────────────────
    {
        let reg = registry.clone();
        let ora3 = Arc::clone(&oracle);
        let cl3 = Arc::clone(&chainlink_oracle);
        let py3 = Arc::clone(&pyth_oracle);
        let tw3 = Arc::clone(&twap_oracle);
        let dag2 = Arc::clone(&dag);
        let halt3 = halt.clone();
        let tx = hp_tx.clone();
        let pq = proof_queue.clone();
        let ph = active_phase;
        // C2: threaded into score_and_admit via run_scoring_loop — see
        // this file's module-level "C2" doc comment.
        let ep3 = Arc::clone(&execution_pipeline);
        tokio::spawn(async move {
            run_scoring_loop(reg, ora3, cl3, py3, tw3, dag2, tx, pq, halt3, ph, ep3).await;
        });
    }

    // ── Health monitor ────────────────────────────────────────────────────────
    {
        let ls = layers.clone();
        let halt4 = halt.clone();
        tokio::spawn(async move { run_health_monitor(ls, halt4).await });
    }

    tracing::info!(
        active_phase,
        chain_id = CHAIN_ID,
        "OmegaEngine v12.0 running — all layers initialised",
    );

    // ── Shutdown ──────────────────────────────────────────────────────────────
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutdown signal received");
    let _ = sd_tx.send(true);
    halt.halt(LayerId::Health, "operator shutdown");
    tokio::time::sleep(Duration::from_secs(SHUTDOWN_DRAIN_S)).await;
    tracing::info!("OmegaEngine shutdown complete");
    Ok(())
}

// ── Background tasks ──────────────────────────────────────────────────────────

async fn run_canary_loop(
    cnry: Arc<dyn omega_core::StrategyTrait>,
    oracle: Arc<PerChainOracle>,
    halt: HaltFlag,
    interval_ms: u64,
) {
    let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        if halt.is_halted() {
            break;
        }
        let snap = oracle.snapshot();
        let sig = omega_core::SignalState {
            state_version: snap.state_version,
            chain_id: CHAIN_ID,
            block_number: snap.fee.block_number,
            base_fee_gwei: snap.fee.base_fee_gwei,
            l1_data_fee_gwei: snap.fee.l1_data_fee_gwei,
            state_hash: snap.state_hash,
        };
        match cnry.score(&sig).await {
            Ok(op) => tracing::debug!(score = op.score, "CANARY_PASS"),
            Err(e) => tracing::warn!(error = %e, "CANARY_MISS"),
        }
    }
}

// Fix (this revision, 2): clippy::too_many_arguments (10/7) — see this
// file's module-level "Audit fix (this revision, 2)" note, item 1, for
// why an allow (not a struct refactor) is the right fix here, mirroring
// score_and_admit's existing identical attribute below.
#[allow(clippy::too_many_arguments)]
async fn run_scoring_loop(
    registry: StrategyRegistry,
    oracle: Arc<PerChainOracle>,
    chainlink_oracle: Arc<ChainlinkOracle>,
    pyth_oracle: Arc<PythOracle>,
    twap_oracle: Arc<TwapOracle>,
    dag: Arc<Mutex<ExecutionDag>>,
    hp_tx: tokio::sync::mpsc::Sender<HotPathRequest>,
    proof_queue: ProofQueue,
    halt: HaltFlag,
    active_phase: u8,
    // C2: threaded through to score_and_admit — see this file's
    // module-level "C2" doc comment.
    execution_pipeline: Arc<ExecutionPipeline<UnconfiguredSigner>>,
) {
    let mut rx = oracle.subscribe();
    loop {
        if halt.is_halted() {
            break;
        }
        match rx.recv().await {
            Ok(_) => {
                let snap = oracle.snapshot();
                let sig = omega_core::SignalState {
                    state_version: snap.state_version,
                    chain_id: CHAIN_ID,
                    block_number: snap.fee.block_number,
                    base_fee_gwei: snap.fee.base_fee_gwei,
                    l1_data_fee_gwei: snap.fee.l1_data_fee_gwei,
                    state_hash: snap.state_hash,
                };
                // Built once per scoring cycle and copied per spawned task
                // below, rather than once per strategy — every strategy
                // scored in this cycle should see the identical oracle
                // snapshot, the same way they already see the identical
                // `sig`/SignalState.
                let oracle_snapshot = build_oracle_snapshot(
                    &chainlink_oracle,
                    &pyth_oracle,
                    &twap_oracle,
                    ORACLE_SNAPSHOT_TOKEN,
                );
                for strategy in registry.active_strategies() {
                    if strategy.strategy_id().is_canary() {
                        continue;
                    }
                    let s2 = sig.clone();
                    let dag2 = Arc::clone(&dag);
                    let tx2 = hp_tx.clone();
                    let pq2 = proof_queue.clone();
                    let h2 = halt.clone();
                    let ph = active_phase;
                    // Fix (this revision, 2): clippy::clone_on_copy —
                    // OracleSnapshot is Copy (six bare scalar fields, no
                    // heap-allocated members), so `.clone()` here just
                    // called the derived Clone impl to do what plain
                    // assignment already does. See this file's
                    // module-level "Audit fix (this revision, 2)" note,
                    // item 2.
                    let os2 = oracle_snapshot;
                    let ep2 = Arc::clone(&execution_pipeline);
                    tokio::spawn(async move {
                        score_and_admit(strategy, s2, dag2, tx2, pq2, h2, ph, os2, ep2).await;
                    });
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "scoring loop lagged")
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn score_and_admit(
    strategy: Arc<dyn omega_core::StrategyTrait>,
    signal: omega_core::SignalState,
    dag: Arc<Mutex<ExecutionDag>>,
    hp_tx: tokio::sync::mpsc::Sender<HotPathRequest>,
    proof_queue: ProofQueue,
    halt: HaltFlag,
    active_phase: u8,
    oracle_snapshot: OracleSnapshot,
    // C2: real DAG-slot ownership flows through execute()'s own
    // DagSlotGuard now — see this file's module-level "C2" doc comment.
    execution_pipeline: Arc<ExecutionPipeline<UnconfiguredSigner>>,
) {
    if halt.is_halted() {
        return;
    }

    let op = match strategy.score(&signal).await {
        Ok(op) if op.score > 0.0 => op,
        _ => return,
    };
    let _ = op;

    let bp = match strategy.build_blueprint(&signal).await {
        Ok(bp) => bp,
        Err(e) => {
            tracing::debug!(error = %e, "build_blueprint failed");
            return;
        }
    };

    {
        // FIX (this revision): tokio::sync::Mutex .lock().await -> std::sync::Mutex
        // .lock().unwrap() — see module-level "Fix: DAG mutex type" note above.
        let mut g = dag.lock().unwrap();
        if g.admit(bp.clone(), &[]).is_err() {
            return;
        }
    }

    let hot = strategy.hot_path_eligible()
        && bp.lane == omega_core::Lane::Microtx
        && bp.l2_exec_gas_estimate <= MICROTX_GAS_LIMIT;

    if hot {
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        if hp_tx
            .try_send(HotPathRequest {
                blueprint: bp.clone(),
                oracle: oracle_snapshot,
                resp_tx: rtx,
            })
            .is_ok()
        {
            if let Ok(resp) = rrx.await {
                if active_phase >= 1 && resp.result.is_ok() {
                    tracing::info!(hash = %bp.blueprint_hash, "hot-path blueprint ready");
                }
            }
        }
    } else {
        let hb: [u8; 32] = *bp.blueprint_hash;
        let profit: u128 = bp.expected_profit_net.try_into().unwrap_or(u128::MAX);
        let micro = bp.lane == omega_core::Lane::Microtx;
        if let Ok(rx) = proof_queue.submit(hb, profit, CHAIN_ID, bp.strategy_id.to_string(), micro)
        {
            if let Ok(Ok(proof)) = rx.await {
                if active_phase >= 1 {
                    tracing::info!(
                        hash   = %bp.blueprint_hash,
                        gen_ms = proof.generation_ms,
                        "ZK proof ready",
                    );
                }
            }
        }
    }

    // ── C2: ExecutionPipeline::execute — real DAG-slot ownership ─────────
    //
    // See this file's module-level "C2" doc comment for what this call
    // does and does not accomplish yet (structural wiring, not a real
    // risk pipeline), and build_check_context's own doc comment for
    // exactly which CheckContext fields are real vs. deliberately
    // fail-closed placeholders.
    let risk_ctx = build_check_context(&signal, oracle_snapshot);
    let current_block = signal.block_number;
    let current_block_timestamp_secs = chrono::Utc::now().timestamp().max(0) as u64;

    match execution_pipeline
        .execute(
            bp.clone(),
            active_phase,
            &risk_ctx,
            current_block,
            current_block_timestamp_secs,
        )
        .await
    {
        Ok(outcome) => {
            tracing::debug!(
                hash = %bp.blueprint_hash,
                outcome = ?outcome,
                "C2: ExecutionPipeline::execute completed"
            );
        }
        Err(e) => {
            // Expected in practice today, not an operational anomaly —
            // see build_check_context's doc comment: every real
            // blueprint currently fails Stage 2c deterministically
            // (MissGas) until Gap 8's real risk data sources exist.
            // debug rather than warn/error for exactly that reason.
            tracing::debug!(
                hash = %bp.blueprint_hash,
                error = %e,
                "C2: ExecutionPipeline::execute rejected blueprint (expected until Gap 8 risk data is real)"
            );
        }
    }

    // C2: dag.complete() REMOVED here — execute() above is now the SOLE
    // owner of this blueprint's DAG slot via its internal DagSlotGuard
    // (crates/omega-execution/src/pipeline.rs), which releases the slot
    // on every exit path (success, every Stage 0-5 error, and
    // panic-via-Drop) exactly once. Calling dag.complete() here too, on
    // top of that, would be a second, redundant release site — see this
    // file's module-level "C2" doc comment for why that's not
    // "exactly-once ownership" even though ExecutionDag::complete()
    // tolerates the double call numerically.
}

async fn run_health_monitor(layers: [Arc<LayerHealthImpl>; 16], halt: HaltFlag) {
    let mut ticker = tokio::time::interval(Duration::from_secs(2));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        if halt.is_halted() {
            break;
        }
        let halted = layers
            .iter()
            .filter(|l| l.state() == HealthState::Halted)
            .count();
        let degraded = layers
            .iter()
            .filter(|l| l.state() == HealthState::Degraded)
            .count();
        if halted > 0 {
            tracing::error!(halted, degraded, "health check: layers HALTED");
        } else if degraded > 0 {
            tracing::warn!(degraded, "health check: layers degraded");
        } else {
            tracing::debug!("health check: all layers healthy");
        }
    }
}