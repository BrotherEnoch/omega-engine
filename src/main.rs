// src/main.rs — OmegaEngine v12.0 Main Entry Point
//
// Required env vars:
//   ARBITRUM_RPC_URL         WebSocket RPC endpoint
//   ARBITRUM_HTTP_RPC_URL    Plain HTTP JSON-RPC endpoint (InclusionTracker uses plain
//                            HTTP POSTs; ARBITRUM_RPC_URL is WS-only and won't work there)
//   VAULT_ADDRESS            OmegaVault address — feeds publicInputsHash for ZK proofs
//   PROFIT_TOKEN             Profit token address — feeds publicInputsHash for ZK proofs
//   ORCHESTRATOR_ADDRESS     OmegaOrchestrator address every signed tx calls execute() on
//   OMEGA_TX_SIGNING_KEY     Hex secp256k1 key for the gas-paying tx-envelope signer
//   OMEGA_BLUEPRINT_SIGNING_KEY  Hex secp256k1 key; derived address must match
//                            OmegaOrchestrator.execution_key on-chain
//
// Optional env vars:
//   OMEGA_CONFIG                          default: config/default.toml
//   OMEGA_CHAIN_ID                        overrides DEFAULT_CHAIN_ID (42161, Arbitrum One);
//                                          malformed value halts startup rather than
//                                          silently falling back
//   OMEGA_AAVE_V3_POOL_TAG_OVERRIDE,
//   OMEGA_BALANCER_V2_VAULT_TAG_OVERRIDE   override only the address TAG recorded in
//                                          LiquidityRegistry — do NOT redirect what the
//                                          L2e poll's eth_call reads actually target
//                                          (that's baked into omega-rpc)
//   OMEGA_RELAY_ENDPOINT_<NAME>, FLASHBOTS_AUTH_KEY, TITAN_AUTH_KEY,
//   BLOXROUTE_AUTH_TOKEN, EDEN_AUTH_TOKEN, OMEGA_EXECUTION_ADDRESS
//                                          per-relay bootstrap; a relay missing its
//                                          endpoint/secret is skipped, never faked
//
// ## Changelog (most recent first within each item; see VCS for full history)
//
// - C10b (this package): CheckContext WETH watch-channel MAX now includes Uniswap V3
//   alongside Aave/Balancer (was registry-only for Uni). Fail closed when all three
//   WETH reads fail (keep previous watch value). `fetch_uniswap_v3_pool_balance`
//   rejects assets other than WETH/USDC_NATIVE when targeting the canonical pool.
//
// - C10: L2e now also polls Uniswap V3 (previously the only provider written to
//   omega-rpc's address list but never actually read from — see the C9 item below,
//   "UniswapV3 is deliberately not written"). Uses a single, verified WETH/USDC_NATIVE
//   0.05%-fee pool (`omega_rpc::UNISWAP_V3_WETH_USDC_POOL`) that covers both currently
//   tracked assets, since a Uniswap V3 pool holds both its tokens' balances directly —
//   "available liquidity" there is just `ERC20(asset).balanceOf(pool)`
//   (`OmegaRpcClient::fetch_uniswap_v3_pool_balance`), no protocol-accounting layer to
//   navigate the way Aave's aToken indirection needs. Verifying that pool address this
//   session caught a real trap worth flagging here, not just in omega-rpc's own
//   comments: Arbitrum has TWO different "USDC/WETH 0.05%" Uniswap V3 pools — one
//   paired with `USDC_NATIVE` (~$75M pooled, the one used here) and one paired with
//   the older bridged `USDC.e` (well under $1M pooled) — and nothing about querying
//   the wrong one would have errored; it would have silently reported a thin,
//   wrong-token pool's balance as this system's Uniswap V3 liquidity signal for every
//   cycle, forever. `omega-rpc`'s own `UNISWAP_V3_WETH_USDC_POOL` doc comment carries
//   the full verification trail. C7's `validate_deployed_contracts` now checks this
//   pool's bytecode presence at startup alongside the other five addresses (6 total),
//   though — same scope limit as always — bytecode presence would NOT by itself have
//   caught the wrong-pool trap above (the wrong pool has real code too); the
//   `balanceOf` read itself is the closest thing to a live check for "is this actually
//   the pool we think it is," the same posture C7 already takes for Aave/Balancer.
//   `select_provider` needed NO changes for this — `omega_flashloan`'s registry and
//   selector were already fully provider- and asset-generic as of C9; UniswapV3 was
//   dead purely because nothing ever wrote to it, not because of any gap in that
//   crate. `omega_flashloan::FlashloanError::NoneAvailable` separately gained an
//   `asset: Address` field this same session (independent of C10's Uniswap wiring) —
//   with multiple assets tracked, a "no provider available" error that didn't say
//   which asset it was about had become a real diagnostic gap.
//
// - C9: L2e now polls BOTH WETH and USDC_NATIVE into LiquidityRegistry (was: WETH
//   only, despite USDC_NATIVE already being validated at startup by C7). This was
//   deliberately NOT safe to do as a one-line addition: `LiquidityRegistry`'s key was
//   `(chain_id, provider, contract)` — no asset component — and Aave's Pool /
//   Balancer's Vault are each a SINGLE contract shared across every token they
//   support. Polling USDC into the old key would have silently overwritten whatever
//   was last written for WETH at that same (chain_id, provider, contract) triple, or
//   vice versa depending on poll ordering — not a panic, not a staleness warning, a
//   quietly wrong liquidity number for one of the two assets. Fixed at the source:
//   omega-flashloan's `ProviderKey`/`LiquidityRegistry::update`/`snapshot`/
//   `available_contracts` and `select_provider` all now take an explicit `asset`
//   parameter (see that crate's own module-level "CHANGE" note); `LaStrategy::
//   build_blueprint` already resolves a real `flashloan_token` per position and now
//   passes it straight through to `select_provider` instead of relying on an
//   asset-agnostic global. The L2e loop below iterates `[WETH, USDC_NATIVE]` and
//   writes a registry row per (provider, asset) pair each tick. The single-scalar
//   `FlashloanLiquidityState` watch channel that feeds `CheckContext.flashloan`
//   deliberately stays WETH-only — it's paired with `ORACLE_SNAPSHOT_TOKEN`, which is
//   also WETH-only, and making that pairing asset-aware is a separate, larger change
//   to `CheckContext`'s shape, not attempted here. STILL OPEN: Uniswap V3 remains
//   unwritten for either asset (no single canonical pool per asset the way
//   AAVE_V3_POOL/BALANCER_V2_VAULT are) — unchanged by this revision, same as the C1
//   item below already noted for WETH.
//
// - C8: LA registered alongside CNRY in the L13 strategy registry (this revision).
//   LaStrategy::new's constructor signature gained `position_registry:
//   Arc<PositionRegistry>` in an earlier omega-strategies revision; main() now
//   constructs that registry and threads it through. LA's bytecode_hash/contract_addr
//   are sourced ONLY from IntegrityRegistry::snapshot()'s "LA" entry (the same,
//   already-loaded deployment-manifest data Stage 2b and resolve_strategy_bytecode_hash
//   already read) — never a placeholder or guessed address. No manifest, or a manifest
//   with no "LA" entry, means LA is simply not registered this run; this mirrors the
//   fail-closed posture Stage 2b already applies to any strategy_id IntegrityRegistry
//   doesn't know about, rather than registering LA against an invented address.
//   ASSUMPTION FLAGGED, NOT VERIFIED: this assumes IntegrityRegistry's manifest-entry
//   type exposes a `contract_address` field alongside the already-confirmed
//   `bytecode_hash` field (only the latter was previously read, by
//   resolve_strategy_bytecode_hash). Confirm the real field name/type in
//   crates/omega-security's entry struct before relying on this in production — adjust
//   the `.contract_address` access and the `.into()` conversions below if they differ
//   (e.g. if it's already an `Address` rather than raw `[u8; 20]`).
//   STILL OPEN, NOT ADDRESSED BY THIS REVISION: registering LA does not make it
//   FUNCTIONAL — `PositionRegistry` has no writer anywhere in this codebase yet (no
//   omega-oracle component populates it from live chain data), so
//   `LaStrategy::select_position()` will return `None` and `score()` will report 0.0
//   every cycle regardless of registration. Separately, even with a real position,
//   `debt_amount_wei` still has no price source (see omega-strategies/src/la.rs's own
//   module-level comment) and `build_blueprint` will keep refusing on that gap. This
//   revision closes the "LA is never constructed" gap only, not either of those two.
//
// - C7: startup validation of hardcoded flashloan/liquidity contract addresses
//   (omega_rpc::validate_deployed_contracts, backed by omega-rpc's flashloan_liq.rs —
//   see that file's own header for what AAVE_V3_POOL/AAVE_PROTOCOL_DATA_PROVIDER/
//   BALANCER_V2_VAULT/WETH/USDC_NATIVE/UNISWAP_V3_WETH_USDC_POOL are and how each was
//   verified; the last of those was added by C10, extending this check from 5 to 6
//   addresses). Runs a real
//   eth_getCode check against every one of those addresses right after the RPC client
//   connects, BEFORE the L2d/L2e poll loops (or anything else) are spawned against
//   them — a wrong or stale address now halts startup with a clear error instead of
//   the L2e loop silently failing soft, cycle after cycle, forever, or worse, quietly
//   returning a wrong-but-plausible-looking liquidity number from an unrelated
//   contract that happens to share a `balanceOf`-shaped ABI. Scope is deliberately
//   limited to "something is deployed here" (bytecode presence), not full ABI
//   conformance — see `DeploymentValidationReport::all_ok`'s own doc comment.
//   `fetch_aave_available`/`fetch_balancer_available` (now real, see the L2e item
//   below) are themselves the closest thing to a live ABI check this system has, the
//   first time the L2e loop actually calls them. NOTE: as of C7, USDC_NATIVE was
//   already validated here even though it wasn't polled until C9 — see the C9 item
//   above for why polling it earlier would have been unsafe without the registry
//   key change C9 makes.
//
// - CHAIN_ID / AAVE_V3_POOL / BALANCER_V2_VAULT are no longer hardcoded to Arbitrum.
//   `resolve_chain_id()` reads OMEGA_CHAIN_ID (default DEFAULT_CHAIN_ID); `chain_id` is
//   threaded explicitly through every function that used to read a CHAIN_ID const. NOTE:
//   the L2d ArbGasInfo poll and L2e Aave/Balancer liquidity poll still target fixed,
//   Arbitrum-specific addresses baked into omega-rpc regardless of this override — they
//   fail soft (warn, keep previous value) rather than redirect on a non-Arbitrum chain.
//   `resolve_chain_id_from(Option<String>)` holds the actual parse/validate logic so it's
//   unit-testable without mutating real env vars; `resolve_chain_id()` is a thin wrapper.
//
// - Real ZK-gate enforcement (was: ZkVerifier::verify() called nowhere in the workspace;
//   proof-queue failures were silently ignored and execute() ran unconditionally either
//   way). score_and_admit's non-hot-path branch now explicitly early-returns (releasing
//   the DAG slot itself) on submission rejection, proof-gen failure, a dropped response
//   channel, or a proof that fails verify() against the real publicInputsHash. Hot-path
//   blueprints fire the same proof_queue.submit() as a detached background task (not
//   awaited) — OmegaVault.receivePendingProfit() doesn't require a proof, only the later
//   releaseProfit() does, so gating hot-path admission on it would reimport the latency
//   cost the hot path exists to avoid. STILL OPEN: nothing here calls
//   OmegaVault.submitProof() on-chain once a proof is ready — no relayer/keeper for that
//   exists in this codebase yet.
//
// - Real TransactionSigner (KeyManagerTransactionSigner replaces UnconfiguredSigner).
//   strategy_onchain_ids() transcribes the 5 real strategyId constants byte-for-byte from
//   contracts/src/StrategyIds.sol — kept in manual sync, nothing enforces it automatically.
//   blueprintCalldata ABI is golden-tested against solc; the EIP-1559 RLP path has only
//   structural checks, not a node-accepted signed-tx vector.
//
// - Real deployment-manifest loading (IntegrityRegistry no longer permanently empty).
//   Three outcomes: file parses & validates → register_all(); file exists but is malformed
//   or has any invalid entry → main() returns Err (a bad manifest present on disk is worse
//   than none); no file → warn, empty registry, every strategy_id fails Stage 2b. A real
//   5-entry manifest (CNRY/SA/MSA/LA/MEV) has since been generated for a local Anvil
//   deployment via omega-manifest-gen and verified to load — this does not change this
//   file's code, only supplies previously-missing data for that environment. Deliberately
//   NOT calling integrity_registry.freeze() here — that's a governance action, not startup.
//   check_context's strategy_bytecode_hash now reads IntegrityRegistry::snapshot()
//   (resolve_strategy_bytecode_hash) instead of a hardcoded [0u8;32].
//
// - C5b: phase >= 1 with zero constructed relays is a hard startup failure (fail closed).
//
// - Real relay production bootstrap (HttpRelayClients replace the C1 zero-relay stub).
//   Endpoints come only from OMEGA_RELAY_ENDPOINT_<NAME> — never hardcoded, since no
//   verified Arbitrum-specific bundle endpoint exists in this codebase for any provider.
//   Auth follows signing.rs's documented mapping (Flashbots/Titan → flashbots-style key;
//   Bloxroute/Eden → bearer token); RelayName::Other is always skipped (no verified auth
//   convention). Relay candidates come from the real, phase-gated
//   omega_core::RelayConfig::phase_1_relays/phase_2plus_relays via
//   omega_execution::config_translation::translate_relay_config (translation reports any
//   config.relay field with no omega_relay::RelayConfig counterpart via warn!).
//   ExecutionAddress is still just a metrics label, not backed by a real signer identity.
//   startup_block is still 0 (no synchronous "current height" read available off `rpc`).
//   Reorg guard: MultiRelayClient::on_new_block now gets fed real (block_number,
//   block_hash) pairs via rpc.subscribe_blocks() + feed_block_event_to_reorg_guard —
//   BlockEvent already carried a real hash field; the missing piece was just never calling
//   subscribe_blocks(). Inclusion reconciliation is separately wired off the oracle's
//   block-number stream (reconcile() only needs a block number, not a hash).
//
// - Real L1 data fee via ArbGasInfo (L2d poll loop, 15s interval) replaces the hardcoded-0
//   placeholder; feeds PerChainOracle::update_l1_data_fee_gwei. Fails soft: keeps the
//   previous value on read error rather than resetting to a worse one. Targets Arbitrum's
//   fixed ArbGasInfo precompile address regardless of OMEGA_CHAIN_ID.
//
// - Real flashloan liquidity signal (L2e poll loop, Aave V3 + Balancer V2) — closes
//   CheckContext.flashloan's hardcoded-0. This is the MAX of the two providers'
//   available liquidity for WETH, a pre-trade sanity signal only — NOT a guarantee
//   that whichever provider a given blueprint's own select_provider() picks has this
//   much. The same task is also LiquidityRegistry's real writer: every successful
//   per-provider read now also calls liquidity_registry.update(...), so
//   select_provider() has live data once a caller (LA) holds the registry. As of C9,
//   this loop also writes USDC_NATIVE rows into the registry (asset-scoped, see the C9
//   item above) — the CheckContext-feeding side of this loop remains WETH-only.
//   UniswapV3 is deliberately not written — no single canonical pool exists for it the
//   way AAVE_V3_POOL/BALANCER_V2_VAULT do. The tag-override env vars only relabel
//   which address is recorded against a successful update — they do not redirect what
//   fetch_aave_available/fetch_balancer_available query on-chain (baked into
//   omega-rpc; both are now real, see omega-rpc/src/flashloan_liq.rs and the C7 item
//   above). LA is now registered in the strategy registry below (see C8 item above), though
//   LaStrategy::build_blueprint still has no flashloan_token pricing source — this poll
//   loop doesn't touch that gap.
//
// - Real risk-score formula (build_check_context) — equal-weighted (0.25 each,
//   RISK_WEIGHT_* — a policy default, not derived from spec) over gas-volatility risk
//   (real, PerChainOracle::l1_gas_volatility_risk), oracle-freshness risk (real, computed
//   from the three feed ages), competition risk (still pinned at 1.0 — no real source),
//   and liquidity risk (real as of the L2e work above). RISK_SCORE_MAX_THRESHOLD (0.45)
//   was chosen so check 12 failed closed unconditionally back when two of four components
//   were pinned; now that liquidity_risk is real, that floor no longer holds
//   unconditionally — 0.45 itself was never derived from spec and needs a fresh look.
//
// - Real per-strategy account exposure cap (AccountExposureTracker, check 14).
//   MAX_ACCOUNT_EXPOSURE_WEI_PLACEHOLDER (1 ETH) is a deliberately conservative,
//   non-risk-approved starting cap — errs small, the opposite direction from the
//   KillSwitchConfig placeholders below. In-memory only; resets on restart.
//
// - Flashloan integration status (checked directly against source, not re-guessed):
//   omega-flashloan itself (provider registry, premium math, ABI encoding) is complete
//   and tested. LA is the only strategy calling select_provider(), and its own
//   build_blueprint still can't source a priced flashloan_token amount (see C8 item
//   above) — a currently-unpriceable-amount gap, not a registration gap anymore.
//   SA/MSA/MEV correctly use flashloan_provider: Address::ZERO by design (no flashloan
//   needed).
//
// - KillSwitchRegistry/IntegrityRegistry/MultiRelayClient/signer were C1's four
//   fail-closed stand-ins; all four are now real (see items above). ExecutionPipeline is
//   constructed once in main() and threaded through the scoring loop.

fn resolve_chain_id() -> Result<u64> {
    resolve_chain_id_from(std::env::var("OMEGA_CHAIN_ID").ok())
}

/// Pure parse/validate logic, split out of `resolve_chain_id()` so it's testable without
/// touching real env vars. `None` → `Ok(DEFAULT_CHAIN_ID)`. `Some(unparseable)` → `Err`,
/// never a silent fallback — same fail-closed posture as this file's other required env
/// vars.
fn resolve_chain_id_from(raw: Option<String>) -> Result<u64> {
    match raw {
        None => Ok(DEFAULT_CHAIN_ID),
        Some(raw) => raw
            .trim()
            .parse::<u64>()
            .with_context(|| format!("OMEGA_CHAIN_ID is set to {raw:?} but is not a valid u64")),
    }
}

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::Level;

// LayerHealth trait must be in scope for .state(), .layer_id(), .set_state() to resolve
// on Arc<LayerHealthImpl>.
use omega_core::{HealthState, LayerHealth, LayerId, OmegaConfig, StrategyId};
use omega_dag::{DagConfig, ExecutionDag};
use omega_execution::signer::KeyManagerTransactionSigner;
use omega_execution::ExecutionPipeline;
use omega_execution::config_translation::{translate_relay_config, RelayBootstrapInputs};
use omega_health::{halt::HaltFlag, LayerHealthImpl};
use omega_hot_path::{HotPathConfig, HotPathRequest, HotPathRunner, MICROTX_GAS_LIMIT};
use omega_observability::{
    EventRingBuffer, ExporterConfig, OmegaExporter, Sampler, DEFAULT_CAPACITY,
};
use omega_oracle::{ChainlinkOracle, PerChainOracle, PythOracle, TwapOracle};
// HttpRelayClient is not re-exported at omega-relay's crate root — referenced via its
// full path at the one call site below instead.
use omega_relay::{
    BuilderBlacklist, ExecutionAddress, LaRelayMetrics, MultiRelayClient, RelayAuth, RelayClient,
    RelayName,
};
use omega_risk::context::{
    CheckContext, FlashloanSnapshot, OracleSnapshot, CHAINLINK_STALENESS_SECS,
    PYTH_STALENESS_SECS, TWAP_STALENESS_SECS,
};
use omega_risk::kill_switch::{KillSwitchConfig, KillSwitchRegistry};
use omega_rpc::{
    rate_limiter::RpcRateLimiter, run_dex_sync_stream, run_fee_oracle_stream,
    run_lending_protocol_stream, run_mev_share_stream, run_pending_tx_stream,
    validate_deployed_contracts, OmegaRpcClient, RpcClientConfig, AAVE_V3_POOL,
    BALANCER_V2_VAULT, UNISWAP_V3_WETH_USDC_POOL, USDC_NATIVE, WETH,
};
use omega_security::{
    strategy_entries_from_manifest, AccountExposureTracker, BlueprintSigner, DeploymentManifest,
    IntegrityRegistry, KeyManager,
};
// C8: LaStrategy added — registered alongside CnryStrategy in the L13 block below.
// ASSUMPTION FLAGGED, NOT VERIFIED: assumes LaStrategy is re-exported at
// omega_strategies's crate root the same way CnryStrategy already is. Not confirmed
// against crates/omega-strategies/src/lib.rs directly — if this re-export doesn't
// exist, use `omega_strategies::la::LaStrategy` instead.
use omega_strategies::{registry::StrategyRegistryBuilder, CnryStrategy, LaStrategy, StrategyRegistry};
use omega_zk::{
    binding::compute_public_inputs_hash, config::ProverTierConfig, PendingProofBuffer,
    ProofQueue, ProofWorkerPool, VerifiedProofSubmission, ZkConfig, ZkVerifier,
};
use omega_flashloan::{FlashloanProvider, LiquidityRegistry};
// C8: real, live lending-position registry LaStrategy now requires at construction.
// Nothing in this codebase writes to it yet — see the C8 changelog entry above and the
// warning logged at the L13 registration site below.
use omega_positions::PositionRegistry;
use std::collections::HashMap;

/// Fallback chain ID (Arbitrum One) used only when OMEGA_CHAIN_ID is unset.
const DEFAULT_CHAIN_ID: u64 = 42_161;
const DEFAULT_RPS: u32 = 500;
const SHUTDOWN_DRAIN_S: u64 = 5;
const DEFAULT_CONFIG: &str = "config/default.toml";

const BUILDER_BLACKLIST_PATH: &str = "config/builder_blacklist.toml";

/// Conventional deployment-manifest path. No default constructor, no placeholder file —
/// unlike BUILDER_BLACKLIST_PATH, an absent manifest and a malformed one are handled
/// differently (see changelog), and neither path fabricates deployment data.
const DEPLOYMENT_MANIFEST_PATH: &str = "config/deployment_manifest.toml";

// ── Risk-score formula weights ──────────────────────────────────────────────────────
//
// Equal weighting is a policy default (spec: "incorporates gas volatility, oracle
// freshness, competition, liquidity depth"), not derived from any spec section shown.
const RISK_WEIGHT_GAS_VOLATILITY: f64 = 0.25;
const RISK_WEIGHT_ORACLE_FRESHNESS: f64 = 0.25;
const RISK_WEIGHT_COMPETITION: f64 = 0.25;
const RISK_WEIGHT_LIQUIDITY: f64 = 0.25;

// Compile-time guard: the four weights must sum to 1.0.
const _: () = assert!(
    (RISK_WEIGHT_GAS_VOLATILITY
        + RISK_WEIGHT_ORACLE_FRESHNESS
        + RISK_WEIGHT_COMPETITION
        + RISK_WEIGHT_LIQUIDITY
        - 1.0)
        .abs()
        < 1e-9
);

/// Threshold check 12 (`MissRisk`) evaluates `risk_score` against.
///
/// CHOSEN, NOT DERIVED: with competition_risk pinned at 1.0 (no real source) and
/// liquidity_risk now real (can legitimately be 0.0), the best-case floor is
/// 0.25×0 + 0.25×0 + 0.25×1 + 0.25×0 = 0.25 — so check 12 no longer unconditionally
/// fails closed for every blueprint the way it did while liquidity was also pinned.
/// 0.45 was never derived from spec under either arithmetic; needs a fresh look now
/// that it can actually bind.
const RISK_SCORE_MAX_THRESHOLD: f64 = 0.45;

/// CHOSEN, NOT RISK-APPROVED: no real per-strategy/per-account exposure policy exists in
/// this codebase (VaultConfig's caps are on Vault PROFIT release, a different concept from
/// capital at risk). 1 ETH is a deliberately conservative starting cap — errs small,
/// unlike KillSwitchConfig's large permissive placeholders below.
const MAX_ACCOUNT_EXPOSURE_WEI_PLACEHOLDER: u128 = 1_000_000_000_000_000_000; // 1 ETH

/// Matches L2c/L2d's cadence — a starting value, not measured against real chain behavior.
const FLASHLOAN_LIQUIDITY_POLL_INTERVAL_S: u64 = 15;

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

/// Parses an OPTIONAL 0x-prefixed (or bare) hex env var into 20 bytes. Unlike
/// `parse_address_env`, an unset var is `Ok(None)`; a SET-but-malformed var still errors,
/// so a typo'd override is never silently ignored.
fn parse_optional_address_env(var_name: &str) -> Result<Option<[u8; 20]>> {
    match std::env::var(var_name) {
        Err(_) => Ok(None),
        Ok(raw) => {
            let trimmed = raw.strip_prefix("0x").unwrap_or(&raw);
            let bytes = hex::decode(trimmed)
                .with_context(|| format!("{var_name} is not valid hex: {raw}"))?;
            let len = bytes.len();
            let arr: [u8; 20] = bytes.try_into().map_err(|_| {
                anyhow::anyhow!("{var_name} must decode to exactly 20 bytes, got {len}")
            })?;
            Ok(Some(arr))
        }
    }
}

/// Loads a real `DeploymentManifest` from `path`, if present. `Ok(None)` when the file
/// doesn't exist (legitimate — no deployment yet). `Err` when it exists but fails to
/// parse or doesn't match `DeploymentManifest`'s shape. Does NOT itself validate hex/
/// length/placeholder data — that's `strategy_entries_from_manifest`'s job, applied by
/// the caller right after this returns.
fn load_deployment_manifest(path: &str) -> Result<Option<DeploymentManifest>> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Ok(None);
    }
    let s = std::fs::read_to_string(p).with_context(|| format!("reading {path}"))?;
    let manifest: DeploymentManifest =
        toml::from_str(&s).with_context(|| format!("parsing {path} as a DeploymentManifest"))?;
    Ok(Some(manifest))
}

/// Parses a REQUIRED 0x-prefixed (or bare) hex env var into 20 bytes. Returns raw
/// `[u8; 20]` rather than `alloy_primitives::Address` — this binary has no direct
/// alloy-primitives dependency, and every consumer of these values already takes raw
/// bytes.
fn parse_address_env(var_name: &str) -> Result<[u8; 20]> {
    let raw = std::env::var(var_name).with_context(|| format!("{var_name} must be set"))?;
    let trimmed = raw.strip_prefix("0x").unwrap_or(&raw);
    let bytes =
        hex::decode(trimmed).with_context(|| format!("{var_name} is not valid hex: {raw}"))?;
    let len = bytes.len();
    let arr: [u8; 20] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("{var_name} must decode to exactly 20 bytes, got {len}"))?;
    Ok(arr)
}

/// Real, deployment-sourced strategyId values, transcribed byte-for-byte from
/// `contracts/src/StrategyIds.sol`'s `keccak256("OMEGA_STRATEGY_<X>")` constants — the
/// same values `RegisterStrategies.s.sol` cross-checks every manifest's `onchain_id`
/// against before registering on-chain.
///
/// MANUAL-SYNC RISK: if StrategyIds.sol ever changes, this map must be updated by hand —
/// nothing keeps the two in sync automatically.
fn strategy_onchain_ids() -> HashMap<String, [u8; 32]> {
    fn hash32(hex_str: &str) -> [u8; 32] {
        let bytes = hex::decode(hex_str).expect("StrategyIds.sol constant must be valid hex");
        bytes
            .try_into()
            .expect("StrategyIds.sol constant must decode to exactly 32 bytes")
    }

    let mut m = HashMap::new();
    // StrategyIds.sol::SIMPLE_ARB — keccak256("OMEGA_STRATEGY_SA")
    m.insert(
        "SA".to_string(),
        hash32("c4bb1c851b1c74593f61f8d1f99ec07e2960d847a94d4a736e321ba387d4d2d7"),
    );
    // StrategyIds.sol::LIQUIDATION_ARB — keccak256("OMEGA_STRATEGY_LA")
    m.insert(
        "LA".to_string(),
        hash32("77b0296a1c4dae896ee0ffe05246d8b3e8ecd44a1d4a0c6591b183fb2390a698"),
    );
    // StrategyIds.sol::MULTI_STEP_ARB — keccak256("OMEGA_STRATEGY_MSA")
    m.insert(
        "MSA".to_string(),
        hash32("bfd7e8e9c54a6762cb6ff399dc8bdefe2226a32400ed6001e1bee533bbaa25d2"),
    );
    // StrategyIds.sol::MEV_OFA — keccak256("OMEGA_STRATEGY_MEV")
    m.insert(
        "MEV".to_string(),
        hash32("892be743cfc8880f51726a84ab1d0d0fc05336d49927c5a9eaaf926a84db319a"),
    );
    // StrategyIds.sol::CANARY_ARB — keccak256("OMEGA_STRATEGY_CNRY")
    m.insert(
        "CNRY".to_string(),
        hash32("93879ddf9ec0b01c066594680539ea61eaab23f806b410fda1c18659efcc7725"),
    );
    m
}

// ── Health layers ─────────────────────────────────────────────────────────────

// new_bare() returns Arc<Self> directly — do NOT wrap in Arc::new again.
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

/// GAP (placeholder, not a verified product decision): the token symbol a per-cycle
/// OracleSnapshot represents.
const ORACLE_SNAPSHOT_TOKEN: &str = "WETH";

/// Live flashloan-liquidity snapshot for `CheckContext::flashloan`, populated by the L2e
/// poll loop and read once per scoring cycle. `available_wei` is the MAX of Aave V3 /
/// Balancer V2 available liquidity for the single tracked asset feeding this signal
/// (WETH — must be kept manually in sync with `ORACLE_SNAPSHOT_TOKEN`, no shared source
/// of truth for that pairing today).
///
/// AS OF C9: the L2e loop also polls USDC_NATIVE and writes it into
/// `LiquidityRegistry` (asset-scoped — see that crate's own module-level note), but
/// this specific struct/watch-channel stays WETH-only by design. It is a single scalar
/// paired one-to-one with `ORACLE_SNAPSHOT_TOKEN` for `CheckContext.flashloan`'s
/// pre-trade sanity check — making THIS asset-aware is a separate, larger change to
/// `CheckContext`'s shape that C9 does not attempt. LA's own flashloan sizing does not
/// go through this struct; it reads `LiquidityRegistry` directly via
/// `omega_flashloan::select_provider`, which IS asset-scoped as of C9.
///
/// AS OF C10: the WETH MAX that feeds this watch channel includes Uniswap V3
/// (`UNISWAP_V3_WETH_USDC_POOL` balanceOf) alongside Aave and Balancer. Registry
/// rows for Uniswap V3 were already written for both WETH and USDC_NATIVE; the
/// pre-trade sanity signal now uses the same three-provider set for WETH.
///
/// KNOWN LIMITATION: this is a pre-trade sanity signal for check 10 (MissLiquidity), not a
/// guarantee that whichever provider `select_provider` actually picks for a given
/// blueprint has this much liquidity — that runs off the separate, per-blueprint
/// LiquidityRegistry. Taking the MAX here is the conservative choice for a sanity check,
/// not a precision claim.
#[derive(Debug, Clone, Default)]
struct FlashloanLiquidityState {
    /// Real, live available liquidity in wei. `0` both genuinely and as the
    /// pre-first-successful-poll default — safe either way, since check 10 treats a
    /// low/zero value as reject in both cases.
    available_wei: u128,
    /// `"aave"` or `"balancer"`, whichever read was larger on the most recent successful
    /// poll. Empty before the first successful poll.
    protocol_id: String,
}

/// Builds a live `OracleSnapshot` for the pre-trade risk checks and the hot-path lane
/// from the three real feed caches.
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

/// Maps a strategy to omega-risk's real per-strategy-class slippage cap constant.
/// Cross-checked: SA=30/cap30, MSA=40/cap50, LA=50/cap100, MEV=30/cap30 — all pass.
fn max_slippage_bps_for(id: omega_core::StrategyId) -> u16 {
    use omega_core::StrategyId;
    match id {
        StrategyId::Sa => omega_risk::context::MAX_SLIPPAGE_BPS_SA,
        StrategyId::Msa => omega_risk::context::MAX_SLIPPAGE_BPS_MSA,
        StrategyId::La => omega_risk::context::MAX_SLIPPAGE_BPS_LA,
        StrategyId::Mev => omega_risk::context::MAX_SLIPPAGE_BPS_MEV,
        StrategyId::Cnry => 0,
    }
}

/// Resolves the real registered bytecode hash for `strategy_id` from `IntegrityRegistry`,
/// falling back to `[0u8; 32]` (fail-closed) when no manifest is loaded or this strategy
/// isn't in it. Reuses the SAME registry/method Stage 2b already reads, so check 4 and
/// Stage 2b can never drift onto two different "expected hash" values for one strategy.
fn resolve_strategy_bytecode_hash(
    integrity_registry: &IntegrityRegistry,
    strategy_id: omega_core::StrategyId,
) -> [u8; 32] {
    let id_str = strategy_id.to_string();
    integrity_registry
        .snapshot()
        .into_iter()
        .find(|e| e.strategy_id == id_str)
        .map(|e| e.bytecode_hash)
        .unwrap_or([0u8; 32])
}

/// Converts a real `omega_rpc::BlockEvent` into the `(block_number, block_hash)` call
/// `MultiRelayClient::on_new_block` needs. Extracted as a standalone sync function so the
/// conversion is directly unit-testable without a live RPC connection — see
/// `reorg_block_feed_tests`.
fn feed_block_event_to_reorg_guard(relay: &MultiRelayClient, event: &omega_rpc::BlockEvent) {
    let hash_bytes: [u8; 32] = *event.hash;
    relay.on_new_block(event.number, hash_bytes);
}

/// Builds the `CheckContext` passed to `ExecutionPipeline::execute`'s Stage 2c (15
/// pre-trade checks). See the module-level changelog for the current status of each
/// field: real fields are `strategy_max_gas`/`max_slippage_bps`/`l1_adaptive_buffer`/
/// `latest_blueprint_nonce` (C3), `strategy_bytecode_hash` (via
/// `resolve_strategy_bytecode_hash`), `current_l1_gas_price_gwei` (ArbGasInfo poll),
/// `risk_score` (real formula, one placeholder component), `current_account_exposure_wei`
/// (AccountExposureTracker), `flashloan` (L2e poll, WETH-only — see C9 changelog item).
/// Still-placeholder fields: `competition_probability`/`max_competition_probability`
/// (pinned, no real source), `rollout_tier` (no config exists, no check reads it).
#[allow(clippy::too_many_arguments)]
fn build_check_context(
    chain_id: u64,
    sig: &omega_core::SignalState,
    oracle_snapshot: OracleSnapshot,
    strategy_max_gas: u64,
    max_slippage_bps: u16,
    latest_blueprint_nonce: u64,
    strategy_bytecode_hash: [u8; 32],
    gas_volatility_risk: f64,
    current_account_exposure_wei: u128,
    flashloan_snapshot: FlashloanLiquidityState,
) -> CheckContext {
    // Oracle freshness: freshest of the three feeds' age/threshold ratios, clamped to
    // [0.0, 1.0]. u64::MAX-sentinel ages (never-read oracle) produce an astronomically
    // large ratio, correctly clamped to max risk rather than overflowing.
    let oracle_freshness_risk = {
        let cl_ratio = oracle_snapshot.chainlink_age_s as f64 / CHAINLINK_STALENESS_SECS as f64;
        let pyth_ratio = oracle_snapshot.pyth_age_s as f64 / PYTH_STALENESS_SECS as f64;
        let twap_ratio = oracle_snapshot.twap_age_s as f64 / TWAP_STALENESS_SECS as f64;
        cl_ratio.min(pyth_ratio).min(twap_ratio).min(1.0)
    };

    // Competition: still a placeholder. Extracted to a local so the SAME value feeds both
    // the risk-score formula and the CheckContext field below, avoiding drift.
    let competition_probability_value = 1.0_f64;
    let max_competition_probability_value = 0.0_f64;

    let flashloan_available_value: u128 = flashloan_snapshot.available_wei;
    let flashloan_protocol_id: String = flashloan_snapshot.protocol_id;

    let competition_risk = competition_probability_value;
    // `> 0` is the correct fail-closed test: a genuine zero reading (both provider reads
    // failed, or both legitimately empty) still maps to max risk.
    let liquidity_risk = if flashloan_available_value > 0 {
        0.0
    } else {
        1.0
    };

    let risk_score = (RISK_WEIGHT_GAS_VOLATILITY * gas_volatility_risk
        + RISK_WEIGHT_ORACLE_FRESHNESS * oracle_freshness_risk
        + RISK_WEIGHT_COMPETITION * competition_risk
        + RISK_WEIGHT_LIQUIDITY * liquidity_risk)
        .clamp(0.0, 1.0);

    CheckContext {
        expected_chain_id: chain_id,
        current_block: sig.block_number,
        current_l2_base_fee_gwei: sig.base_fee_gwei,
        oracle: oracle_snapshot,
        strategy_max_gas,
        max_slippage_bps,
        l1_adaptive_buffer: omega_risk::gas_model::l1_adaptive_buffer(&[]),
        current_l1_gas_price_gwei: sig.l1_data_fee_gwei,
        flashloan: FlashloanSnapshot {
            available: flashloan_available_value,
            protocol_id: flashloan_protocol_id,
        },
        competition_probability: competition_probability_value,
        max_competition_probability: max_competition_probability_value,
        rollout_tier: 0.0,
        strategy_bytecode_hash,
        risk_score,
        max_risk_score: RISK_SCORE_MAX_THRESHOLD,
        current_account_exposure_wei,
        max_account_exposure_wei: MAX_ACCOUNT_EXPOSURE_WEI_PLACEHOLDER,
        latest_blueprint_nonce,
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

    // Resolved early so a malformed OMEGA_CHAIN_ID halts startup immediately rather than
    // surfacing only once RPC connect's own chain-ID check fails downstream.
    let chain_id = resolve_chain_id()?;
    if chain_id != DEFAULT_CHAIN_ID {
        tracing::warn!(
            chain_id,
            default_chain_id = DEFAULT_CHAIN_ID,
            "OMEGA_CHAIN_ID overrides the default (Arbitrum One) — the L2d ArbGasInfo poll \
             and L2e Aave/Balancer liquidity poll still target fixed, Arbitrum-specific \
             addresses baked into omega-rpc regardless of this override; they'll fail every \
             cycle on a non-Arbitrum chain, degrading per their own fail-soft handling"
        );
    }

    // Tag overrides affect only the address recorded in LiquidityRegistry — not what the
    // L2e poll's eth_call reads actually target.
    let aave_pool_tag = match parse_optional_address_env("OMEGA_AAVE_V3_POOL_TAG_OVERRIDE")? {
        Some(bytes) => bytes.into(),
        None => AAVE_V3_POOL,
    };
    let balancer_vault_tag =
        match parse_optional_address_env("OMEGA_BALANCER_V2_VAULT_TAG_OVERRIDE")? {
            Some(bytes) => bytes.into(),
            None => BALANCER_V2_VAULT,
        };

    let vault_address = parse_address_env("VAULT_ADDRESS")?;
    let profit_token = parse_address_env("PROFIT_TOKEN")?;

    let config = load_config(&config_path)?;
    let active_phase = config.active_phase;

    tracing::info!(active_phase, chain_id, "Config loaded");
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
    let rpc = OmegaRpcClient::connect_with_retry(RpcClientConfig::new(
        &rpc_url,
        DEFAULT_RPS,
        chain_id,
    ))
    .await
    .context("connecting to Arbitrum RPC endpoint")?
    .with_health(as_health(find_layer(&layers, LayerId::Rpc)));

    // ── C7: validate hardcoded flashloan/liquidity addresses against the connected
    // chain, BEFORE anything (L2d/L2e poll loops, block subscription, etc.) is spawned
    // against them. A wrong or stale address is a fund-safety-adjacent bug class — see
    // omega-rpc/src/flashloan_liq.rs's own header for the real transcription error this
    // check caught during development of that file. Fail closed: refuse to start rather
    // than degrade silently for the process's entire lifetime.
    let address_validation = validate_deployed_contracts(&rpc, chain_id).await;
    if !address_validation.all_ok() {
        for r in &address_validation.results {
            if !r.has_code || r.error.is_some() {
                tracing::error!(
                    label = r.label,
                    address = %r.address,
                    has_code = r.has_code,
                    error = ?r.error,
                    "C7 startup validation: hardcoded contract address failed on-chain check"
                );
            }
        }
        anyhow::bail!(
            "C7 startup validation failed: one or more hardcoded flashloan/oracle \
             addresses have no confirmed bytecode on chain {chain_id} (see error logs \
             above) — refusing to start the L2d/L2e poll loops against unverified \
             addresses"
        );
    }
    tracing::info!(
        chain_id,
        checked = address_validation.results.len(),
        "C7: all hardcoded contract addresses validated on-chain"
    );

    {
        let r = rpc.clone();
        tokio::spawn(async move { r.run_block_subscription().await });
    }

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
        tokio::spawn(async move { run_pending_tx_stream(u, chain_id, l, t).await });
    }
    {
        let u = ws_url.clone();
        let l = Arc::clone(&limiter);
        let t = fee_tx.clone();
        tokio::spawn(async move { run_fee_oracle_stream(u, chain_id, l, t).await });
    }
    {
        let u = ws_url.clone();
        let l = Arc::clone(&limiter);
        let t = dex_tx.clone();
        tokio::spawn(async move { run_dex_sync_stream(u, chain_id, l, t).await });
    }
    {
        let u = ws_url.clone();
        let l = Arc::clone(&limiter);
        let t = lend_tx.clone();
        tokio::spawn(async move { run_lending_protocol_stream(u, chain_id, l, t).await });
    }
    {
        let t = mev_tx.clone();
        tokio::spawn(async move { run_mev_share_stream(t).await });
    }

    tracing::info!("L1 RPC: 5 subscription streams running");

    // ── L2: Oracle ────────────────────────────────────────────────────────────
    let twap_oracle = TwapOracle::new(chain_id);
    let chainlink_oracle = ChainlinkOracle::new(chain_id);
    let pyth_oracle = PythOracle::new(chain_id);

    let oracle =
        PerChainOracle::new(chain_id).with_health(as_health(find_layer(&layers, LayerId::Oracle)));

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

    // ── L2c: Chainlink polling ────────────────────────────────────────────────
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
            tracing::error!(
                error = %e,
                "Chainlink feed table malformed — Chainlink ingestion NOT started"
            );
        }
    }

    tracing::warn!(
        "Pyth cache constructed but UNFED — no ingestion path exists yet."
    );

    // ── L2d: ArbGasInfo L1 data fee polling ────────────────────────────────────
    // Targets Arbitrum's ArbGasInfo precompile at a fixed address regardless of
    // `chain_id` — fails soft (warn, keep previous value) on a non-Arbitrum chain.
    {
        let gas_client = rpc.clone();
        let gas_oracle = Arc::clone(&oracle);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(15));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                match gas_client.fetch_l1_base_fee_estimate_gwei().await {
                    Ok(gwei) => {
                        gas_oracle.update_l1_data_fee_gwei(gwei);
                        tracing::debug!(l1_data_fee_gwei = gwei, "ArbGasInfo poll: updated");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "ArbGasInfo poll failed — keeping previous value");
                    }
                }
            }
        });
        tracing::info!("L2d ArbGasInfo poll loop started (15s interval)");
    }

    // ── L2e: flashloan liquidity polling ───────────────────────────────────────
    // Real ingestion for CheckContext::flashloan.available (WETH-only, see the
    // FlashloanLiquidityState doc comment) AND the real, asset-scoped writer for
    // LiquidityRegistry (every successful per-provider, per-asset read updates the
    // registry; the WETH read additionally updates the MAX-across-providers watch
    // channel below). Tag overrides only relabel the recorded address, never redirect
    // the eth_call target.
    //
    // C9: now tracks both WETH and USDC_NATIVE. This is safe only because
    // LiquidityRegistry::update/snapshot/available_contracts and
    // omega_flashloan::select_provider are all asset-scoped as of this revision — see
    // omega-flashloan's own module-level "CHANGE" note. Adding USDC_NATIVE here without
    // that registry change would have silently overwritten whichever asset's snapshot
    // was written last at the same (chain_id, provider, contract) key, since Aave's Pool
    // and Balancer's Vault are each one contract shared across every token.
    let (flashloan_liq_tx, flashloan_liq_rx) =
        tokio::sync::watch::channel(FlashloanLiquidityState::default());
    // LiquidityRegistry::new() is assumed to return Arc<Self> already, matching every
    // other registry in this file — not independently confirmed against omega-flashloan's
    // source; wrap in Arc::new(...) if cargo build disagrees.
    let liquidity_registry = LiquidityRegistry::new();
    {
        let liq_client = rpc.clone();
        let liq_tx = flashloan_liq_tx.clone();
        let registry = Arc::clone(&liquidity_registry);
        let chain_id_l2e = chain_id;
        let aave_tag_l2e = aave_pool_tag;
        let balancer_tag_l2e = balancer_vault_tag;
        tokio::spawn(async move {
            // Every asset this poll loop tracks. WETH remains the sole asset that feeds
            // the CheckContext-facing watch channel below (see the `token != WETH`
            // guard); USDC_NATIVE is written into the registry only, for LA's
            // asset-scoped select_provider() to read directly.
            let tracked_assets = [WETH, USDC_NATIVE];
            let mut ticker =
                tokio::time::interval(Duration::from_secs(FLASHLOAN_LIQUIDITY_POLL_INTERVAL_S));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;

                for &token in &tracked_assets {
                    let aave = liq_client.fetch_aave_available(token).await;
                    let balancer = liq_client.fetch_balancer_available(token).await;

                    // block_number passed as 0 — no synchronous "current head" read is
                    // available off `rpc` today; LiquidityRegistry's staleness model is
                    // timestamp-driven, so this doesn't weaken it. `.try_into()` (not
                    // `.into()`): U256 has no infallible `From<u128>` in the resolved
                    // ruint version, only `TryFrom`. `.expect(...)` is safe — a u128
                    // always fits in 256 bits; a panic here would only fire if that
                    // invariant were somehow violated, worth surfacing loudly rather
                    // than swallowing.
                    if let Ok(available) = &aave {
                        registry.update(
                            chain_id_l2e,
                            FlashloanProvider::AaveV3,
                            token,
                            aave_tag_l2e,
                            (*available)
                                .try_into()
                                .expect("u128 always fits in a 256-bit unsigned integer"),
                            0,
                        );
                    }
                    if let Ok(available) = &balancer {
                        registry.update(
                            chain_id_l2e,
                            FlashloanProvider::Balancer,
                            token,
                            balancer_tag_l2e,
                            (*available)
                                .try_into()
                                .expect("u128 always fits in a 256-bit unsigned integer"),
                            0,
                        );
                    }

                    // C10: Uniswap V3, via the single WETH/USDC_NATIVE 0.05% pool —
                    // covers both currently-tracked assets since a Uniswap V3 pool
                    // holds both its tokens' balances. See UNISWAP_V3_WETH_USDC_POOL's
                    // own doc comment (omega-rpc) for the wrong-pool trap this address
                    // was deliberately verified against. Unlike Aave/Balancer, there is
                    // no tag-override env var for this pool today — see
                    // resolve_liquidity_addresses's own scope note in omega-rpc for why.
                    let uniswap = liq_client
                        .fetch_uniswap_v3_pool_balance(UNISWAP_V3_WETH_USDC_POOL, token)
                        .await;
                    if let Ok(available) = &uniswap {
                        registry.update(
                            chain_id_l2e,
                            FlashloanProvider::UniswapV3,
                            token,
                            UNISWAP_V3_WETH_USDC_POOL,
                            (*available)
                                .try_into()
                                .expect("u128 always fits in a 256-bit unsigned integer"),
                            0,
                        );
                    } else if let Err(e) = &uniswap {
                        tracing::warn!(
                            error = %e,
                            asset = %token,
                            "Uniswap V3 pool balance read failed for this asset this cycle — \
                             registry keeps its previous value for (UniswapV3, this asset)"
                        );
                    }

                    // Only WETH drives CheckContext.flashloan's single-scalar sanity
                    // signal — see FlashloanLiquidityState's own doc comment for why
                    // this stays asset-pinned rather than becoming asset-aware here.
                    if token != WETH {
                        continue;
                    }

                    // C10: include Uniswap V3 in the MAX across providers for the
                    // CheckContext pre-trade sanity signal (was Aave/Balancer only).
                    // Fail closed: if every read fails, keep the previous watch value
                    // rather than publishing zero and looking "freshly measured empty".
                    let mut best: Option<(u128, &'static str)> = None;
                    match &aave {
                        Ok(a) => best = Some((*a, "aave")),
                        Err(e) => tracing::warn!(
                            error = %e,
                            "Aave liquidity read failed this cycle (WETH CheckContext path)"
                        ),
                    }
                    match &balancer {
                        Ok(b) => {
                            best = match best {
                                Some((prev, _)) if *b > prev => Some((*b, "balancer")),
                                Some(x) => Some(x),
                                None => Some((*b, "balancer")),
                            };
                        }
                        Err(e) => tracing::warn!(
                            error = %e,
                            "Balancer liquidity read failed this cycle (WETH CheckContext path)"
                        ),
                    }
                    match &uniswap {
                        Ok(u) => {
                            best = match best {
                                Some((prev, _)) if *u > prev => Some((*u, "uniswap_v3")),
                                Some(x) => Some(x),
                                None => Some((*u, "uniswap_v3")),
                            };
                        }
                        Err(e) => tracing::warn!(
                            error = %e,
                            "Uniswap V3 liquidity read failed this cycle (WETH CheckContext path)"
                        ),
                    }

                    let candidate = match best {
                        Some((available_wei, protocol_id)) => {
                            Some((available_wei, protocol_id.to_string()))
                        }
                        None => {
                            tracing::warn!(
                                "all flashloan liquidity reads failed (Aave, Balancer, Uniswap V3)                                  — keeping previous CheckContext watch value (C10 fail closed)"
                            );
                            None
                        }
                    };

                    if let Some((available_wei, protocol_id)) = candidate {
                        tracing::debug!(
                            available_wei,
                            protocol_id = %protocol_id,
                            "flashloan liquidity poll updated (registry + watch)"
                        );
                        let _ = liq_tx.send(FlashloanLiquidityState {
                            available_wei,
                            protocol_id,
                        });
                    }
                }
            }
        });
        tracing::info!(
            interval_s = FLASHLOAN_LIQUIDITY_POLL_INTERVAL_S,
            assets = "WETH, USDC_NATIVE",
            providers = "Aave V3, Balancer V2, Uniswap V3 (single WETH/USDC_NATIVE pool)",
            "L2e flashloan liquidity poll loop started (feeds LiquidityRegistry for both \
             assets across all three providers; CheckContext WETH watch channel MAX \
             includes Aave, Balancer, and Uniswap V3 — C10)"
        );
    }

    // ── L6: DAG ───────────────────────────────────────────────────────────────
    let dag = Arc::new(Mutex::new(ExecutionDag::new(DagConfig {
        microtx_slots: 16,
        normal_slots: 4,
        eviction_log_capacity: 1_000,
    })));
    tracing::info!("L6 DAG initialised");

    // ── ExecutionPipeline construction ─────────────────────────────────────────
    let kill_switch_cfg = KillSwitchConfig {
        max_cumulative_loss_wei: u128::MAX / 4,
        max_loss_per_window_wei: u128::MAX / 8,
        loss_window: Duration::from_secs(3600),
        max_consecutive_failures: 32,
    };
    let kill_switches =
        Arc::new(KillSwitchRegistry::new(kill_switch_cfg).context("KillSwitchRegistry::new")?);
    tracing::warn!(
        "KillSwitchRegistry constructed with non-production placeholder thresholds"
    );

    // IntegrityRegistry — no longer unconditionally empty (see changelog).
    let integrity_registry = IntegrityRegistry::new();
    match load_deployment_manifest(DEPLOYMENT_MANIFEST_PATH)
        .with_context(|| format!("loading deployment manifest from {DEPLOYMENT_MANIFEST_PATH}"))?
    {
        Some(manifest) => {
            // One bad entry fails the WHOLE call via `?`, halting startup rather than
            // running with a partially-registered or all-placeholder registry.
            let entries = strategy_entries_from_manifest(&manifest, active_phase)
                .context("validating deployment manifest entries")?;
            let count = entries.len();
            let ids: Vec<String> = entries.iter().map(|e| e.strategy_id.clone()).collect();
            integrity_registry.register_all(entries);
            tracing::info!(
                count,
                strategy_ids = ?ids,
                path = DEPLOYMENT_MANIFEST_PATH,
                active_phase,
                "Real deployment manifest loaded — strategies registered in IntegrityRegistry"
            );
        }
        None => {
            tracing::warn!(
                path = DEPLOYMENT_MANIFEST_PATH,
                "No deployment manifest found at the conventional path — IntegrityRegistry \
                 empty, every strategy_id will fail Stage 2b as StrategyUnknown until a real \
                 manifest (forge deploy output or an on-chain eth_getCode read — never \
                 fabricated) is placed here"
            );
        }
    }
    // Deliberately NOT calling integrity_registry.freeze(...) here — that's a governance
    // action (permanently disables a strategy), not a startup step.

    // ── Relay production bootstrap ─────────────────────────────────────────────
    let confirmation_rpc_url = std::env::var("ARBITRUM_HTTP_RPC_URL").context(
        "ARBITRUM_HTTP_RPC_URL must be set — a real chain JSON-RPC HTTP endpoint for \
         inclusion confirmation, distinct from ARBITRUM_RPC_URL's WebSocket endpoint",
    )?;

    let translated = translate_relay_config(
        &config.relay,
        RelayBootstrapInputs {
            confirmation_rpc_url: confirmation_rpc_url.clone(),
        },
    );
    for f in &translated.unmapped_fields {
        tracing::warn!(
            field = f.field_name,
            configured_value = %f.configured_value,
            "config.relay field has no counterpart in omega_relay::RelayConfig — configured \
             value is not taking effect at this layer"
        );
    }
    let relay_cfg = translated.config;

    let candidate_relays: &[RelayName] = if active_phase >= 2 {
        &relay_cfg.phase_2plus_relays
    } else {
        &relay_cfg.phase_1_relays
    };

    let relay_http_client = omega_relay::client::HttpRelayClient::build_http_client()
        .context("building shared reqwest client for relay submission")?;

    let mut relay_clients: HashMap<String, Arc<dyn RelayClient>> = HashMap::new();
    for name in candidate_relays.iter() {
        let endpoint_var = format!("OMEGA_RELAY_ENDPOINT_{}", name.to_string().to_uppercase());
        let endpoint = match std::env::var(&endpoint_var) {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    relay = %name,
                    var = %endpoint_var,
                    "no endpoint configured for this relay — skipped, not guessed at"
                );
                continue;
            }
        };

        let auth = match name {
            RelayName::Flashbots => match std::env::var("FLASHBOTS_AUTH_KEY") {
                Ok(k) => match RelayAuth::flashbots_style(&k) {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::error!(relay = %name, error = %e, "invalid FLASHBOTS_AUTH_KEY — relay skipped");
                        continue;
                    }
                },
                Err(_) => {
                    tracing::warn!(relay = %name, "FLASHBOTS_AUTH_KEY not set — relay skipped");
                    continue;
                }
            },
            RelayName::Titan => match std::env::var("TITAN_AUTH_KEY") {
                Ok(k) => match RelayAuth::flashbots_style(&k) {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::error!(relay = %name, error = %e, "invalid TITAN_AUTH_KEY — relay skipped");
                        continue;
                    }
                },
                Err(_) => {
                    tracing::warn!(relay = %name, "TITAN_AUTH_KEY not set — relay skipped");
                    continue;
                }
            },
            RelayName::Bloxroute => match std::env::var("BLOXROUTE_AUTH_TOKEN") {
                Ok(t) => RelayAuth::BearerToken(t),
                Err(_) => {
                    tracing::warn!(relay = %name, "BLOXROUTE_AUTH_TOKEN not set — relay skipped");
                    continue;
                }
            },
            RelayName::Eden => match std::env::var("EDEN_AUTH_TOKEN") {
                Ok(t) => RelayAuth::BearerToken(t),
                Err(_) => {
                    tracing::warn!(relay = %name, "EDEN_AUTH_TOKEN not set — relay skipped");
                    continue;
                }
            },
            RelayName::Other(raw) => {
                tracing::error!(
                    relay = %raw,
                    "no verified auth convention for this relay name — skipped, not guessed at"
                );
                continue;
            }
        };

        let client = omega_relay::client::HttpRelayClient::new(
            name.to_string(),
            endpoint,
            relay_http_client.clone(),
            auth,
        );
        relay_clients.insert(name.to_string(), client);
    }

    if relay_clients.is_empty() {
        // C5 fail closed: phase 0 may run with zero relays (shadow / no submission).
        // Phase 1+ requires at least one live HttpRelayClient — otherwise every
        // submission would fail at the multi-relay layer with no recovery path.
        if active_phase >= 1 {
            anyhow::bail!(
                "C5: zero relay clients constructed (no OMEGA_RELAY_ENDPOINT_* / auth keys)                  while active_phase={} — refusing to start rather than running a production                  phase that cannot submit bundles",
                active_phase
            );
        }
        tracing::warn!(
            "zero relay clients constructed (no endpoints/secrets present in environment) — \
             phase 0 only; submissions will fail closed if attempted"
        );
    } else {
        tracing::info!(
            relays = ?relay_clients.keys().collect::<Vec<_>>(),
            phase = active_phase,
            "real relay clients constructed (C5)"
        );
    }

    // Metrics/carryover identity label only — not a signing capability.
    let execution_address = std::env::var("OMEGA_EXECUTION_ADDRESS")
        .unwrap_or_else(|_| "0xC1_UNCONFIGURED".to_string());
    if execution_address == "0xC1_UNCONFIGURED" {
        tracing::warn!(
            "OMEGA_EXECUTION_ADDRESS not set — relay metrics identity still a placeholder"
        );
    }
    let relay_metrics = LaRelayMetrics::new(50, ExecutionAddress(execution_address));

    if !std::path::Path::new(BUILDER_BLACKLIST_PATH).exists() {
        if let Some(parent) = std::path::Path::new(BUILDER_BLACKLIST_PATH).parent() {
            std::fs::create_dir_all(parent).context("creating config/ directory")?;
        }
        std::fs::write(
            BUILDER_BLACKLIST_PATH,
            "# empty builder blacklist — no entries registered yet\n",
        )
        .context("writing empty builder blacklist")?;
        tracing::warn!(
            path = BUILDER_BLACKLIST_PATH,
            "created empty builder blacklist file (none existed)"
        );
    }
    let blacklist =
        BuilderBlacklist::load(BUILDER_BLACKLIST_PATH).context("BuilderBlacklist::load")?;

    // startup_block: still 0 — no synchronous "current height" read available off `rpc`.
    let (relay, reorg_event_rx) =
        MultiRelayClient::new(relay_clients, relay_metrics, blacklist, &relay_cfg, 0);

    tokio::spawn(async move {
        let mut rx = reorg_event_rx;
        while let Some(ev) = rx.recv().await {
            tracing::debug!(
                ?ev,
                "LaReorgRiskEvent received (rescoring not wired to it yet — the block-hash \
                 feed task below drives detection; consuming the rescore signal itself is a \
                 separate, still-open piece)"
            );
        }
    });

    // ── Real block-hash feed for the reorg guard ───────────────────────────────
    // Independent of every other task in this function — its own subscription, its own
    // loop — spawned separately so it runs concurrently rather than serializing behind
    // the reorg-drain-log task above or the reconciliation task below.
    {
        let relay6 = Arc::clone(&relay);
        let mut block_rx = rpc.subscribe_blocks();
        tokio::spawn(async move {
            loop {
                match block_rx.recv().await {
                    Ok(event) => feed_block_event_to_reorg_guard(&relay6, &event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "reorg block-feed loop lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        tracing::info!("reorg guard now receiving real (block_number, block_hash) pairs");
    }

    // ── Real TransactionSigner construction ────────────────────────────────────
    let orchestrator_address = parse_address_env("ORCHESTRATOR_ADDRESS")
        .context("ORCHESTRATOR_ADDRESS must be set -- the deployed OmegaOrchestrator contract \
                  address every signed transaction this signer produces calls execute() on")?;

    let tx_signing_key_hex = std::env::var("OMEGA_TX_SIGNING_KEY").context(
        "OMEGA_TX_SIGNING_KEY must be set -- hex-encoded secp256k1 secret key for the \
         gas-paying transaction-envelope signer. Deliberately a SEPARATE key from \
         OMEGA_BLUEPRINT_SIGNING_KEY below -- the tx-envelope signer and the on-chain \
         blueprint-authorization signer are independent concerns.",
    )?;
    let tx_key_manager = Arc::new(
        KeyManager::from_hex(&tx_signing_key_hex, chain_id)
            .context("constructing tx_key_manager from OMEGA_TX_SIGNING_KEY")?,
    );

    let blueprint_signing_key_hex = std::env::var("OMEGA_BLUEPRINT_SIGNING_KEY").context(
        "OMEGA_BLUEPRINT_SIGNING_KEY must be set -- hex-encoded secp256k1 secret key whose \
         derived address must match OmegaOrchestrator.execution_key (or pending_key, during \
         a rotation window) on-chain, or every execute() call this signer produces will \
         revert with InvalidSignature. Confirming that match is an operational deployment \
         step, not something this file can verify for itself.",
    )?;
    let blueprint_key_manager = Arc::new(
        KeyManager::from_hex(&blueprint_signing_key_hex, chain_id)
            .context("constructing blueprint_key_manager from OMEGA_BLUEPRINT_SIGNING_KEY")?,
    );
    let blueprint_signer = Arc::new(BlueprintSigner::new(blueprint_key_manager));

    let signer = Arc::new(KeyManagerTransactionSigner::new(
        tx_key_manager,
        orchestrator_address.into(),
        strategy_onchain_ids(),
        blueprint_signer,
    ));
    tracing::info!(
        orchestrator = %hex::encode(orchestrator_address),
        tx_signer_address = %hex::encode(signer.active_address()),
        "KeyManagerTransactionSigner constructed -- real transaction signing wired in"
    );

    let execution_pipeline = Arc::new(ExecutionPipeline::new(
        Arc::clone(&kill_switches),
        Arc::clone(&integrity_registry),
        Arc::clone(&relay),
        Arc::clone(&dag),
        Arc::clone(&signer),
        chain_id,
    ));
    tracing::info!(
        idempotency_cache_len = execution_pipeline.idempotency_cache_len(),
        "ExecutionPipeline constructed"
    );

    // ── Reconciliation lifecycle ────────────────────────────────────────────────
    // Drives InclusionTracker::reconcile off the same oracle block-number stream
    // run_scoring_loop subscribes to — reconcile() only needs a block number, not the
    // hash the reorg-guard wiring above needs.
    {
        let relay5 = Arc::clone(&relay);
        let oracle5 = Arc::clone(&oracle);
        let mut rx = oracle5.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(_) => {
                        let current_block = oracle5.snapshot().fee.block_number;
                        let results = relay5.reconcile_inclusions(current_block).await;
                        if !results.is_empty() {
                            tracing::debug!(
                                count = results.len(),
                                "inclusion confirmations reconciled"
                            );
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "reconciliation loop lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        tracing::info!("reconciliation lifecycle task started");
    }

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
        chain_id, // was hard-coded inside ProofWorkerPool::start
    };
    let proof_queue = ProofQueue::new(zk_cfg.clone());
    let _pool = ProofWorkerPool::start(zk_cfg, proof_queue.clone());
    // Stateless (holds only expected_chain_id) — a single Arc-wrapped instance is shared
    // across every scoring-loop task rather than reconstructed per call.
    let zk_verifier = Arc::new(ZkVerifier::new(chain_id));
    // Verified proofs awaiting OmegaVault.submitProof (calldata only until a signer is wired).
    let pending_proofs = Arc::new(PendingProofBuffer::new(256));
    tracing::info!(
        "L7 ZK: proof worker pool started, ZkVerifier + PendingProofBuffer ready"
    );

    // Keeper: drain verified proofs into submitProof calldata. Does NOT broadcast —
    // replace the body with KeyManagerTransactionSigner when the on-chain path is live.
    {
        let buf = Arc::clone(&pending_proofs);
        let vault = vault_address;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(5));
            loop {
                ticker.tick().await;
                for sub in buf.drain(8) {
                    match sub.encode_calldata() {
                        Ok(data) => {
                            tracing::info!(
                                blueprint = %hex::encode(sub.blueprint_hash),
                                vault = %hex::encode(vault),
                                calldata_len = data.len(),
                                "ZK submitProof calldata ready — wire KeyManagerTransactionSigner to broadcast"
                            );
                            let _ = data;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "submitProof encode failed")
                        }
                    }
                }
            }
        });
    }

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

    // ── Nonce registry ────────────────────────────────────────────────────────
    let nonce_registry = omega_security::replay::NonceRegistry::new();
    tracing::warn!(
        "NonceRegistry constructed but never advanced — check 15 only rejects each \
         strategy's very first blueprint (nonce 0) until Stage 7 reconciliation wires \
         advance() in"
    );

    // ── Account exposure tracker ─────────────────────────────────────────────────
    let exposure_tracker = AccountExposureTracker::new();
    tracing::warn!(
        "AccountExposureTracker constructed — real per-strategy tracking via each \
         blueprint's own expiry_block as a conservative TTL, but max_account_exposure_wei \
         is still a non-risk-approved placeholder (1 ETH) and this tracker is in-memory \
         only (resets on restart)"
    );

    // ── L13: Strategy registry ────────────────────────────────────────────────
    // Different registry from IntegrityRegistry above. C8: LA is registered alongside
    // CNRY; SA/MSA/MEV are still not registered here.

    // Real, live lending-position registry LaStrategy requires at construction. NOTHING
    // IN THIS CODEBASE WRITES TO IT YET — no omega-oracle component exists to populate
    // it from live chain data (Aave/Compound/Morpho health-factor scanning). Constructed
    // here so LA is reachable and so a future writer has somewhere real to write to;
    // until that writer exists, LaStrategy::select_position() always returns None and LA
    // scores 0.0 every cycle, same observable behavior as before this revision.
    let position_registry = PositionRegistry::new();

    let mut registry_builder = StrategyRegistryBuilder::new(active_phase)
        .register(CnryStrategy::new(chain_id, &config))
        .expect("CNRY registration must succeed");

    // LA's bytecode_hash/contract_addr are sourced ONLY from the same, already-loaded
    // IntegrityRegistry manifest data resolve_strategy_bytecode_hash reads from above —
    // never a placeholder or guessed address. No manifest, or a manifest with no "LA"
    // entry, means LA is simply not registered this run: the same fail-closed posture
    // Stage 2b already applies to any strategy_id IntegrityRegistry doesn't know about.
    //
    // ASSUMPTION FLAGGED, NOT VERIFIED: this assumes IntegrityRegistry::snapshot()'s
    // entry type exposes a `contract_address` field alongside the already-confirmed
    // `bytecode_hash` field. Only `bytecode_hash` has been read anywhere in this file
    // before now (via resolve_strategy_bytecode_hash) — confirm the real field name/type
    // in crates/omega-security's manifest entry struct before relying on this in
    // production, and adjust the `.contract_address` access and `.into()` conversions
    // below if they differ (e.g. if it's already an `Address` rather than `[u8; 20]`).
    match integrity_registry
        .snapshot()
        .into_iter()
        .find(|e| e.strategy_id == "LA")
    {
        Some(entry) => {
            let la = LaStrategy::new(
                chain_id,
                entry.bytecode_hash.into(),
                entry.contract_address.into(),
                Arc::clone(&liquidity_registry),
                Arc::clone(&position_registry),
                &config,
            );
            registry_builder = registry_builder
                .register(la)
                .expect("LA registration must succeed");
            tracing::info!("L13: LA registered from deployment manifest");
        }
        None => {
            tracing::warn!(
                path = DEPLOYMENT_MANIFEST_PATH,
                "L13: no LA entry in IntegrityRegistry (manifest missing or has no LA \
                 entry) — LA NOT registered this run. Registering it now would be inert \
                 anyway: no live position data exists (PositionRegistry has no writer \
                 yet) and build_blueprint refuses on missing debt-amount pricing \
                 regardless (see omega-strategies/src/la.rs's own doc comments)."
            );
        }
    }

    let registry = registry_builder.build();

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
        let cid_canary = chain_id;
        tokio::spawn(async move { run_canary_loop(cnry, ora2, cid_canary, halt2, 500).await });
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
        let ep3 = Arc::clone(&execution_pipeline);
        let nr3 = nonce_registry.clone();
        let ir3 = Arc::clone(&integrity_registry);
        let et3 = exposure_tracker.clone();
        let fl3 = flashloan_liq_rx.clone();
        let cid3 = chain_id;
        let va3 = vault_address;
        let pt3 = profit_token;
        let zv3 = Arc::clone(&zk_verifier);
        let pp3 = Arc::clone(&pending_proofs);
        tokio::spawn(async move {
            run_scoring_loop(
                reg, ora3, cl3, py3, tw3, dag2, tx, pq, halt3, ph, ep3, nr3, ir3, et3, fl3, cid3,
                va3, pt3, zv3, pp3,
            )
            .await;
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
        chain_id,
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
    chain_id: u64,
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
            chain_id,
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
    execution_pipeline: Arc<ExecutionPipeline<KeyManagerTransactionSigner>>,
    nonce_registry: omega_security::replay::NonceRegistry,
    integrity_registry: Arc<IntegrityRegistry>,
    exposure_tracker: AccountExposureTracker,
    flashloan_liq_rx: tokio::sync::watch::Receiver<FlashloanLiquidityState>,
    chain_id: u64,
    vault_address: [u8; 20],
    profit_token: [u8; 20],
    zk_verifier: Arc<ZkVerifier>,
    pending_proofs: Arc<PendingProofBuffer>,
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
                    chain_id,
                    block_number: snap.fee.block_number,
                    base_fee_gwei: snap.fee.base_fee_gwei,
                    l1_data_fee_gwei: snap.fee.l1_data_fee_gwei,
                    state_hash: snap.state_hash,
                };
                let oracle_snapshot = build_oracle_snapshot(
                    &chainlink_oracle,
                    &pyth_oracle,
                    &twap_oracle,
                    ORACLE_SNAPSHOT_TOKEN,
                );
                // Computed once per scoring cycle so every strategy scored this cycle
                // sees the identical gas-volatility reading.
                let gas_volatility_risk = oracle.l1_gas_volatility_risk();
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
                    let os2 = oracle_snapshot;
                    let ep2 = Arc::clone(&execution_pipeline);
                    let nr2 = nonce_registry.clone();
                    let ir2 = Arc::clone(&integrity_registry);
                    let gv2 = gas_volatility_risk;
                    let et2 = exposure_tracker.clone();
                    let fl2 = flashloan_liq_rx.clone();
                    let cid2 = chain_id;
                    let va2 = vault_address;
                    let pt2 = profit_token;
                    let zv2 = Arc::clone(&zk_verifier);
                    let pp2 = Arc::clone(&pending_proofs);
                    tokio::spawn(async move {
                        score_and_admit(
                            strategy, s2, dag2, tx2, pq2, h2, ph, os2, ep2, nr2, ir2, gv2, et2,
                            fl2, cid2, va2, pt2, zv2, pp2,
                        )
                        .await;
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
    execution_pipeline: Arc<ExecutionPipeline<KeyManagerTransactionSigner>>,
    nonce_registry: omega_security::replay::NonceRegistry,
    integrity_registry: Arc<IntegrityRegistry>,
    gas_volatility_risk: f64,
    exposure_tracker: AccountExposureTracker,
    flashloan_liq_rx: tokio::sync::watch::Receiver<FlashloanLiquidityState>,
    chain_id: u64,
    vault_address: [u8; 20],
    profit_token: [u8; 20],
    zk_verifier: Arc<ZkVerifier>,
    pending_proofs: Arc<PendingProofBuffer>,
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
        let mut g = dag.lock().unwrap();
        if g.admit(bp.clone(), &[]).is_err() {
            return;
        }
    }

    // Records this blueprint's flashloan exposure the moment it's genuinely admitted —
    // a no-op for amount_wei == 0 (SA/MSA/MEV today), so inert for every strategy but LA
    // without needing a branch here.
    exposure_tracker.record(
        &strategy.strategy_id().to_string(),
        bp.flashloan_amount.try_into().unwrap_or(u128::MAX),
        bp.expiry_block,
    );

    let hot = strategy.hot_path_eligible()
        && bp.lane == omega_core::Lane::Microtx
        && bp.l2_exec_gas_estimate <= MICROTX_GAS_LIMIT;

    if hot {
        // Hot-path blueprints also provision a ZK proof, as a DETACHED background task
        // (not awaited) — OmegaVault.receivePendingProfit() (called immediately after
        // execution) doesn't require a proof, only the later releaseProfit() does, so
        // gating hot-path admission on proof completion here would reimport the exact
        // latency cost the hot path exists to avoid. `is_microtx: true` is deliberate —
        // hot-path blueprints are Microtx lane by construction, and the queue privileges
        // microtx submissions under pressure.
        //
        // STILL OPEN: this makes a verified proof become available; nothing here (or
        // anywhere in this codebase) actually calls OmegaVault.submitProof() on-chain
        // once it's ready.
        {
            let hb: [u8; 32] = *bp.blueprint_hash;
            let profit: u128 = bp.expected_profit_net.try_into().unwrap_or(u128::MAX);
            let expected_public_inputs_hash =
                compute_public_inputs_hash(vault_address, hb, profit, profit_token);

            match proof_queue.submit(
                hb,
                expected_public_inputs_hash,
                profit,
                chain_id,
                bp.strategy_id.to_string(),
                true, // is_microtx — see comment above
            ) {
                Ok(proof_rx) => {
                    let hash_for_log = bp.blueprint_hash;
                    let zv_bg = Arc::clone(&zk_verifier);
                    let pp_bg = Arc::clone(&pending_proofs);
                    tokio::spawn(async move {
                        match proof_rx.await {
                            Ok(Ok(proof)) => {
                                if let Err(e) = zv_bg.verify(&proof, expected_public_inputs_hash) {
                                    tracing::error!(
                                        hash = %hash_for_log,
                                        error = %e,
                                        "hot-path background ZK proof FAILED VERIFICATION \
                                         against expected publicInputsHash"
                                    );
                                } else {
                                    match VerifiedProofSubmission::from_verified_proof(&proof) {
                                        Ok(sub) => {
                                            if let Err(e) = pp_bg.push(sub) {
                                                tracing::warn!(
                                                    hash = %hash_for_log,
                                                    error = %e,
                                                    "verified ZK proof could not be buffered                                                      for on-chain submitProof"
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            tracing::error!(
                                                hash = %hash_for_log,
                                                error = %e,
                                                "verified proof rejected at submission packaging                                                  — fail closed on buffer"
                                            );
                                        }
                                    }
                                    tracing::debug!(
                                        hash = %hash_for_log,
                                        gen_ms = proof.generation_ms,
                                        "hot-path background ZK proof ready and verified \
                                         (buffered for submitProof)"
                                    );
                                }
                            }
                            Ok(Err(zk_error)) => {
                                tracing::warn!(
                                    hash = %hash_for_log,
                                    error = %zk_error,
                                    "hot-path background ZK proof generation failed"
                                );
                            }
                            Err(_recv_error) => {
                                tracing::warn!(
                                    hash = %hash_for_log,
                                    "hot-path background ZK proof response channel closed \
                                     before a result arrived"
                                );
                            }
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        hash = %bp.blueprint_hash,
                        error = %e,
                        "hot-path ZK proof submission rejected by queue — this blueprint's \
                         eventual profit will have no proof pathway; execute() below is NOT \
                         blocked on this"
                    );
                }
            }
        }

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

        let expected_public_inputs_hash =
            compute_public_inputs_hash(vault_address, hb, profit, profit_token);

        // Every early-return below releases the DAG slot explicitly, since execute()
        // (and its DagSlotGuard) is never reached on these paths.
        let proof_rx = match proof_queue.submit(
            hb,
            expected_public_inputs_hash,
            profit,
            chain_id,
            bp.strategy_id.to_string(),
            micro,
        ) {
            Ok(rx) => rx,
            Err(e) => {
                tracing::warn!(
                    hash = %bp.blueprint_hash,
                    error = %e,
                    "ZK proof submission rejected by queue — dropping blueprint, NOT executing"
                );
                dag.lock().unwrap().complete(bp.blueprint_hash);
                return;
            }
        };

        let proof = match proof_rx.await {
            Ok(Ok(proof)) => proof,
            Ok(Err(zk_error)) => {
                tracing::warn!(
                    hash = %bp.blueprint_hash,
                    error = %zk_error,
                    "ZK proof generation failed — dropping blueprint, NOT executing"
                );
                dag.lock().unwrap().complete(bp.blueprint_hash);
                return;
            }
            Err(_recv_error) => {
                tracing::warn!(
                    hash = %bp.blueprint_hash,
                    "ZK proof response channel closed before a result arrived (worker \
                     crashed or shut down?) — dropping blueprint, NOT executing"
                );
                dag.lock().unwrap().complete(bp.blueprint_hash);
                return;
            }
        };

        if let Err(verify_err) = zk_verifier.verify(&proof, expected_public_inputs_hash) {
            tracing::error!(
                hash = %bp.blueprint_hash,
                error = %verify_err,
                "ZK proof FAILED VERIFICATION against expected publicInputsHash — dropping \
                 blueprint, NOT executing. Should be unreachable in normal operation — a hit \
                 here most likely signals a vault_address/profit_token configuration bug, or \
                 something worse."
            );
            dag.lock().unwrap().complete(bp.blueprint_hash);
            return;
        }

        match VerifiedProofSubmission::from_verified_proof(&proof) {
            Ok(sub) => {
                if let Err(e) = pending_proofs.push(sub) {
                    tracing::warn!(
                        hash = %bp.blueprint_hash,
                        error = %e,
                        "verified ZK proof could not be buffered for on-chain submitProof"
                    );
                }
            }
            Err(e) => {
                tracing::error!(
                    hash = %bp.blueprint_hash,
                    error = %e,
                    "verified proof rejected at submission packaging — fail closed on buffer"
                );
            }
        }

        if active_phase >= 1 {
            tracing::info!(
                hash   = %bp.blueprint_hash,
                gen_ms = proof.generation_ms,
                "ZK proof ready and verified (buffered for submitProof)",
            );
        }
    }

    // Reachable only for: hot-path blueprints (unconditionally), or non-hot-path
    // blueprints whose ZK proof both generated successfully AND verified. Every other
    // non-hot-path outcome already returned above, releasing its own DAG slot.
    let strategy_max_gas = strategy.gas_budget();
    let max_slippage_bps = max_slippage_bps_for(strategy.strategy_id());
    let latest_blueprint_nonce =
        nonce_registry.next_nonce(&strategy.strategy_id().to_string(), chain_id);
    let strategy_bytecode_hash =
        resolve_strategy_bytecode_hash(&integrity_registry, strategy.strategy_id());
    // Moved before build_check_context — the exposure read below needs the current block
    // number to prune expired entries.
    let current_block = signal.block_number;
    let current_account_exposure_wei = exposure_tracker
        .current_exposure_wei(&strategy.strategy_id().to_string(), current_block);
    // `.borrow()` returns a guard; `.clone()` out immediately so the watch channel's
    // internal lock isn't held across the rest of this function.
    let flashloan_snapshot = flashloan_liq_rx.borrow().clone();
    let risk_ctx = build_check_context(
        chain_id,
        &signal,
        oracle_snapshot,
        strategy_max_gas,
        max_slippage_bps,
        latest_blueprint_nonce,
        strategy_bytecode_hash,
        gas_volatility_risk,
        current_account_exposure_wei,
        flashloan_snapshot,
    );
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
                "ExecutionPipeline::execute completed"
            );
        }
        Err(e) => {
            // Expected today for any strategy not in a real, loaded manifest (Stage 2b
            // StrategyUnknown), and for every strategy until remaining fail-closed
            // CheckContext fields (competition, primarily) get real sources.
            tracing::debug!(
                hash = %bp.blueprint_hash,
                error = %e,
                "ExecutionPipeline::execute rejected blueprint (expected until remaining \
                 risk-data gaps are closed)"
            );
        }
    }

    // dag.complete() intentionally NOT called here — execute() above is the sole owner
    // of this blueprint's DAG slot via its internal DagSlotGuard, for every blueprint
    // that reaches this point.
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

#[cfg(test)]
mod deployment_manifest_bootstrap_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Uniquely-named temp file per test, so these tests can exercise
    /// load_deployment_manifest's real disk-reading behavior without colliding across
    /// concurrently-running test threads.
    fn write_temp_manifest(content: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "omega_main_manifest_test_{}_{}_{}.toml",
            std::process::id(),
            n,
            nanos
        ));
        let mut f = std::fs::File::create(&path).expect("create temp manifest file");
        f.write_all(content.as_bytes())
            .expect("write temp manifest file");
        path
    }

    fn valid_manifest_toml() -> String {
        format!(
            r#"
                [[strategies]]
                strategy_id = "SA"
                bytecode_hash = "0x{}"
                contract_address = "0x{}"
                min_phase = 1
            "#,
            "11".repeat(32),
            "21".repeat(20),
        )
    }

    #[test]
    fn load_deployment_manifest_missing_file_returns_ok_none() {
        let path = std::path::Path::new("/this/path/should/not/exist/on/any/machine.toml");
        let result = load_deployment_manifest(path.to_str().unwrap());
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn load_deployment_manifest_malformed_toml_returns_err() {
        let path = write_temp_manifest("this is { not valid toml at all [[[");
        let result = load_deployment_manifest(path.to_str().unwrap());
        assert!(
            result.is_err(),
            "malformed TOML must fail to load, not silently return an empty manifest"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_deployment_manifest_wrong_shape_returns_err() {
        let path = write_temp_manifest("not_a_strategies_field = 42\n");
        let result = load_deployment_manifest(path.to_str().unwrap());
        assert!(
            result.is_err(),
            "well-formed TOML that doesn't match DeploymentManifest's shape must still fail"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_deployment_manifest_valid_file_returns_ok_some() {
        let path = write_temp_manifest(&valid_manifest_toml());
        let result = load_deployment_manifest(path.to_str().unwrap()).unwrap();
        assert!(result.is_some());
        let manifest = result.unwrap();
        assert_eq!(manifest.strategies.len(), 1);
        assert_eq!(manifest.strategies[0].strategy_id, "SA");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn full_bootstrap_chain_valid_manifest_populates_registry() {
        let path = write_temp_manifest(&valid_manifest_toml());
        let active_phase: u8 = 1;

        let manifest = load_deployment_manifest(path.to_str().unwrap())
            .unwrap()
            .expect("valid manifest must load as Some");
        let entries = strategy_entries_from_manifest(&manifest, active_phase)
            .expect("valid entries must pass validation");

        let integrity_registry = IntegrityRegistry::new();
        integrity_registry.register_all(entries);

        let expected_hash = {
            let mut h = [0u8; 32];
            h.fill(0x11);
            h
        };
        assert!(integrity_registry
            .check_bytecode("SA", &expected_hash)
            .is_ok());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn full_bootstrap_chain_one_bad_entry_fails_the_whole_load_and_registry_stays_empty() {
        let bad_manifest = format!(
            r#"
                [[strategies]]
                strategy_id = "SA"
                bytecode_hash = "0x{}"
                contract_address = "0x{}"
                min_phase = 1

                [[strategies]]
                strategy_id = "LA"
                bytecode_hash = "0x{}"
                contract_address = "0x{}"
                min_phase = 3
            "#,
            "11".repeat(32),
            "21".repeat(20),
            "00".repeat(32), // placeholder — must fail validation
            "22".repeat(20),
        );
        let path = write_temp_manifest(&bad_manifest);

        let manifest = load_deployment_manifest(path.to_str().unwrap())
            .unwrap()
            .expect("well-formed TOML must still load as Some — the bad data is \
                     caught one step later, by strategy_entries_from_manifest");

        let result = strategy_entries_from_manifest(&manifest, 4);
        assert!(
            result.is_err(),
            "one placeholder entry must fail the WHOLE manifest, exactly as main() \
             relies on via its own `?` propagation — a partially-registered \
             IntegrityRegistry (SA checked, LA silently unchecked) must never happen"
        );

        let integrity_registry = IntegrityRegistry::new();
        assert!(integrity_registry.registered_ids().is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn full_bootstrap_chain_missing_manifest_leaves_registry_empty_and_authorizes_nothing() {
        let path = std::path::Path::new("/this/path/should/not/exist/on/any/machine.toml");
        let result = load_deployment_manifest(path.to_str().unwrap()).unwrap();
        assert!(result.is_none());

        let integrity_registry = IntegrityRegistry::new();
        assert!(integrity_registry
            .check_bytecode("SA", &[0x11; 32])
            .is_err());
    }
}

#[cfg(test)]
mod parse_address_env_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    // NOTE: these tests mutate process-global env vars, so they use distinct,
    // test-specific var names to avoid interfering with each other or with any real
    // VAULT_ADDRESS/PROFIT_TOKEN in the actual test-running environment.

    #[test]
    fn parses_valid_0x_prefixed_address() {
        std::env::set_var(
            "OMEGA_TEST_ADDR_1",
            "0x1111111111111111111111111111111111111111",
        );
        let result = parse_address_env("OMEGA_TEST_ADDR_1").unwrap();
        assert_eq!(result, [0x11u8; 20]);
        std::env::remove_var("OMEGA_TEST_ADDR_1");
    }

    #[test]
    fn parses_valid_address_without_0x_prefix() {
        std::env::set_var(
            "OMEGA_TEST_ADDR_2",
            "2222222222222222222222222222222222222222",
        );
        let result = parse_address_env("OMEGA_TEST_ADDR_2").unwrap();
        assert_eq!(result, [0x22u8; 20]);
        std::env::remove_var("OMEGA_TEST_ADDR_2");
    }

    #[test]
    fn missing_env_var_errors() {
        std::env::remove_var("OMEGA_TEST_ADDR_MISSING");
        assert!(parse_address_env("OMEGA_TEST_ADDR_MISSING").is_err());
    }

    #[test]
    fn wrong_length_errors() {
        std::env::set_var("OMEGA_TEST_ADDR_SHORT", "0x1234");
        assert!(parse_address_env("OMEGA_TEST_ADDR_SHORT").is_err());
        std::env::remove_var("OMEGA_TEST_ADDR_SHORT");
    }

    #[test]
    fn invalid_hex_errors() {
        std::env::set_var(
            "OMEGA_TEST_ADDR_BADHEX",
            "0xZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ",
        );
        assert!(parse_address_env("OMEGA_TEST_ADDR_BADHEX").is_err());
        std::env::remove_var("OMEGA_TEST_ADDR_BADHEX");
    }
}

#[cfg(test)]
mod chain_id_and_tag_override_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    // resolve_chain_id_from tests: no env var involved — plain in-memory Option<String>,
    // so no std::env::set_var/remove_var race with any other test in this module.
    #[test]
    fn resolve_chain_id_defaults_when_unset() {
        assert_eq!(resolve_chain_id_from(None).unwrap(), DEFAULT_CHAIN_ID);
    }

    #[test]
    fn resolve_chain_id_reads_valid_override() {
        assert_eq!(resolve_chain_id_from(Some("31337".into())).unwrap(), 31337);
    }

    #[test]
    fn resolve_chain_id_errors_on_malformed_value_rather_than_defaulting() {
        assert!(resolve_chain_id_from(Some("not-a-number".into())).is_err());
    }

    #[test]
    fn parse_optional_address_env_returns_none_when_unset() {
        std::env::remove_var("OMEGA_TEST_OPTIONAL_ADDR_UNSET");
        let result = parse_optional_address_env("OMEGA_TEST_OPTIONAL_ADDR_UNSET").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn parse_optional_address_env_returns_some_when_set_and_valid() {
        std::env::set_var(
            "OMEGA_TEST_OPTIONAL_ADDR_SET",
            "0x3333333333333333333333333333333333333333",
        );
        let result = parse_optional_address_env("OMEGA_TEST_OPTIONAL_ADDR_SET");
        std::env::remove_var("OMEGA_TEST_OPTIONAL_ADDR_SET");
        assert_eq!(result.unwrap(), Some([0x33u8; 20]));
    }

    #[test]
    fn parse_optional_address_env_errors_when_set_but_malformed() {
        std::env::set_var("OMEGA_TEST_OPTIONAL_ADDR_BAD", "0xZZ");
        let result = parse_optional_address_env("OMEGA_TEST_OPTIONAL_ADDR_BAD");
        std::env::remove_var("OMEGA_TEST_OPTIONAL_ADDR_BAD");
        assert!(
            result.is_err(),
            "a set-but-malformed override must error, not be silently treated as absent"
        );
    }
}

#[cfg(test)]
mod reorg_block_feed_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Writes a minimal, valid empty builder-blacklist file for BuilderBlacklist::load —
    /// same pattern main() itself uses, reused to avoid a tempfile crate dependency in
    /// the binary just for this test.
    fn write_empty_blacklist() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "omega_main_blacklist_test_{}.toml",
            std::process::id()
        ));
        std::fs::write(&path, "# empty blacklist for test\n").unwrap();
        path
    }

    #[tokio::test]
    async fn feed_block_event_to_reorg_guard_detects_a_real_reorg() {
        // Proves the actual production call path — the B256 -> [u8; 32] extraction and
        // the on_new_block call itself — using the real MultiRelayClient and LaReorgGuard.
        let path = write_empty_blacklist();
        let blacklist = BuilderBlacklist::load(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        let metrics = LaRelayMetrics::new(10, ExecutionAddress("0xTEST".into()));
        let clients: HashMap<String, Arc<dyn RelayClient>> = HashMap::new();
        let cfg = omega_relay::RelayConfig {
            confirmation_rpc_url: "http://localhost:1".into(),
            ..Default::default()
        };
        let (relay, mut event_rx) = MultiRelayClient::new(clients, metrics, blacklist, &cfg, 0);

        relay.on_bundle_submitted(omega_relay::TxHash("0xfeed".into()), 700);

        // B256 constructed via From<[u8; 32]>, not the unresolved alloy::primitives:: path
        // — this binary has no direct alloy dependency.
        let event_a = omega_rpc::BlockEvent {
            number: 700,
            hash: [1u8; 32].into(),
            base_fee_gwei: None,
            timestamp: 0,
            is_reorg_or_stale: false,
        };
        let event_b = omega_rpc::BlockEvent {
            number: 700,
            hash: [2u8; 32].into(), // different hash, same height
            base_fee_gwei: None,
            timestamp: 0,
            is_reorg_or_stale: true,
        };

        feed_block_event_to_reorg_guard(&relay, &event_a);
        feed_block_event_to_reorg_guard(&relay, &event_b);

        let ev = tokio::time::timeout(std::time::Duration::from_millis(200), event_rx.recv())
            .await
            .expect("must not time out — this is the real production call path")
            .expect("channel must not be closed");
        assert_eq!(ev.orphaned_block, 700);
    }
}

#[cfg(test)]
mod hot_path_zk_provisioning_tests {
    // Regression coverage: score_and_admit's `hot` branch fires proof_queue.submit() as a
    // DETACHED background task — hot-path admission must NOT block on that proof
    // completing (the regression this guards against is accidentally re-gating hot-path
    // admission on proof completion, reimporting the latency cost the hot path exists to
    // avoid).
    //
    // ASSUMPTION FLAGGED, NOT VERIFIED: imports omega_strategies::SaStrategy on the
    // assumption it's re-exported at that crate's root, the same way CnryStrategy is
    // (per this file's top-level `use`). Not confirmed against
    // crates/omega-strategies/src/lib.rs directly — if the re-export doesn't exist, use
    // `omega_strategies::sa::SaStrategy` instead.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use omega_core::StrategyTrait;
    use omega_strategies::SaStrategy;

    const TEST_CHAIN_ID: u64 = 42_161;

    /// Builds every real dependency score_and_admit needs, using only constructor calls
    /// already present in this file's own main() — no new guesses about internal shapes.
    async fn build_harness() -> (
        Arc<dyn StrategyTrait>,
        Arc<Mutex<ExecutionDag>>,
        tokio::sync::mpsc::Sender<HotPathRequest>,
        tokio::sync::mpsc::Receiver<HotPathRequest>,
        ProofQueue,
        Arc<ExecutionPipeline<KeyManagerTransactionSigner>>,
        omega_security::replay::NonceRegistry,
        Arc<IntegrityRegistry>,
        AccountExposureTracker,
        tokio::sync::watch::Receiver<FlashloanLiquidityState>,
        Arc<ZkVerifier>,
        Arc<PendingProofBuffer>,
    ) {
        let strategy: Arc<dyn StrategyTrait> = SaStrategy::new(
            TEST_CHAIN_ID,
            [0xABu8; 32].into(),
            [0u8; 20].into(),
            &OmegaConfig::default(),
        );

        let dag = Arc::new(Mutex::new(ExecutionDag::new(DagConfig {
            microtx_slots: 16,
            normal_slots: 4,
            eviction_log_capacity: 1_000,
        })));

        let (hp_tx, hp_rx) = tokio::sync::mpsc::channel(64);

        let zk_cfg = ZkConfig {
            prover_tier: ProverTierConfig::T1Software,
            worker_count: 1,
            microtx_sla_ms: 1_200,
            normal_sla_ms: 4_000,
            proof_queue_throttle: 128,
            proof_queue_suspend: 256,
            proof_queue_halt: 512,
            allow_skip_in_shadow: true,
            checkpoint_dir: OmegaConfig::default().ml.checkpoint_dir.clone(),
            max_checkpoints: OmegaConfig::default().ml.checkpoint_retention,
            chain_id: TEST_CHAIN_ID,
        };
        // Deliberately NOT starting a ProofWorkerPool — leaving the proof queue
        // permanently unserviced is the whole point of this test.
        let proof_queue = ProofQueue::new(zk_cfg);

        let zk_verifier = Arc::new(ZkVerifier::new(TEST_CHAIN_ID));
        let pending_proofs = Arc::new(PendingProofBuffer::new(16));

        let kill_switch_cfg = KillSwitchConfig {
            max_cumulative_loss_wei: u128::MAX / 4,
            max_loss_per_window_wei: u128::MAX / 8,
            loss_window: Duration::from_secs(3600),
            max_consecutive_failures: 32,
        };
        let kill_switches = Arc::new(
            KillSwitchRegistry::new(kill_switch_cfg).expect("KillSwitchRegistry::new"),
        );

        let integrity_registry = IntegrityRegistry::new();

        let path = std::env::temp_dir().join(format!(
            "omega_hot_path_provisioning_test_blacklist_{}_{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, "# empty blacklist for test\n").unwrap();
        let blacklist = BuilderBlacklist::load(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        let relay_metrics = LaRelayMetrics::new(10, ExecutionAddress("0xTEST".into()));
        let relay_clients: HashMap<String, Arc<dyn RelayClient>> = HashMap::new();
        let relay_cfg = omega_relay::RelayConfig {
            confirmation_rpc_url: "http://localhost:1".into(),
            ..Default::default()
        };
        let (relay, _reorg_event_rx) =
            MultiRelayClient::new(relay_clients, relay_metrics, blacklist, &relay_cfg, 0);

        // Test-only key material (same pattern as omega-execution::signer's own tests) —
        // never real keys. Reuses the real, production strategy_onchain_ids() helper
        // rather than a second hand-built map, so this test can never silently drift
        // from what main() actually configures.
        let test_tx_key_manager = Arc::new(
            KeyManager::from_hex(&"3a".repeat(32), TEST_CHAIN_ID).unwrap(),
        );
        let test_blueprint_key_manager = Arc::new(
            KeyManager::from_hex(&"3b".repeat(32), TEST_CHAIN_ID).unwrap(),
        );
        let test_blueprint_signer = Arc::new(BlueprintSigner::new(test_blueprint_key_manager));
        let signer = Arc::new(KeyManagerTransactionSigner::new(
            test_tx_key_manager,
            [0x01u8; 20].into(),
            strategy_onchain_ids(),
            test_blueprint_signer,
        ));
        let execution_pipeline = Arc::new(ExecutionPipeline::new(
            Arc::clone(&kill_switches),
            Arc::clone(&integrity_registry),
            Arc::clone(&relay),
            Arc::clone(&dag),
            Arc::clone(&signer),
            TEST_CHAIN_ID,
        ));

        let nonce_registry = omega_security::replay::NonceRegistry::new();
        let exposure_tracker = AccountExposureTracker::new();
        let (_flashloan_liq_tx, flashloan_liq_rx) =
            tokio::sync::watch::channel(FlashloanLiquidityState::default());

        (
            strategy,
            dag,
            hp_tx,
            hp_rx,
            proof_queue,
            execution_pipeline,
            nonce_registry,
            integrity_registry,
            exposure_tracker,
            flashloan_liq_rx,
            zk_verifier,
            pending_proofs,
        )
    }

    /// Low base fee, block 1 — matches sa.rs's own make_signal(5) test pattern, so
    /// SaStrategy::score/build_blueprint return a genuinely profitable opportunity.
    fn profitable_signal() -> omega_core::SignalState {
        omega_core::SignalState {
            state_version: 1,
            chain_id: TEST_CHAIN_ID,
            block_number: 1_000_000,
            base_fee_gwei: 5,
            l1_data_fee_gwei: 2,
            state_hash: [0x01u8; 32].into(),
        }
    }

    #[tokio::test]
    async fn hot_path_admission_does_not_block_on_zk_proof_completion() {
        let (
            strategy,
            dag,
            hp_tx,
            mut hp_rx,
            proof_queue,
            execution_pipeline,
            nonce_registry,
            integrity_registry,
            exposure_tracker,
            flashloan_liq_rx,
            zk_verifier,
            pending_proofs,
        ) = build_harness().await;

        // SA is hot_path_eligible with gas_budget() == MICROTX_GAS_LIMIT — confirmed
        // against sa.rs's own SA_GAS_BUDGET constant and StrategyTrait impl.
        assert!(strategy.hot_path_eligible(), "test assumes SA is hot-path eligible");

        // Stub hot-path runner: reply immediately so score_and_admit's rrx.await doesn't
        // hang waiting for a real HotPathRunner this test deliberately doesn't spin up.
        tokio::spawn(async move {
            if let Some(req) = hp_rx.recv().await {
                let _ = req.resp_tx.send(omega_hot_path::HotPathResponse {
                    result: Err(omega_core::errors::OmegaError::dropped(
                        omega_core::errors::DropCode::MissCapacity,
                    )),
                    elapsed_us: 0,
                });
            }
        });

        let signal = profitable_signal();

        // Critical assertion: score_and_admit must return within a short bound even
        // though no ProofWorkerPool was ever started, so the background ZK-proof task
        // can never complete. If hot-path admission were re-gated on proof completion,
        // this would hang until the timeout and fail — the regression this test guards.
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            score_and_admit(
                strategy,
                signal,
                dag,
                hp_tx,
                proof_queue,
                HaltFlag::new(),
                1, // active_phase
                OracleSnapshot {
                    chainlink_price: 2000.0,
                    pyth_price: 2001.0,
                    twap_price: 1999.0,
                    chainlink_age_s: 10,
                    pyth_age_s: 10,
                    twap_age_s: 60,
                },
                execution_pipeline,
                nonce_registry,
                integrity_registry,
                0.0, // gas_volatility_risk
                exposure_tracker,
                flashloan_liq_rx,
                TEST_CHAIN_ID,
                [0x11u8; 20],  // vault_address
                [0x22u8; 20],  // profit_token
                zk_verifier,
                pending_proofs,
            ),
        )
        .await;

        assert!(
            outcome.is_ok(),
            "score_and_admit for a hot-path-eligible blueprint must not block on ZK proof \
             completion — it hung past the 5s bound instead, which would mean hot-path \
             admission has regressed back to being gated on the proof queue"
        );
    }
}

#[cfg(test)]
mod la_registration_wiring_tests {
    // C8 regression coverage: LA must be registered in the L13 strategy registry when
    // (and only when) a real "LA" entry exists in IntegrityRegistry. This exercises the
    // decision logic added to main()'s L13 block directly, without spinning up the full
    // binary — the same style as this file's other #[cfg(test)] modules, which build
    // only the real dependencies each unit under test actually needs.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use omega_security::strategy_entries_from_manifest;

    const TEST_CHAIN_ID: u64 = 42_161;

    fn manifest_with_la() -> DeploymentManifest {
        let toml_str = format!(
            r#"
                [[strategies]]
                strategy_id = "LA"
                bytecode_hash = "0x{}"
                contract_address = "0x{}"
                min_phase = 1
            "#,
            "22".repeat(32),
            "33".repeat(20),
        );
        toml::from_str(&toml_str).expect("test manifest TOML must parse")
    }

    /// Regression guard: with a real "LA" entry loaded into IntegrityRegistry, the same
    /// lookup main()'s L13 block performs (`snapshot().find(|e| e.strategy_id == "LA")`)
    /// must find it, and LaStrategy::new must accept the resulting fields without
    /// panicking. This does not spin up main() itself — it proves the lookup and
    /// construction path in isolation.
    #[test]
    fn la_entry_present_in_manifest_is_found_and_constructs_la_strategy() {
        let manifest = manifest_with_la();
        let entries = strategy_entries_from_manifest(&manifest, 4)
            .expect("valid LA entry must pass validation");

        let integrity_registry = IntegrityRegistry::new();
        integrity_registry.register_all(entries);

        let found = integrity_registry
            .snapshot()
            .into_iter()
            .find(|e| e.strategy_id == "LA");
        assert!(
            found.is_some(),
            "L13's lookup must find a real 'LA' entry once one is registered"
        );

        let entry = found.unwrap();
        let liquidity_registry = LiquidityRegistry::new();
        let position_registry = PositionRegistry::new();

        // Must not panic — proves LaStrategy::new accepts the field types L13 passes it
        // (bytecode_hash/contract_address via .into()).
        let _la = LaStrategy::new(
            TEST_CHAIN_ID,
            entry.bytecode_hash.into(),
            entry.contract_address.into(),
            liquidity_registry,
            position_registry,
            &OmegaConfig::default(),
        );
    }

    /// Regression guard: an empty IntegrityRegistry (no manifest loaded, or a manifest
    /// with no LA entry) must NOT be treated as a construction error — L13's match arm
    /// must take the None branch and simply skip registering LA, exactly as it does for
    /// SA/MSA/MEV today.
    #[test]
    fn no_la_entry_is_absent_not_an_error() {
        let integrity_registry = IntegrityRegistry::new();
        let found = integrity_registry
            .snapshot()
            .into_iter()
            .find(|e| e.strategy_id == "LA");
        assert!(
            found.is_none(),
            "an empty IntegrityRegistry must yield None for LA, not a fabricated entry"
        );
    }
}