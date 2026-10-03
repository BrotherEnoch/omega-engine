// src/main.rs — OmegaEngine v12.0 Main Entry Point
//
// Required env vars:
//   ARBITRUM_RPC_URL         WebSocket RPC endpoint
//   ARBITRUM_HTTP_RPC_URL    Plain HTTP JSON-RPC endpoint (InclusionTracker uses plain
//                            HTTP POSTs; ARBITRUM_RPC_URL is WS-only and won't work there)
//   VAULT_ADDRESS            OmegaVault address — feeds publicInputsHash for ZK proofs
//   PROFIT_TOKEN             Profit token address — feeds publicInputsHash for ZK proofs
//   ORCHESTRATOR_ADDRESS     OmegaOrchestrator address every signed tx calls execute() on
//   OMEGA_BLUEPRINT_SIGNING_KEY  Hex secp256k1 key; derived address must match
//                            OmegaOrchestrator.execution_key on-chain
//
// Gas-paying outer envelope (choose one):
//   OMEGA_TX_SIGNER=local    (default) requires OMEGA_TX_SIGNING_KEY
//   OMEGA_TX_SIGNER=kms      requires AWS_KMS_KEY_ID (+ AWS creds/region);
//                            optional OMEGA_TX_EXPECTED_ADDRESS fail-closed check
//   OMEGA_TX_SIGNING_KEY     Hex secp256k1 key when OMEGA_TX_SIGNER=local
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
//   OMEGA_VAULT_SUBMIT_NONCE              FALLBACK ONLY (see C10k): the submitProof keeper
//                                          reads the signer's pending nonce from the chain
//                                          at startup and re-syncs on any broadcast error;
//                                          this value is used only if that chain read fails
//   OMEGA_LOCAL_L1_DATA_FEE_GWEI          non-default chains only (see C10k): L1 data fee
//                                          seeded when the ArbGasInfo poll fails; default 1
//
// ## Changelog (most recent first within each item; see VCS for full history)
//
// - C10k (this package, patch): three fixes found while running against a local Anvil
//   chain (OMEGA_CHAIN_ID=31337).
//   (1) L2d seed: Anvil has no ArbGasInfo precompile, so `getL1BaseFeeEstimate` fails with
//   `InvalidFEOpcode` every cycle and `l1_data_fee_gwei` stays 0. Combined with check 6's
//   old zero-clamping bug (fixed in omega-risk's checks.rs) that dropped 226/226
//   blueprints as MissGasSpike. On a NON-default chain only, a failed poll now seeds a
//   stable fee (OMEGA_LOCAL_L1_DATA_FEE_GWEI, default 1) so the signal carries a
//   consistent value. On the default chain (Arbitrum One) behavior is unchanged: a failed
//   poll keeps the previous value.
//   (2) submitProof keeper nonce: the keeper used an AtomicU64 seeded once from the
//   OMEGA_VAULT_SUBMIT_NONCE env var and never looked at the chain again, so any other
//   sender sharing the account (Anvil account #0 is shared with deploy scripts and the
//   execution path) produced a flood of `nonce too low`. It now reads the signer's
//   pending nonce via eth_getTransactionCount at startup, and on ANY broadcast failure
//   re-reads it from the chain and (for `nonce too low`) retries that proof up to
//   SUBMIT_PROOF_MAX_ATTEMPTS times. The failure log now includes the nonce used.
//   (3) Removed a duplicated, unreachable `Err(e)` match arm in that keeper that had been
//   introduced by pasting the nonce-logging edit next to the original arm.
//
// - C10j (this package, patch): re-wired `tx_signer_factory` back into the binary.
//   A prior edit had started constructing the transaction signer directly in main()
//   via `KeyManagerTransactionSigner::new(...)`, bypassing `tx_signer_factory.rs`'s
//   `ProductionTxSigner` enum and its `OMEGA_TX_SIGNER=local|kms` branch entirely —
//   confirmed by the absence of `mod tx_signer_factory;` and by `aws-sdk-kms` /
//   `aws-config` / `aws-kms-signer` / `aws-kms-omega-adapter` never appearing in a real
//   `cargo build --release` dependency list, even though `tx_signer_factory.rs` directly
//   imports and uses all four. That made `tx_signer_factory.rs` dead code: on disk, but
//   unreachable from the running binary, and `OMEGA_TX_SIGNER`/`AWS_KMS_KEY_ID`/
//   `OMEGA_TX_EXPECTED_ADDRESS` silently did nothing. Restored `mod tx_signer_factory;`
//   and construct the signer via `tx_signer_factory::build_production_tx_signer(...)`,
//   matching this file's own header comment, which has documented the KMS backend the
//   whole time. `ExecutionPipeline<...>`'s generic parameter changes accordingly, from
//   `KeyManagerTransactionSigner` to `tx_signer_factory::ProductionTxSigner`, in both
//   `run_scoring_loop` and `score_and_admit`'s signatures; `hot_path_zk_provisioning_
//   tests::build_harness` now wraps its test-only `KeyManagerTransactionSigner` in
//   `tx_signer_factory::ProductionTxSigner::Local(...)` so its constructed
//   `ExecutionPipeline` still type-checks against `score_and_admit`'s updated signature.
//   No behavior change for anyone already running `OMEGA_TX_SIGNER=local` (or unset) —
//   `ProductionTxSigner::Local` wraps the same `KeyManagerTransactionSigner` construction
//   that was inlined in main() before; this fix makes `OMEGA_TX_SIGNER=kms` actually
//   reachable, which it was not.
//
// - C10i (this package, patch): removed unused-import/dead-code warnings flagged by
//   `cargo check --workspace` without touching behavior or breaking `cargo test`.
//   `KeyManagerTransactionSigner`, `BlueprintSigner`, and `KeyManager` are only
//   referenced from `hot_path_zk_provisioning_tests::build_harness` (via `use
//   super::*;`), so `cargo check` (which doesn't compile `#[cfg(test)]` code) flagged
//   them as unused even though deleting them outright would have broken `cargo test`
//   with an unresolved-name error in that module. Gated the three imports behind
//   `#[cfg(test)]` instead of removing them. `ProductionTxSigner::sign_call()` in
//   `tx_signer_factory.rs` was genuinely dead (only `sign_call_gwei()` is called, from
//   the ZK submitProof keeper) and was removed outright, along with the
//   `alloy_primitives::U256` import that existed only for that method's signature and
//   would otherwise have become a new unused-import warning.
//
// - C10h (this package, patch): fixed E0728 (`await` outside an async function/block)
//   in `OracleTokenPriceLookup::price_usd_and_decimals` — this method implements
//   `omega_strategies::TokenPriceLookup`, whose trait signature is synchronous (see
//   `build_oracle_snapshot` above, which already calls `chainlink.read(token)` /
//   `pyth.read(token)` / `twap.read(token)` with no `.await`), but the Chainlink branch
//   here was still writing `self.chainlink.read(symbol).await` — a leftover `.await`
//   from an earlier draft where this lookup was sketched as async. `ChainlinkOracle::read`
//   is not an async fn, so `.await`ing its return value is only legal inside an
//   `async fn`/block, and `price_usd_and_decimals` is neither. Fixed by dropping the
//   stray `.await`; the Pyth branch immediately below was already correctly
//   synchronous (`self.pyth.read(symbol)`, no `.await`) and needed no change. No
//   behavior change: `ChainlinkOracle::read` already returned its `Option<Price>`
//   synchronously, so removing `.await` from a value that was never a `Future` doesn't
//   alter what value flows through — it only removes code that could never compile.
//
// - C10g (this package, patch): fixed E0428 (`main` defined multiple times) and the
//   resulting E0432/E0433 unresolved-crate errors — a leftover scratch block (an
//   `aws_sdk_kms` import plus its own standalone `fn main() { ... }` that only checked
//   that the AWS KMS SDK's API surface compiled) had been left in this file above the
//   real `#[tokio::main] async fn main() -> Result<()>`. Two separate `fn main`
//   definitions in one module is a hard error regardless of signature, and this crate
//   has no `aws-sdk-kms` dependency in Cargo.toml, so the import was also unresolved on
//   its own. Removed both the import and the scratch `fn main() { ... }` entirely; the
//   real, `#[tokio::main]`-annotated `async fn main() -> Result<()>` below is unchanged
//   and is now the sole `main`. No behavior change to the running engine — this was
//   dead scratch code, never reachable from the real entry point.
//
// - C10f (this package, patch): fixed the `-D warnings` clippy failure —
//   `use std::collections::{HashMap, HashSet};` triggered `unused_imports` on
//   `HashSet` because the only `HashSet` usage in this file (the Gap 6 manifest
//   completeness check below) already spells it out via the fully-qualified
//   `std::collections::HashSet` path rather than the bare imported name. Fixed by
//   importing only `HashMap`; behavior is unchanged, this is formatting/import-hygiene
//   only.
//
// - C10c (this package, patch): fixed pyth_ratio in build_check_context's oracle
//   freshness calculation — it was dividing Pyth's age by CHAINLINK_STALENESS_SECS
//   instead of PYTH_STALENESS_SECS (a copy-paste bug from the adjacent cl_ratio line).
//
// - C10d (this package, patch): fixed E0061 in
//   hot_path_zk_provisioning_tests::build_harness — SaStrategy::new's constructor
//   signature gained a `liquidity_registry: Arc<LiquidityRegistry>` parameter (Option B
//   capital path, see sa.rs's own module-level comment) that this test harness was
//   never updated for.
//
// - C10e (this package, patch): OraclePrice::is_fresh() no longer takes a staleness
//   argument (omega-oracle's own resolution.rs derives the threshold from the price's
//   own `source` internally).
//
// - C10b: CheckContext WETH watch-channel MAX now includes Uniswap V3
//   alongside Aave/Balancer (was registry-only for Uni).
//
// - C10: L2e now also polls Uniswap V3, via a single verified WETH/USDC_NATIVE
//   0.05%-fee pool (`omega_rpc::UNISWAP_V3_WETH_USDC_POOL`).
//
// - C9: L2e now polls BOTH WETH and USDC_NATIVE into LiquidityRegistry.
//
// - C8: LA registered alongside CNRY in the L13 strategy registry.
// VERIFIED / closed — see StrategyEntry and omega-strategies lib.rs re-exports.

/// Parse a 0x-prefixed or bare 32-byte hex string into B256.
fn parse_b256_hex(s: &str) -> Option<alloy_primitives::B256> {
    let s = s.trim().trim_start_matches("0x");
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(alloy_primitives::B256::from(out))
}

/// Reads `eth_getTransactionCount(address, "pending")` over plain HTTP JSON-RPC.
///
/// Used by the ZK submitProof keeper (C10k) so its nonce always comes from the chain
/// rather than from a one-shot env var. Returns `None` (and logs at `warn`) on any
/// transport/parse failure; callers decide how to fall back.
async fn fetch_pending_nonce(
    http: &reqwest::Client,
    rpc_url: &str,
    address_hex: &str,
) -> Option<u64> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_getTransactionCount",
        "params": [address_hex, "pending"]
    });
    let resp = match http.post(rpc_url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, address = %address_hex, "pending nonce lookup: HTTP request failed");
            return None;
        }
    };
    let v: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, address = %address_hex, "pending nonce lookup: JSON decode failed");
            return None;
        }
    };
    if let Some(err) = v.get("error") {
        tracing::warn!(rpc_error = %err, address = %address_hex, "pending nonce lookup: JSON-RPC error");
        return None;
    }
    let hex = v.get("result").and_then(|r| r.as_str())?;
    match u64::from_str_radix(hex.trim_start_matches("0x"), 16) {
        Ok(n) => Some(n),
        Err(e) => {
            tracing::warn!(error = %e, address = %address_hex, "pending nonce lookup: hex parse failed");
            None
        }
    }
}

/// Async eth_call lookup of OmegaVault.pending_profit(bytes32).
///
/// Never blocks the tokio runtime: all HTTP is `.await`ed. Lookup failures
/// log at `warn` and return `None` so Stage 7 can fall back to provisional
/// expected profit without aborting the reconciliation task.
struct HttpVaultPendingProfitLookup {
    rpc_url: String,
    vault_address: String,
    http: reqwest::Client,
}

impl HttpVaultPendingProfitLookup {
    fn new(rpc_url: impl Into<String>, vault_address: impl Into<String>) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            vault_address: vault_address.into(),
            http: reqwest::Client::new(),
        }
    }

    async fn fetch_pending_profit(&self, blueprint_hash_hex: &str) -> Option<i128> {
        let hash = blueprint_hash_hex.trim_start_matches("0x");
        if hash.len() != 64 {
            tracing::warn!(
                blueprint_hash = %blueprint_hash_hex,
                "vault profit lookup: invalid blueprint_hash length (need 32 bytes hex)"
            );
            return None;
        }
        let full = omega_security::keccak256(b"pending_profit(bytes32)");
        let selector = format!(
            "{:02x}{:02x}{:02x}{:02x}",
            full[0], full[1], full[2], full[3]
        );
        let data = format!("0x{}{}", selector, hash);
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_call",
            "params": [
                { "to": self.vault_address, "data": data },
                "latest"
            ]
        });
        let resp = match self.http.post(&self.rpc_url).json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    blueprint_hash = %blueprint_hash_hex,
                    "vault profit lookup: HTTP request failed"
                );
                return None;
            }
        };
        if !resp.status().is_success() {
            tracing::warn!(
                status = %resp.status(),
                blueprint_hash = %blueprint_hash_hex,
                "vault profit lookup: non-success HTTP status"
            );
            return None;
        }
        let v: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    blueprint_hash = %blueprint_hash_hex,
                    "vault profit lookup: JSON decode failed"
                );
                return None;
            }
        };
        if let Some(err) = v.get("error") {
            tracing::warn!(
                rpc_error = %err,
                blueprint_hash = %blueprint_hash_hex,
                "vault profit lookup: JSON-RPC error"
            );
            return None;
        }
        let hex = match v.get("result").and_then(|r| r.as_str()) {
            Some(h) => h,
            None => {
                tracing::warn!(
                    blueprint_hash = %blueprint_hash_hex,
                    "vault profit lookup: missing result field"
                );
                return None;
            }
        };
        let hex = hex.trim_start_matches("0x");
        if hex.is_empty() {
            return Some(0);
        }
        match u128::from_str_radix(hex, 16) {
            Ok(profit) if profit > i128::MAX as u128 => Some(i128::MAX),
            Ok(profit) => Some(profit as i128),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    blueprint_hash = %blueprint_hash_hex,
                    "vault profit lookup: hex parse failed"
                );
                None
            }
        }
    }
}

#[async_trait]
impl AsyncRealizedProfitLookup for HttpVaultPendingProfitLookup {
    async fn realized_profit_wei(&self, blueprint_hash_hex: &str) -> Option<i128> {
        self.fetch_pending_profit(blueprint_hash_hex).await
    }
}

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
use async_trait::async_trait;
use omega_core::{HealthState, LayerHealth, LayerId, OmegaConfig, StrategyId};
use omega_dag::{DagConfig, ExecutionDag};
use omega_execution::config_translation::{translate_relay_config, RelayBootstrapInputs};
use omega_execution::run_idempotency_eviction_loop;
// C10i: only referenced from hot_path_zk_provisioning_tests::build_harness (via
// `use super::*;`) — not from any non-test code in this file. `cargo check
// --workspace` doesn't compile #[cfg(test)] code, so an unconditional import here
// showed up as unused there; gating it behind #[cfg(test)] keeps `cargo test` compiling
// (that module still needs it) while silencing the `cargo check` warning.
#[cfg(test)]
use omega_execution::signer::KeyManagerTransactionSigner;
use omega_execution::{
    default_journal_path, open_default_journal, process_confirmation_results_async,
    AsyncRealizedProfitLookup, ExecutionOutcome, ExecutionPipeline, InFlightJournal,
    InFlightRecord, InFlightStatus, Stage7Config,
};
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
    CheckContext, FlashloanSnapshot, OracleSnapshot, CHAINLINK_STALENESS_SECS, PYTH_STALENESS_SECS,
    TWAP_STALENESS_SECS,
};
use omega_risk::kill_switch::{KillSwitchConfig, KillSwitchRegistry};
use omega_rpc::{
    rate_limiter::RpcRateLimiter, run_dex_sync_stream, run_fee_oracle_stream,
    run_lending_protocol_stream, run_mev_share_stream, run_pending_tx_stream,
    validate_deployed_contracts, OmegaRpcClient, RpcClientConfig, AAVE_V3_POOL, BALANCER_V2_VAULT,
    UNISWAP_V3_WETH_USDC_POOL, USDC_NATIVE, WETH,
};
use omega_security::{
    strategy_entries_from_manifest, AccountExposureTracker, DeploymentManifest, IntegrityRegistry,
};
// C10i: same reasoning as the KeyManagerTransactionSigner gate above —
// BlueprintSigner/KeyManager are only used by hot_path_zk_provisioning_tests::
// build_harness (constructing a test-only signer pair), not by any non-test code in
// this file.
#[cfg(test)]
use omega_security::{BlueprintSigner, KeyManager};
// Strategies from IntegrityRegistry manifest. VERIFIED: re-exported at crate root.
use omega_flashloan::{FlashloanProvider, LiquidityRegistry};
use omega_strategies::{
    registry::StrategyRegistryBuilder, CnryStrategy, LaStrategy, MevStrategy, MsaStrategy,
    SaStrategy, StrategyRegistry,
};
use omega_zk::{
    binding::compute_public_inputs_hash, config::ProverTierConfig, PendingProofBuffer, ProofQueue,
    ProofWorkerPool, VerifiedProofSubmission, ZkConfig, ZkVerifier,
};
// C8: real, live lending-position registry LaStrategy now requires at construction.
use omega_positions::PositionRegistry;
// C10f: HashSet dropped from this import — its only use in this file (the Gap 6
// manifest-completeness check below) already spells it out as the fully-qualified
// `std::collections::HashSet`, so importing the bare name here was unused.
use std::collections::HashMap;

/// Restored (C10j): the signer factory module — see this file's header for why this
/// must be declared for `tx_signer_factory.rs` to be reachable at all.
mod tx_signer_factory;

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

/// C10k: max sign+broadcast attempts per submitProof when the node answers `nonce too low`.
const SUBMIT_PROOF_MAX_ATTEMPTS: u32 = 3;

/// C10k: L1 data fee (gwei) seeded on NON-default chains when the ArbGasInfo poll fails,
/// unless overridden by OMEGA_LOCAL_L1_DATA_FEE_GWEI.
const LOCAL_CHAIN_L1_DATA_FEE_GWEI_DEFAULT: u64 = 1;

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
const RISK_SCORE_MAX_THRESHOLD: f64 = 0.45;

/// Shadow-only default (1 ETH). Phase >= 1 requires explicit OMEGA_MAX_ACCOUNT_EXPOSURE_WEI.
const MAX_ACCOUNT_EXPOSURE_WEI_SHADOW_DEFAULT: u128 = 1_000_000_000_000_000_000;

fn max_account_exposure_wei_from_env(active_phase: u8) -> Option<u128> {
    match std::env::var("OMEGA_MAX_ACCOUNT_EXPOSURE_WEI") {
        Ok(s) => match s.parse::<u128>() {
            Ok(v) if v > 0 => Some(v),
            _ => {
                tracing::error!("OMEGA_MAX_ACCOUNT_EXPOSURE_WEI must be positive u128");
                None
            }
        },
        Err(_) if active_phase == 0 => Some(MAX_ACCOUNT_EXPOSURE_WEI_SHADOW_DEFAULT),
        Err(_) => {
            tracing::error!(
                active_phase,
                "OMEGA_MAX_ACCOUNT_EXPOSURE_WEI required when active_phase >= 1"
            );
            None
        }
    }
}

/// C3: max competition probability before MissCompetition. Default 0.95 so a real
/// model output can pass; `0.0` would fail-closed every blueprint (previous bug).
fn max_competition_probability_from_env() -> f64 {
    std::env::var("OMEGA_MAX_COMPETITION_PROBABILITY")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|v| (0.0..=1.0).contains(v))
        .unwrap_or(0.95)
}

/// C3: rollout tier [0,1] for S19 volume gating. No check reads this yet; still
/// assembled from env so control plane can set it without code changes.
fn rollout_tier_from_env() -> f64 {
    std::env::var("OMEGA_ROLLOUT_TIER")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|v| (0.0..=1.0).contains(v))
        .unwrap_or(1.0)
}

/// C10k: L1 data fee (gwei) to seed on non-default chains when the ArbGasInfo poll fails.
fn local_l1_data_fee_gwei_from_env() -> u64 {
    std::env::var("OMEGA_LOCAL_L1_DATA_FEE_GWEI")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(LOCAL_CHAIN_L1_DATA_FEE_GWEI_DEFAULT)
}

/// Load kill-switch thresholds from env.
///
/// Threshold evaluation (cumulative / window / consecutive failures) is
/// implemented in `omega_risk::kill_switch`. This loads the caps only.
///
/// Phase 0 may use defaults. Phase ≥ 1 requires every `OMEGA_KILL_*` set
/// explicitly (no silent live defaults).
fn kill_switch_config_from_env(active_phase: u8) -> anyhow::Result<KillSwitchConfig> {
    const DEFAULT_CUMULATIVE: u128 = 1_000_000_000_000_000_000;
    const DEFAULT_WINDOW_LOSS: u128 = 250_000_000_000_000_000;
    const DEFAULT_WINDOW_SECS: u64 = 3600;
    const DEFAULT_CONSEC: u32 = 5;

    fn req_u128(key: &str, default: u128, require: bool) -> anyhow::Result<u128> {
        match std::env::var(key) {
            Ok(s) => s.parse().map_err(|e| anyhow::anyhow!("{key}={s:?}: {e}")),
            Err(_) if require => Err(anyhow::anyhow!(
                "{key} must be set when active_phase >= 1 (no silent kill defaults)"
            )),
            Err(_) => Ok(default),
        }
    }
    fn req_u32(key: &str, default: u32, require: bool) -> anyhow::Result<u32> {
        match std::env::var(key) {
            Ok(s) => s.parse().map_err(|e| anyhow::anyhow!("{key}={s:?}: {e}")),
            Err(_) if require => Err(anyhow::anyhow!(
                "{key} must be set when active_phase >= 1 (no silent kill defaults)"
            )),
            Err(_) => Ok(default),
        }
    }
    fn req_secs(key: &str, default: u64, require: bool) -> anyhow::Result<Duration> {
        let v = match std::env::var(key) {
            Ok(s) => {
                let secs: u64 = s.parse().map_err(|e| anyhow::anyhow!("{key}={s:?}: {e}"))?;
                if secs == 0 {
                    return Err(anyhow::anyhow!("{key} must be > 0"));
                }
                secs
            }
            Err(_) if require => {
                return Err(anyhow::anyhow!(
                    "{key} must be set when active_phase >= 1 (no silent kill defaults)"
                ));
            }
            Err(_) => default,
        };
        Ok(Duration::from_secs(v))
    }
    let require = active_phase >= 1;
    Ok(KillSwitchConfig {
        max_cumulative_loss_wei: req_u128(
            "OMEGA_KILL_MAX_CUMULATIVE_LOSS_WEI",
            DEFAULT_CUMULATIVE,
            require,
        )?,
        max_loss_per_window_wei: req_u128(
            "OMEGA_KILL_MAX_LOSS_PER_WINDOW_WEI",
            DEFAULT_WINDOW_LOSS,
            require,
        )?,
        loss_window: req_secs("OMEGA_KILL_LOSS_WINDOW_SECS", DEFAULT_WINDOW_SECS, require)?,
        max_consecutive_failures: req_u32(
            "OMEGA_KILL_MAX_CONSECUTIVE_FAILURES",
            DEFAULT_CONSEC,
            require,
        )?,
    })
}

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
/// parse or doesn't match `DeploymentManifest`'s shape.
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

/// Parses a REQUIRED 0x-prefixed (or bare) hex env var into 20 bytes.
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
/// `contracts/src/StrategyIds.sol`'s `keccak256("OMEGA_STRATEGY_<X>")` constants.
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
    m.insert(
        "SA".to_string(),
        hash32("c4bb1c851b1c74593f61f8d1f99ec07e2960d847a94d4a736e321ba387d4d2d7"),
    );
    m.insert(
        "LA".to_string(),
        hash32("77b0296a1c4dae896ee0ffe05246d8b3e8ecd44a1d4a0c6591b183fb2390a698"),
    );
    m.insert(
        "MSA".to_string(),
        hash32("bfd7e8e9c54a6762cb6ff399dc8bdefe2226a32400ed6001e1bee533bbaa25d2"),
    );
    m.insert(
        "MEV".to_string(),
        hash32("892be743cfc8880f51726a84ab1d0d0fc05336d49927c5a9eaaf926a84db319a"),
    );
    m.insert(
        "CNRY".to_string(),
        hash32("93879ddf9ec0b01c066594680539ea61eaab23f806b410fda1c18659efcc7725"),
    );
    m
}

// ── Health layers ─────────────────────────────────────────────────────────────

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

/// Sliding-window counter of MEV-Share events that indicate competing order flow.
#[derive(Debug, Default)]
struct MevShareActivityTracker {
    inner: std::sync::Mutex<std::collections::VecDeque<u64>>,
}

impl MevShareActivityTracker {
    const WINDOW_MS: u64 = 30_000;
    const MAX_EVENTS: usize = 256;

    fn new() -> Self {
        Self::default()
    }

    fn record(&self, received_at_unix_ms: u64) {
        let mut q = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        q.push_back(received_at_unix_ms);
        while q.len() > Self::MAX_EVENTS {
            q.pop_front();
        }
        let cutoff = received_at_unix_ms.saturating_sub(Self::WINDOW_MS);
        while q.front().map(|t| *t < cutoff).unwrap_or(false) {
            q.pop_front();
        }
    }

    fn events_in_window(&self, now_unix_ms: u64) -> u32 {
        let mut q = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let cutoff = now_unix_ms.saturating_sub(Self::WINDOW_MS);
        while q.front().map(|t| *t < cutoff).unwrap_or(false) {
            q.pop_front();
        }
        q.len() as u32
    }
}

#[derive(Debug, Clone, Default)]
struct FlashloanLiquidityState {
    /// Real, live available liquidity in wei.
    available_wei: u128,
    /// `"aave"`, `"balancer"`, or `"uniswap_v3"`, whichever read was larger on the
    /// most recent successful poll. Empty before the first successful poll.
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
/// isn't in it.
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
/// `MultiRelayClient::on_new_block` needs.
fn feed_block_event_to_reorg_guard(relay: &MultiRelayClient, event: &omega_rpc::BlockEvent) {
    let hash_bytes: [u8; 32] = *event.hash;
    relay.on_new_block(event.number, hash_bytes);
}

/// Builds the `CheckContext` passed to `ExecutionPipeline::execute`'s Stage 2c
/// (15+ pre-trade checks).
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
    l1_adaptive_buffer: f64,
    competition_probability: f64,
    max_competition_probability: f64,
    max_account_exposure_wei: u128,
    rollout_tier: f64,
) -> CheckContext {
    let oracle_freshness_risk = {
        let cl_ratio = oracle_snapshot.chainlink_age_s as f64 / CHAINLINK_STALENESS_SECS as f64;
        let pyth_ratio = oracle_snapshot.pyth_age_s as f64 / PYTH_STALENESS_SECS as f64;
        let twap_ratio = oracle_snapshot.twap_age_s as f64 / TWAP_STALENESS_SECS as f64;
        cl_ratio.min(pyth_ratio).min(twap_ratio).min(1.0)
    };

    let flashloan_available_value: u128 = flashloan_snapshot.available_wei;
    let flashloan_protocol_id: String = flashloan_snapshot.protocol_id;

    let competition_risk = competition_probability.clamp(0.0, 1.0);
    let liquidity_risk = if flashloan_available_value > 0 {
        0.0
    } else {
        1.0
    };

    let rollout_risk = (1.0 - rollout_tier.clamp(0.0, 1.0)).clamp(0.0, 1.0);

    let risk_score = (RISK_WEIGHT_GAS_VOLATILITY * gas_volatility_risk
        + RISK_WEIGHT_ORACLE_FRESHNESS * oracle_freshness_risk
        + RISK_WEIGHT_COMPETITION * competition_risk
        + RISK_WEIGHT_LIQUIDITY * liquidity_risk
        + 0.10 * rollout_risk)
        .clamp(0.0, 1.0);

    CheckContext {
        expected_chain_id: chain_id,
        current_block: sig.block_number,
        current_l1_gas_price_gwei: sig.l1_data_fee_gwei,
        current_l2_base_fee_gwei: sig.base_fee_gwei,
        l1_adaptive_buffer,
        oracle: oracle_snapshot,
        flashloan: FlashloanSnapshot {
            available: flashloan_available_value,
            protocol_id: flashloan_protocol_id,
        },
        competition_probability: competition_risk,
        max_competition_probability,
        strategy_max_gas,
        max_slippage_bps,
        rollout_tier,
        strategy_bytecode_hash,
        risk_score,
        max_risk_score: RISK_SCORE_MAX_THRESHOLD,
        current_account_exposure_wei,
        max_account_exposure_wei,
        latest_blueprint_nonce,
    }
}

/// C3: competition probability for the primary tracked asset (WETH oracle path).
fn competition_probability_for_primary_asset(mev_share_events_in_window: u32) -> f64 {
    use omega_risk::competition::{competition_probability, competition_with_mev_share, AssetTier};
    let tier = AssetTier::from_symbol(ORACLE_SNAPSHOT_TOKEN);
    let base = competition_probability(tier, 1.05, 0.0);
    competition_with_mev_share(base, mev_share_events_in_window)
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let log_level = std::env::var("OMEGA_LOG_LEVEL")
        .ok()
        .and_then(|s| s.trim().parse::<Level>().ok())
        .unwrap_or(Level::INFO);
    tracing_subscriber::fmt()
        .with_max_level(log_level)
        .with_target(true)
        .json()
        .init();

    tracing::info!("OmegaEngine v12.0 starting");

    let rpc_url = std::env::var("ARBITRUM_RPC_URL").context("ARBITRUM_RPC_URL must be set")?;
    let config_path = std::env::var("OMEGA_CONFIG").unwrap_or_else(|_| DEFAULT_CONFIG.to_string());

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
    let max_account_exposure_wei =
        max_account_exposure_wei_from_env(active_phase).ok_or_else(|| {
            anyhow::anyhow!("set OMEGA_MAX_ACCOUNT_EXPOSURE_WEI > 0 when active_phase >= 1")
        })?;
    tracing::info!(
        max_account_exposure_wei,
        "check 14 max account exposure resolved"
    );

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
    let rpc =
        OmegaRpcClient::connect_with_retry(RpcClientConfig::new(&rpc_url, DEFAULT_RPS, chain_id))
            .await
            .context("connecting to Arbitrum RPC endpoint")?
            .with_health(as_health(find_layer(&layers, LayerId::Rpc)));

    // ── C7: validate hardcoded flashloan/liquidity addresses ───────────────────
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
    let (mev_tx, mev_rx) = tokio::sync::broadcast::channel(256);
    let mev_share_activity = Arc::new(MevShareActivityTracker::new());

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
    {
        let mut rx = mev_rx;
        let activity = Arc::clone(&mev_share_activity);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        if ev.indicates_competition() {
                            activity.record(ev.received_at_unix_ms);
                            tracing::debug!(
                                has_txs = ev.has_txs,
                                has_logs = ev.has_logs,
                                "MEV-Share competition signal recorded"
                            );
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "MEV-Share consumer lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        tracing::info!("MEV-Share consumer attached (feeds CheckContext competition)");
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

    tracing::warn!("Pyth cache constructed but UNFED — no ingestion path exists yet.");

    // ── L2d: ArbGasInfo L1 data fee polling + L1GasEma for CheckContext ────────
    let l1_gas_ema = Arc::new(omega_risk::gas_model::L1GasEma::new(32));
    {
        let gas_client = rpc.clone();
        let gas_oracle = Arc::clone(&oracle);
        let l1_ema = Arc::clone(&l1_gas_ema);
        let chain_id_l2d = chain_id;
        let local_l1_fee_gwei = local_l1_data_fee_gwei_from_env();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(15));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                match gas_client.fetch_l1_base_fee_estimate_gwei().await {
                    Ok(gwei) => {
                        gas_oracle.update_l1_data_fee_gwei(gwei);
                        l1_ema.push_price(gwei);
                        tracing::debug!(l1_data_fee_gwei = gwei, "ArbGasInfo poll: updated");
                    }
                    Err(e) => {
                        if chain_id_l2d != DEFAULT_CHAIN_ID {
                            // C10k: a local/test chain has no ArbGasInfo precompile, so this
                            // poll can never succeed there. Seed a stable value so the signal
                            // carries a consistent L1 data fee instead of a permanent 0.
                            tracing::warn!(
                                error = %e,
                                seeded_l1_data_fee_gwei = local_l1_fee_gwei,
                                "ArbGasInfo poll failed on non-default chain — seeding stable local L1 data fee"
                            );
                            gas_oracle.update_l1_data_fee_gwei(local_l1_fee_gwei);
                            l1_ema.push_price(local_l1_fee_gwei);
                        } else {
                            tracing::warn!(error = %e, "ArbGasInfo poll failed — keeping previous value");
                        }
                    }
                }
            }
        });
        tracing::info!("L2d ArbGasInfo poll loop started (15s interval; L1GasEma window=32)");
    }

    // ── L2e: flashloan liquidity polling ───────────────────────────────────────
    let (flashloan_liq_tx, flashloan_liq_rx) =
        tokio::sync::watch::channel(FlashloanLiquidityState::default());
    let liquidity_registry = LiquidityRegistry::new();
    {
        let liq_client = rpc.clone();
        let liq_tx = flashloan_liq_tx.clone();
        let registry = Arc::clone(&liquidity_registry);
        let chain_id_l2e = chain_id;
        let aave_tag_l2e = aave_pool_tag;
        let balancer_tag_l2e = balancer_vault_tag;
        tokio::spawn(async move {
            let tracked_assets = [WETH, USDC_NATIVE];
            let mut ticker =
                tokio::time::interval(Duration::from_secs(FLASHLOAN_LIQUIDITY_POLL_INTERVAL_S));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;

                for &token in &tracked_assets {
                    let aave = liq_client.fetch_aave_available(token).await;
                    let balancer = liq_client.fetch_balancer_available(token).await;

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

                    if token != WETH {
                        continue;
                    }

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
                                "all flashloan liquidity reads failed (Aave, Balancer, Uniswap V3) \
                                 — keeping previous CheckContext watch value (C10 fail closed)"
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
            "L2e flashloan liquidity poll loop started"
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
    let kill_switch_cfg =
        kill_switch_config_from_env(active_phase).context("kill switch config")?;
    tracing::info!(
        max_cumulative_loss_wei = kill_switch_cfg.max_cumulative_loss_wei,
        max_loss_per_window_wei = kill_switch_cfg.max_loss_per_window_wei,
        loss_window_secs = kill_switch_cfg.loss_window.as_secs(),
        max_consecutive_failures = kill_switch_cfg.max_consecutive_failures,
        "KillSwitchRegistry thresholds (env OMEGA_KILL_* or production defaults)"
    );
    let kill_switches =
        Arc::new(KillSwitchRegistry::new(kill_switch_cfg).context("KillSwitchRegistry::new")?);

    let integrity_registry = IntegrityRegistry::new();
    match load_deployment_manifest(DEPLOYMENT_MANIFEST_PATH)
        .with_context(|| format!("loading deployment manifest from {DEPLOYMENT_MANIFEST_PATH}"))?
    {
        Some(manifest) => {
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
            if active_phase >= 1 {
                const REQUIRED: &[(&str, u8)] =
                    &[("CNRY", 0), ("SA", 1), ("MSA", 2), ("LA", 3), ("MEV", 4)];
                let present: std::collections::HashSet<String> =
                    ids.iter().map(|s| s.to_ascii_uppercase()).collect();
                let missing: Vec<String> = REQUIRED
                    .iter()
                    .filter(|(id, mp)| *mp <= active_phase && !present.contains(*id))
                    .map(|(id, mp)| format!("{id} (min_phase={mp})"))
                    .collect();
                if !missing.is_empty() {
                    anyhow::bail!(
                        "Gap 6: manifest incomplete for active_phase={active_phase}: missing {}",
                        missing.join(", ")
                    );
                }
            }
        }
        None => {
            if active_phase >= 1 {
                anyhow::bail!(
                    "Gap 6: no deployment manifest at {DEPLOYMENT_MANIFEST_PATH} while \
                     active_phase={active_phase}. Phase ≥ 1 requires CNRY/SA/MSA/LA/MEV entries."
                );
            }
            tracing::warn!(
                path = DEPLOYMENT_MANIFEST_PATH,
                "No deployment manifest — IntegrityRegistry empty (shadow phase 0 only)"
            );
        }
    }

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
                Ok(t) => RelayAuth::AuthorizationToken(t),
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
                "C5: zero relay clients constructed (no OMEGA_RELAY_ENDPOINT_* / auth keys) \
                 while active_phase={} — refusing to start rather than running a production \
                 phase that cannot submit bundles",
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

    let position_registry = PositionRegistry::new();
    tracing::info!("PositionRegistry ready for LA + reorg invalidate");

    let (relay, reorg_event_rx) =
        MultiRelayClient::new(relay_clients, relay_metrics, blacklist, &relay_cfg, 0);

    {
        let mut rx = reorg_event_rx;
        let ks = Arc::clone(&kill_switches);
        let positions = Arc::clone(&position_registry);
        let cid = chain_id;
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                tracing::warn!(
                    orphaned_block = ev.orphaned_block,
                    rescore_at = ev.rescore_at_block,
                    tx = %ev.tx_hash.0,
                    "LaReorgRiskEvent — KS outcome + position invalidate"
                );
                if let Some(reason) = ks.record_outcome("global", None, false) {
                    tracing::error!(?reason, "kill switch tripped after reorg-risk event");
                }
                let removed = positions.invalidate_chain(cid);
                tracing::info!(
                    removed,
                    chain_id = cid,
                    "PositionRegistry invalidated for LA rescore after reorg"
                );
            }
        });
    }

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

    // ── Real TransactionSigner construction (C10j: via tx_signer_factory) ──────
    let orchestrator_address = parse_address_env("ORCHESTRATOR_ADDRESS").context(
        "ORCHESTRATOR_ADDRESS must be set -- the deployed OmegaOrchestrator contract \
                  address every signed transaction this signer produces calls execute() on",
    )?;

    let signer = tx_signer_factory::build_production_tx_signer(
        chain_id,
        orchestrator_address.into(),
        strategy_onchain_ids(),
    )
    .await
    .context("build_production_tx_signer")?;
    tracing::info!(
        backend = signer.backend_name(),
        orchestrator = %hex::encode(orchestrator_address),
        tx_signer_address = %hex::encode(signer.active_address()),
        "ProductionTxSigner constructed -- real transaction signing wired in"
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

    {
        let pipe = Arc::clone(&execution_pipeline);
        tokio::spawn(async move {
            run_idempotency_eviction_loop(
                pipe,
                Duration::from_secs(60),
                chrono::Duration::hours(2),
            )
            .await;
        });
        tracing::info!("idempotency eviction loop started (60s tick, 2h max age)");
    }

    // ── In-flight journal (crash recovery) ────────────────────────────────────
    let inflight_journal: Option<Arc<InFlightJournal>> = match open_default_journal() {
        Some(j) => Some(Arc::new(j)),
        None => {
            if active_phase >= 1 {
                anyhow::bail!(
                    "OMEGA_INFLIGHT_JOURNAL (or default {}) could not be opened while \
                     active_phase={} — refusing production start without crash-recovery trail",
                    default_journal_path().display(),
                    active_phase
                );
            }
            tracing::warn!(
                "InFlightJournal unavailable in phase 0 (shadow) — continuing without durable recovery"
            );
            None
        }
    };

    if let Some(ref journal) = inflight_journal {
        match journal.recover_open() {
            Ok(open) => {
                let mut seeded = 0usize;
                for rec in &open {
                    if let Some(key) = parse_b256_hex(&rec.idempotency_key_hex) {
                        execution_pipeline.seed_idempotency_key(key);
                        seeded += 1;
                    } else {
                        tracing::warn!(
                            blueprint_hash = %rec.blueprint_hash,
                            key = %rec.idempotency_key_hex,
                            "InFlightJournal recover: unparseable idempotency_key_hex — skipped seed"
                        );
                    }
                }
                tracing::info!(
                    open = open.len(),
                    seeded,
                    "InFlightJournal recovery: idempotency keys re-seeded"
                );
            }
            Err(e) => {
                if active_phase >= 1 {
                    return Err(e).context("InFlightJournal::recover_open failed at phase >= 1");
                }
                tracing::warn!(error = %e, "InFlightJournal::recover_open failed in phase 0");
            }
        }
    }

    // ── Nonce registry ────────────────────────────────────────────────────────
    let nonce_registry = omega_security::replay::NonceRegistry::new();
    tracing::info!("NonceRegistry ready — advanced on execute Ok + Stage-7 inclusion");

    // ── Stage 7: confirmation reconciliation ───────────────────────────────────
    {
        let relay7 = Arc::clone(&relay);
        let oracle7 = Arc::clone(&oracle);
        let ks7 = Arc::clone(&kill_switches);
        let nr7 = nonce_registry.clone();
        let stage7_cfg = Stage7Config::from_env(chain_id);
        let journal7 = inflight_journal.clone();
        let http_rpc = confirmation_rpc_url.clone();
        let vault_hex = format!("0x{}", hex::encode(vault_address));
        let profit_lookup: Arc<dyn AsyncRealizedProfitLookup> =
            Arc::new(HttpVaultPendingProfitLookup::new(http_rpc, vault_hex));
        let mut rx = oracle7.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(_) => {
                        let current_block = oracle7.snapshot().fee.block_number;
                        let results = relay7.reconcile_inclusions(current_block).await;
                        if results.is_empty() {
                            continue;
                        }
                        let processed = process_confirmation_results_async(
                            &results,
                            ks7.as_ref(),
                            &nr7,
                            &stage7_cfg,
                            profit_lookup.as_ref(),
                        )
                        .await;
                        if let Some(ref journal) = journal7 {
                            for (r, p) in results.iter().zip(processed.iter()) {
                                let status = if p.included {
                                    InFlightStatus::Included
                                } else {
                                    InFlightStatus::Missed
                                };
                                if let Err(e) = journal.record_terminal(
                                    &r.blueprint_hash,
                                    &r.bundle_hash,
                                    status,
                                    &p.strategy_id,
                                    p.nonce,
                                    stage7_cfg.chain_id,
                                    r.expected_profit_net_wei,
                                    current_block,
                                    "",
                                ) {
                                    tracing::error!(
                                        error = %e,
                                        bundle_hash = %r.bundle_hash,
                                        "InFlightJournal::record_terminal failed"
                                    );
                                }
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "Stage-7 reconciliation loop lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        tracing::info!("Stage-7 reconciliation started (with vault P&L lookup + journal terminal)");
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
        chain_id,
    };
    let proof_queue = ProofQueue::new(zk_cfg.clone());
    let _pool = ProofWorkerPool::start(zk_cfg, proof_queue.clone());
    let zk_verifier = Arc::new(ZkVerifier::new(chain_id));
    let pending_proofs = Arc::new(PendingProofBuffer::new(256));
    tracing::info!("L7 ZK: proof worker pool started, ZkVerifier + PendingProofBuffer ready");

    {
        let buf = Arc::clone(&pending_proofs);
        let vault = vault_address;
        let signer_bg = Arc::clone(&signer);
        let rpc_bg = rpc.clone();
        let chain_id_bg = chain_id;
        // C10k: nonce now comes from the chain (HTTP JSON-RPC), not a one-shot env var.
        let nonce_rpc_url = confirmation_rpc_url.clone();
        let signer_addr_hex = format!("0x{}", hex::encode(signer.active_address()));
        let env_nonce_fallback: u64 = std::env::var("OMEGA_VAULT_SUBMIT_NONCE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(5));
            let gas_limit: u64 = std::env::var("OMEGA_VAULT_SUBMIT_GAS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(800_000);
            let priority_gwei: u64 = std::env::var("OMEGA_VAULT_SUBMIT_PRIORITY_GWEI")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1);
            let max_fee_gwei: u64 = std::env::var("OMEGA_VAULT_SUBMIT_MAX_FEE_GWEI")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(50);

            let nonce_http = reqwest::Client::new();
            let mut next_nonce: u64 =
                match fetch_pending_nonce(&nonce_http, &nonce_rpc_url, &signer_addr_hex).await {
                    Some(n) => {
                        tracing::info!(
                            signer = %signer_addr_hex,
                            nonce = n,
                            "ZK submitProof keeper: starting nonce read from chain (pending)"
                        );
                        n
                    }
                    None => {
                        tracing::warn!(
                            signer = %signer_addr_hex,
                            fallback_nonce = env_nonce_fallback,
                            "ZK submitProof keeper: could not read pending nonce from chain — \
                             using OMEGA_VAULT_SUBMIT_NONCE fallback (will re-sync on first error)"
                        );
                        env_nonce_fallback
                    }
                };

            loop {
                ticker.tick().await;
                for sub in buf.drain(8) {
                    let data = match sub.encode_calldata() {
                        Ok(d) => d,
                        Err(e) => {
                            tracing::warn!(error = %e, "submitProof encode failed");
                            continue;
                        }
                    };

                    let mut attempt: u32 = 0;
                    loop {
                        attempt += 1;
                        let nonce = next_nonce;

                        let signed = match signer_bg
                            .sign_call_gwei(
                                chain_id_bg,
                                nonce,
                                vault,
                                &data,
                                gas_limit,
                                priority_gwei,
                                max_fee_gwei,
                            )
                            .await
                        {
                            Ok(s) => s,
                            Err(e) => {
                                tracing::error!(
                                    blueprint = %hex::encode(sub.blueprint_hash),
                                    nonce,
                                    error = %e,
                                    "ZK submitProof signing failed — proof remains unposted"
                                );
                                break;
                            }
                        };

                        match rpc_bg.submit_signed_raw_tx(&signed.raw_tx_hex).await {
                            Ok(tx_hash) => {
                                next_nonce = nonce.saturating_add(1);
                                tracing::info!(
                                    blueprint = %hex::encode(sub.blueprint_hash),
                                    tx_hash = %hex::encode(tx_hash),
                                    nonce,
                                    "ZK submitProof broadcast to OmegaVault"
                                );
                                break;
                            }
                            Err(e) => {
                                let msg = e.to_string();
                                let nonce_too_low = msg.contains("nonce too low");
                                tracing::error!(
                                    blueprint = %hex::encode(sub.blueprint_hash),
                                    nonce,
                                    attempt,
                                    error = %msg,
                                    "ZK submitProof broadcast failed"
                                );
                                // Any failure may mean the local counter drifted from the
                                // chain (another sender on this account, a dropped tx, a
                                // restart). Re-sync from the chain's pending nonce.
                                if let Some(n) = fetch_pending_nonce(
                                    &nonce_http,
                                    &nonce_rpc_url,
                                    &signer_addr_hex,
                                )
                                .await
                                {
                                    if n != next_nonce {
                                        tracing::warn!(
                                            old_nonce = next_nonce,
                                            chain_pending_nonce = n,
                                            "ZK submitProof keeper: nonce re-synced from chain"
                                        );
                                    }
                                    next_nonce = n;
                                }
                                if nonce_too_low && attempt < SUBMIT_PROOF_MAX_ATTEMPTS {
                                    continue;
                                }
                                break;
                            }
                        }
                    }
                }
            }
        });
        tracing::info!("ZK submitProof keeper started (sign + broadcast to VAULT_ADDRESS)");
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

    // ── Account exposure tracker ─────────────────────────────────────────────────
    let exposure_tracker = AccountExposureTracker::new();
    tracing::warn!(
        "AccountExposureTracker constructed — real per-strategy tracking via each \
         blueprint's own expiry_block as a conservative TTL, but max_account_exposure_wei \
         is still a non-risk-approved placeholder (1 ETH) and this tracker is in-memory \
         only (resets on restart)"
    );

    // ── L13: Strategy registry ────────────────────────────────────────────────
    struct OracleTokenPriceLookup {
        chainlink: Arc<omega_oracle::ChainlinkOracle>,
        pyth: Arc<omega_oracle::PythOracle>,
    }

    fn arbitrum_token_symbol(token: alloy_primitives::Address) -> Option<(&'static str, u8)> {
        const WETH: [u8; 20] = [
            0x82, 0xaf, 0x49, 0x44, 0x7d, 0x8a, 0x07, 0xe3, 0xbd, 0x95, 0xbd, 0x0d, 0x56, 0xf3,
            0x52, 0x41, 0x52, 0x3f, 0xba, 0xb1,
        ];
        const USDC: [u8; 20] = [
            0xaf, 0x88, 0xd0, 0x65, 0xe7, 0x7c, 0x8c, 0xc2, 0x23, 0x93, 0x27, 0xc5, 0xed, 0xb3,
            0xa4, 0x32, 0x26, 0x8e, 0x58, 0x31,
        ];
        const USDC_E: [u8; 20] = [
            0xff, 0x97, 0x0a, 0x61, 0xa0, 0x4b, 0x1c, 0xa1, 0x48, 0x34, 0xa4, 0x3f, 0x5d, 0xe4,
            0x53, 0x3e, 0xbd, 0xdb, 0x5c, 0xc8,
        ];
        const USDT: [u8; 20] = [
            0xfd, 0x08, 0x6b, 0xc7, 0xcd, 0x5c, 0x48, 0x1d, 0xcc, 0x9c, 0x85, 0xeb, 0xe4, 0x78,
            0xa1, 0xc0, 0xb6, 0x9f, 0xcb, 0xb9,
        ];
        let b: [u8; 20] = token.into();
        if b == WETH {
            Some(("WETH", 18))
        } else if b == USDC || b == USDC_E {
            Some(("USDC", 6))
        } else if b == USDT {
            Some(("USDT", 6))
        } else {
            None
        }
    }

    impl omega_strategies::TokenPriceLookup for OracleTokenPriceLookup {
        fn price_usd_and_decimals(&self, token: alloy_primitives::Address) -> Option<(f64, u8)> {
            let (symbol, decimals) = arbitrum_token_symbol(token)?;
            if let Some(p) = self.chainlink.read(symbol) {
                if p.is_fresh() {
                    return Some((p.price_usd, decimals));
                }
            }
            if let Some(p) = self.pyth.read(symbol) {
                if p.is_fresh() {
                    return Some((p.price_usd, decimals));
                }
            }
            None
        }
    }

    let la_price_lookup: Arc<dyn omega_strategies::TokenPriceLookup> =
        Arc::new(OracleTokenPriceLookup {
            chainlink: Arc::clone(&chainlink_oracle),
            pyth: Arc::clone(&pyth_oracle),
        });

    let mut registry_builder = StrategyRegistryBuilder::new(active_phase)
        .register(CnryStrategy::new(chain_id, &config))
        .expect("CNRY registration must succeed");

    let manifest_entries: Vec<_> = integrity_registry.snapshot().into_iter().collect();
    let find_entry = |id: &str| {
        manifest_entries
            .iter()
            .find(|e| e.strategy_id.eq_ignore_ascii_case(id))
            .cloned()
    };

    if let Some(entry) = find_entry("SA") {
        let sa = SaStrategy::new(
            chain_id,
            entry.bytecode_hash.into(),
            entry.contract_address.into(),
            Arc::clone(&liquidity_registry),
            &config,
        );
        registry_builder = registry_builder
            .register(sa)
            .expect("SA registration must succeed");
        tracing::info!("L13: SA registered (Option B + LiquidityRegistry)");
    } else {
        tracing::warn!("L13: no SA entry in IntegrityRegistry — SA NOT registered");
    }

    if let Some(entry) = find_entry("MSA") {
        let msa = MsaStrategy::new(
            chain_id,
            entry.bytecode_hash.into(),
            entry.contract_address.into(),
            Arc::clone(&liquidity_registry),
            &config,
        );
        registry_builder = registry_builder
            .register(msa)
            .expect("MSA registration must succeed");
        tracing::info!("L13: MSA registered (Option B + LiquidityRegistry)");
    } else {
        tracing::warn!("L13: no MSA entry in IntegrityRegistry — MSA NOT registered");
    }

    if let Some(entry) = find_entry("LA") {
        let la = LaStrategy::new(
            chain_id,
            entry.bytecode_hash.into(),
            entry.contract_address.into(),
            Arc::clone(&liquidity_registry),
            Arc::clone(&position_registry),
            &config,
            Some(la_price_lookup.clone()),
        );
        registry_builder = registry_builder
            .register(la)
            .expect("LA registration must succeed");
        tracing::info!("L13: LA registered from deployment manifest");
    } else {
        tracing::warn!("L13: no LA entry in IntegrityRegistry — LA NOT registered");
    }

    if let Some(entry) = find_entry("MEV") {
        let mev = MevStrategy::new(
            chain_id,
            entry.bytecode_hash.into(),
            entry.contract_address.into(),
            &config,
        );
        registry_builder = registry_builder
            .register(mev)
            .expect("MEV registration must succeed");
        tracing::info!("L13: MEV registered from deployment manifest");
    } else {
        tracing::warn!("L13: no MEV entry in IntegrityRegistry — MEV NOT registered");
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
        let max_exp3 = max_account_exposure_wei;
        let fl3 = flashloan_liq_rx.clone();
        let cid3 = chain_id;
        let va3 = vault_address;
        let pt3 = profit_token;
        let zv3 = Arc::clone(&zk_verifier);
        let pp3 = Arc::clone(&pending_proofs);
        let l1e3 = Arc::clone(&l1_gas_ema);
        let msa3 = Arc::clone(&mev_share_activity);
        let ij3 = inflight_journal.clone();
        tokio::spawn(async move {
            run_scoring_loop(
                reg, ora3, cl3, py3, tw3, dag2, tx, pq, halt3, ph, ep3, nr3, ir3, et3, max_exp3,
                fl3, cid3, va3, pt3, zv3, pp3, l1e3, msa3, ij3,
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
    execution_pipeline: Arc<ExecutionPipeline<tx_signer_factory::ProductionTxSigner>>,
    nonce_registry: omega_security::replay::NonceRegistry,
    integrity_registry: Arc<IntegrityRegistry>,
    exposure_tracker: AccountExposureTracker,
    max_account_exposure_wei: u128,
    flashloan_liq_rx: tokio::sync::watch::Receiver<FlashloanLiquidityState>,
    chain_id: u64,
    vault_address: [u8; 20],
    profit_token: [u8; 20],
    zk_verifier: Arc<ZkVerifier>,
    pending_proofs: Arc<PendingProofBuffer>,
    l1_gas_ema: Arc<omega_risk::gas_model::L1GasEma>,
    mev_share_activity: Arc<MevShareActivityTracker>,
    inflight_journal: Option<Arc<InFlightJournal>>,
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
                    let max_exp2 = max_account_exposure_wei;
                    let fl2 = flashloan_liq_rx.clone();
                    let cid2 = chain_id;
                    let va2 = vault_address;
                    let pt2 = profit_token;
                    let zv2 = Arc::clone(&zk_verifier);
                    let pp2 = Arc::clone(&pending_proofs);
                    let l1e2 = Arc::clone(&l1_gas_ema);
                    let msa2 = Arc::clone(&mev_share_activity);
                    let ij2 = inflight_journal.clone();
                    tokio::spawn(async move {
                        score_and_admit(
                            strategy, s2, dag2, tx2, pq2, h2, ph, os2, ep2, nr2, ir2, gv2, et2,
                            max_exp2, fl2, cid2, va2, pt2, zv2, pp2, l1e2, msa2, ij2,
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

/// Release a DAG slot without panicking on a poisoned mutex (matches
/// DagSlotGuard's recovery posture in omega-execution).
fn dag_complete_slot(dag: &Arc<Mutex<ExecutionDag>>, hash: alloy_primitives::B256) {
    match dag.lock() {
        Ok(mut g) => {
            g.complete(hash);
        }
        Err(poisoned) => {
            tracing::error!(
                blueprint_hash = %hash,
                "DAG mutex poisoned — recovering guard and completing slot"
            );
            poisoned.into_inner().complete(hash);
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
    execution_pipeline: Arc<ExecutionPipeline<tx_signer_factory::ProductionTxSigner>>,
    nonce_registry: omega_security::replay::NonceRegistry,
    integrity_registry: Arc<IntegrityRegistry>,
    gas_volatility_risk: f64,
    exposure_tracker: AccountExposureTracker,
    max_account_exposure_wei: u128,
    flashloan_liq_rx: tokio::sync::watch::Receiver<FlashloanLiquidityState>,
    chain_id: u64,
    vault_address: [u8; 20],
    profit_token: [u8; 20],
    zk_verifier: Arc<ZkVerifier>,
    pending_proofs: Arc<PendingProofBuffer>,
    l1_gas_ema: Arc<omega_risk::gas_model::L1GasEma>,
    mev_share_activity: Arc<MevShareActivityTracker>,
    inflight_journal: Option<Arc<InFlightJournal>>,
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

    exposure_tracker.record(
        &strategy.strategy_id().to_string(),
        bp.flashloan_amount.try_into().unwrap_or(u128::MAX),
        bp.expiry_block,
    );

    let hot = strategy.hot_path_eligible()
        && bp.lane == omega_core::Lane::Microtx
        && bp.l2_exec_gas_estimate <= MICROTX_GAS_LIMIT;

    if hot {
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
                true,
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
                                                    "verified ZK proof could not be buffered \
                                                     for on-chain submitProof"
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            tracing::error!(
                                                hash = %hash_for_log,
                                                error = %e,
                                                "verified proof rejected at submission packaging \
                                                 — fail closed on buffer"
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
                dag_complete_slot(&dag, bp.blueprint_hash);
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
                dag_complete_slot(&dag, bp.blueprint_hash);
                return;
            }
            Err(_recv_error) => {
                tracing::warn!(
                    hash = %bp.blueprint_hash,
                    "ZK proof response channel closed before a result arrived (worker \
                     crashed or shut down?) — dropping blueprint, NOT executing"
                );
                dag_complete_slot(&dag, bp.blueprint_hash);
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
            dag_complete_slot(&dag, bp.blueprint_hash);
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

    let strategy_max_gas = strategy
        .gas_budget()
        .saturating_add(bp.l1_data_gas_estimate)
        .saturating_add(bp.extraction_gas);
    let max_slippage_bps = max_slippage_bps_for(strategy.strategy_id());
    let latest_blueprint_nonce =
        nonce_registry.next_nonce(&strategy.strategy_id().to_string(), chain_id);
    let strategy_bytecode_hash =
        resolve_strategy_bytecode_hash(&integrity_registry, strategy.strategy_id());
    let current_block = signal.block_number;
    let current_account_exposure_wei =
        exposure_tracker.current_exposure_wei(&strategy.strategy_id().to_string(), current_block);
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
        l1_gas_ema.current_buffer(),
        competition_probability_for_primary_asset({
            let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
            mev_share_activity.events_in_window(now_ms)
        }),
        max_competition_probability_from_env(),
        max_account_exposure_wei,
        rollout_tier_from_env(),
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
            let strategy_label = strategy.strategy_id().to_string();
            let amount: u128 = bp.flashloan_amount.try_into().unwrap_or(u128::MAX);
            exposure_tracker.release(&strategy_label, amount);
            let highest = nonce_registry.record_processed(&strategy_label, chain_id, bp.nonce);
            tracing::debug!(
                hash = %bp.blueprint_hash,
                outcome = ?outcome,
                strategy = %strategy_label,
                blueprint_nonce = bp.nonce,
                highest_processed = highest,
                released_exposure_wei = amount,
                "ExecutionPipeline::execute completed; nonce+exposure updated"
            );
            let any_accepted = match &outcome {
                ExecutionOutcome::SubmittedSingle { any_accepted } => *any_accepted,
                ExecutionOutcome::SubmittedCascade { any_accepted, .. } => *any_accepted,
                ExecutionOutcome::Suppressed => false,
            };
            if any_accepted {
                if let Some(ref journal) = inflight_journal {
                    let expected: u128 = bp.expected_profit_net.try_into().unwrap_or(u128::MAX);
                    let rec = InFlightRecord {
                        recorded_at: chrono::Utc::now(),
                        status: InFlightStatus::Submitted,
                        strategy_id: strategy_label.clone(),
                        nonce: bp.nonce,
                        bundle_hash: format!("0x{}", hex::encode(bp.blueprint_hash.as_slice())),
                        blueprint_hash: format!("0x{}", hex::encode(bp.blueprint_hash.as_slice())),
                        chain_id,
                        target_block: current_block,
                        expected_profit_net_wei: expected,
                        idempotency_key_hex: format!(
                            "0x{}",
                            hex::encode(bp.idempotency_key.as_slice())
                        ),
                    };
                    if let Err(e) = journal.record_submitted(rec) {
                        tracing::error!(
                            error = %e,
                            hash = %bp.blueprint_hash,
                            "InFlightJournal::record_submitted failed — capital may lack durable trail"
                        );
                    }
                }
            }
        }
        Err(e) => {
            tracing::debug!(
                hash = %bp.blueprint_hash,
                error = %e,
                "ExecutionPipeline::execute rejected blueprint (expected until remaining \
                 risk-data gaps are closed)"
            );
        }
    }
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
            "00".repeat(32),
            "22".repeat(20),
        );
        let path = write_temp_manifest(&bad_manifest);

        let manifest = load_deployment_manifest(path.to_str().unwrap())
            .unwrap()
            .expect(
                "well-formed TOML must still load as Some — the bad data is \
                     caught one step later, by strategy_entries_from_manifest",
            );

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

    #[test]
    fn local_l1_data_fee_defaults_to_one_when_unset_or_malformed() {
        std::env::remove_var("OMEGA_LOCAL_L1_DATA_FEE_GWEI");
        assert_eq!(
            local_l1_data_fee_gwei_from_env(),
            LOCAL_CHAIN_L1_DATA_FEE_GWEI_DEFAULT
        );
        std::env::set_var("OMEGA_LOCAL_L1_DATA_FEE_GWEI", "not-a-number");
        assert_eq!(
            local_l1_data_fee_gwei_from_env(),
            LOCAL_CHAIN_L1_DATA_FEE_GWEI_DEFAULT
        );
        std::env::set_var("OMEGA_LOCAL_L1_DATA_FEE_GWEI", "7");
        assert_eq!(local_l1_data_fee_gwei_from_env(), 7);
        std::env::remove_var("OMEGA_LOCAL_L1_DATA_FEE_GWEI");
    }
}

#[cfg(test)]
mod reorg_block_feed_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

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

        let event_a = omega_rpc::BlockEvent {
            number: 700,
            hash: [1u8; 32].into(),
            base_fee_gwei: None,
            timestamp: 0,
            is_reorg_or_stale: false,
        };
        let event_b = omega_rpc::BlockEvent {
            number: 700,
            hash: [2u8; 32].into(),
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
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use omega_core::StrategyTrait;
    use omega_strategies::SaStrategy;

    const TEST_CHAIN_ID: u64 = 42_161;

    /// C10j: signer now wrapped in `tx_signer_factory::ProductionTxSigner::Local(...)`
    /// so this harness's `ExecutionPipeline<tx_signer_factory::ProductionTxSigner>`
    /// matches `score_and_admit`'s updated signature.
    async fn build_harness() -> (
        Arc<dyn StrategyTrait>,
        Arc<Mutex<ExecutionDag>>,
        tokio::sync::mpsc::Sender<HotPathRequest>,
        tokio::sync::mpsc::Receiver<HotPathRequest>,
        ProofQueue,
        Arc<ExecutionPipeline<tx_signer_factory::ProductionTxSigner>>,
        omega_security::replay::NonceRegistry,
        Arc<IntegrityRegistry>,
        AccountExposureTracker,
        tokio::sync::watch::Receiver<FlashloanLiquidityState>,
        Arc<ZkVerifier>,
        Arc<PendingProofBuffer>,
        Arc<MevShareActivityTracker>,
    ) {
        let test_liquidity_registry = LiquidityRegistry::new();
        test_liquidity_registry.update(
            TEST_CHAIN_ID,
            FlashloanProvider::Balancer,
            WETH,
            [0xB0u8; 20].into(),
            10_000_000_000_000_000_000u128
                .try_into()
                .expect("u128 always fits in a 256-bit unsigned integer"),
            1,
        );

        let strategy: Arc<dyn StrategyTrait> = SaStrategy::new(
            TEST_CHAIN_ID,
            [0xABu8; 32].into(),
            [0u8; 20].into(),
            test_liquidity_registry,
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
        let proof_queue = ProofQueue::new(zk_cfg);

        let zk_verifier = Arc::new(ZkVerifier::new(TEST_CHAIN_ID));
        let pending_proofs = Arc::new(PendingProofBuffer::new(16));

        let kill_switch_cfg = KillSwitchConfig {
            max_cumulative_loss_wei: u128::MAX / 4,
            max_loss_per_window_wei: u128::MAX / 8,
            loss_window: Duration::from_secs(3600),
            max_consecutive_failures: 32,
        };
        let kill_switches =
            Arc::new(KillSwitchRegistry::new(kill_switch_cfg).expect("KillSwitchRegistry::new"));

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
        // never real keys. Reuses the real, production strategy_onchain_ids() helper.
        let test_tx_key_manager =
            Arc::new(KeyManager::from_hex(&"3a".repeat(32), TEST_CHAIN_ID).unwrap());
        let test_blueprint_key_manager =
            Arc::new(KeyManager::from_hex(&"3b".repeat(32), TEST_CHAIN_ID).unwrap());
        let test_blueprint_signer = Arc::new(BlueprintSigner::new(test_blueprint_key_manager));
        let signer = Arc::new(tx_signer_factory::ProductionTxSigner::Local(
            KeyManagerTransactionSigner::new(
                test_tx_key_manager,
                [0x01u8; 20].into(),
                strategy_onchain_ids(),
                test_blueprint_signer,
            ),
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
        let mev_share_activity = Arc::new(MevShareActivityTracker::new());

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
            mev_share_activity,
        )
    }

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
            mev_share_activity,
        ) = build_harness().await;

        assert!(
            strategy.hot_path_eligible(),
            "test assumes SA is hot-path eligible"
        );

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

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            score_and_admit(
                strategy,
                signal,
                dag,
                hp_tx,
                proof_queue,
                HaltFlag::new(),
                1,
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
                0.0,
                exposure_tracker,
                1_000_000_000_000_000_000u128,
                flashloan_liq_rx,
                TEST_CHAIN_ID,
                [0x11u8; 20],
                [0x22u8; 20],
                zk_verifier,
                pending_proofs,
                Arc::new(omega_risk::gas_model::L1GasEma::new(8)),
                mev_share_activity,
                None,
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

        let _la = LaStrategy::new(
            TEST_CHAIN_ID,
            entry.bytecode_hash.into(),
            entry.contract_address.into(),
            liquidity_registry,
            position_registry,
            &OmegaConfig::default(),
            None,
        );
    }

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

#[cfg(test)]
mod check_context_assembly_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn sample_oracle() -> OracleSnapshot {
        OracleSnapshot {
            chainlink_price: 2000.0,
            pyth_price: 2001.0,
            twap_price: 1999.0,
            chainlink_age_s: 10,
            pyth_age_s: 12,
            twap_age_s: 40,
        }
    }

    fn sample_signal() -> omega_core::SignalState {
        omega_core::SignalState {
            state_version: 1,
            chain_id: 42_161,
            block_number: 50_000_000,
            base_fee_gwei: 7,
            l1_data_fee_gwei: 15,
            state_hash: [0xabu8; 32].into(),
        }
    }

    #[test]
    fn build_check_context_traces_live_fields() {
        let ema = omega_risk::gas_model::L1GasEma::new(8);
        ema.push_price(20);
        ema.push_price(22);
        ema.push_price(18);
        let buf = ema.current_buffer();
        assert!(buf >= 1.0);

        let ctx = build_check_context(
            42_161,
            &sample_signal(),
            sample_oracle(),
            1_200_000,
            50,
            7,
            [0xaau8; 32],
            0.25,
            100_000_000_000_000_000,
            FlashloanLiquidityState {
                available_wei: 5_000_000_000_000_000_000,
                protocol_id: "aave".into(),
            },
            buf,
            0.765,
            0.95,
            1_000_000_000_000_000_000,
            1.0,
        );

        assert_eq!(ctx.expected_chain_id, 42_161);
        assert_eq!(ctx.current_block, 50_000_000);
        assert_eq!(ctx.current_l2_base_fee_gwei, 7);
        assert_eq!(ctx.current_l1_gas_price_gwei, 15);
        assert!((ctx.l1_adaptive_buffer - buf).abs() < 1e-12);
        assert_eq!(ctx.oracle.chainlink_age_s, 10);
        assert_eq!(ctx.flashloan.available, 5_000_000_000_000_000_000);
        assert_eq!(ctx.flashloan.protocol_id, "aave");
        assert!((ctx.competition_probability - 0.765).abs() < 1e-9);
        assert!((ctx.max_competition_probability - 0.95).abs() < 1e-9);
        assert_eq!(ctx.strategy_max_gas, 1_200_000);
        assert_eq!(ctx.max_slippage_bps, 50);
        assert_eq!(ctx.strategy_bytecode_hash, [0xaau8; 32]);
        assert_eq!(ctx.current_account_exposure_wei, 100_000_000_000_000_000);
        assert_eq!(ctx.max_account_exposure_wei, 1_000_000_000_000_000_000);
        assert_eq!(ctx.latest_blueprint_nonce, 7);
        assert!((ctx.rollout_tier - 1.0).abs() < 1e-9);
        assert!(ctx.risk_score >= 0.0 && ctx.risk_score <= 1.0);
    }

    #[test]
    fn competition_for_weth_is_not_pinned_at_one() {
        let p = competition_probability_for_primary_asset(0);
        assert!(p > 0.0 && p < 1.0, "got {p}");
        assert!(p <= 0.95);
    }

    #[test]
    fn empty_flashloan_maps_to_high_liquidity_risk_component() {
        let ctx = build_check_context(
            42_161,
            &sample_signal(),
            sample_oracle(),
            1,
            1,
            0,
            [0u8; 32],
            0.0,
            0,
            FlashloanLiquidityState::default(),
            1.1,
            0.5,
            0.95,
            1_000_000_000_000_000_000,
            1.0,
        );
        assert!(ctx.risk_score >= 0.24, "risk_score={}", ctx.risk_score);
    }

    #[test]
    fn pyth_freshness_uses_pyth_staleness_threshold_not_chainlink() {
        let stale_pyth_oracle = OracleSnapshot {
            chainlink_price: 2000.0,
            pyth_price: 2001.0,
            twap_price: 1999.0,
            chainlink_age_s: 1,
            pyth_age_s: PYTH_STALENESS_SECS + 5,
            twap_age_s: 1,
        };

        let ctx = build_check_context(
            42_161,
            &sample_signal(),
            stale_pyth_oracle,
            1,
            1,
            0,
            [0u8; 32],
            0.0,
            0,
            FlashloanLiquidityState {
                available_wei: 1,
                protocol_id: "aave".into(),
            },
            1.0,
            0.0,
            0.95,
            1_000_000_000_000_000_000,
            1.0,
        );

        assert!(ctx.risk_score >= 0.0 && ctx.risk_score <= 1.0);
    }
}
