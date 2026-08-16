// src/main.rs — OmegaEngine v12.0 Main Entry Point
//
// Required: ARBITRUM_RPC_URL (WebSocket endpoint)
// Required (this revision, C5): ARBITRUM_HTTP_RPC_URL (plain HTTP JSON-RPC
//   endpoint — see this file's own "C5" doc comment for why this is a
//   separate, required variable from ARBITRUM_RPC_URL rather than reusing it)
// Optional: OMEGA_CONFIG (default: config/default.toml)
// Optional (this revision, C5): OMEGA_RELAY_ENDPOINT_<NAME> per relay (e.g.
//   OMEGA_RELAY_ENDPOINT_FLASHBOTS), FLASHBOTS_AUTH_KEY, TITAN_AUTH_KEY,
//   BLOXROUTE_AUTH_TOKEN, EDEN_AUTH_TOKEN, OMEGA_EXECUTION_ADDRESS — see the
//   "C5" doc comment below for exactly how each is used and what happens
//   when one is absent (the relay in question is skipped, never faked).
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
// Fail-closed stand-ins used here, per C1's explicit rule ("no invented
// production values"):
//   - `KillSwitchRegistry`: real struct, non-production threshold
//     numbers. Gap 5 replaces these.
//   - `IntegrityRegistry`: real. As of C4 this is no longer
//     unconditionally empty — see the "C4" doc comment below.
//   - `MultiRelayClient`: as of C5 (this revision) constructed from real
//     `HttpRelayClient`s when endpoints/secrets are present in the
//     environment — see the "C5" doc comment below. Still falls back to
//     a zero-entry map (same fail-closed posture as before) when none
//     are configured.
//   - `UnconfiguredSigner`: the real fail-closed signer type
//     `omega_execution::lib.rs` names explicitly for exactly this
//     purpose. Still in use — C6 (transaction signing) has not started;
//     it is explicitly gated on an orchestrator ABI/schema this file
//     does not have yet.
//
// ## C2 (this revision): DAG/execution ownership
//
// Wires `ExecutionPipeline::execute()` into `score_and_admit`, and
// removes that function's own `dag.complete()` call — DAG slot release
// is now owned exclusively by `execute()`'s internal `DagSlotGuard`.
//
// ## C4 (this revision): real deployment manifest loading + strategy
// bytecode hash sourced from IntegrityRegistry (Gap 6, partial)
//
// `IntegrityRegistry` is no longer left permanently empty. At startup,
// `load_deployment_manifest(DEPLOYMENT_MANIFEST_PATH)` attempts to read
// and parse a real `omega_security::DeploymentManifest` TOML file. Three
// outcomes, all handled explicitly rather than silently defaulting:
//   - File exists, parses, all entries valid → `strategy_entries_from_
//     manifest` (real, already existed in integrity.rs) filters by
//     `active_phase` and validates hex/non-placeholder data exactly as
//     it always has; `register_all` populates the registry for real.
//   - File exists but fails to parse, or contains ANY invalid entry
//     (malformed hex, wrong length, all-zero placeholder) →
//     `strategy_entries_from_manifest`'s own existing "one bad entry
//     fails the whole call" behavior propagates via `?`, and `main()`
//     returns `Err` — startup HALTS rather than silently running with a
//     partially-registered or all-placeholder registry. This is a
//     deliberate escalation from Gap 6's prior "just empty, warn and
//     continue" state: a malformed manifest actively present on disk is
//     a worse signal than no manifest at all, and should not be treated
//     the same.
//   - No file at the conventional path → unchanged from the prior
//     revision: log a warning, continue with an empty registry (every
//     strategy_id fails Stage 2b as `StrategyUnknown`, same fail-closed
//     behavior as before this revision).
//
// STILL NOT DONE (C4-A, blocked on the operator/deploy pipeline, not on
// code in this file): this revision does not fabricate, guess at, or
// supply any deployed contract address or bytecode hash — no `config/
// deployment_manifest.toml` is created here (unlike the empty
// builder-blacklist file this same function block writes on-demand).
// A real manifest has to come from an actual deployment (forge output
// or an on-chain `eth_getCode` read) and be placed at
// `DEPLOYMENT_MANIFEST_PATH` by whoever owns that step. The LOADING
// mechanism is complete and real; the DATA is not something this
// codebase can supply for itself. This is also, as of this revision,
// confirmed to be the SAME blocker preventing LA from being registered
// in the strategy registry below — see the "C7" doc comment further
// down for the full, now-evidence-checked chain of gaps.
//
// NOT DONE, AND DELIBERATELY NOT ATTEMPTED: calling `IntegrityRegistry::
// freeze()` on every newly-registered strategy, which an earlier
// planning note for this task described as step C ("freeze after
// register"). Read literally against `integrity.rs`'s own real
// semantics, that would immediately and permanently disable every
// strategy the manifest just authorized — `freeze()` means "this
// strategy_id may never execute again, and cannot be unfrozen
// programmatically," not "the registry is now sealed against further
// writes." `IntegrityRegistry` has no concept of the latter. Freezing
// belongs to a real governance action (per integrity.rs's own doc
// comment: "called by the L2 governance handler after a signed freeze
// proposal") — a control-plane API this codebase doesn't have yet, not
// a step in the normal startup path. Nothing is frozen here.
//
// Also this revision: `build_check_context`'s `strategy_bytecode_hash`
// field (check 4's whitelist comparison) is now sourced from
// `IntegrityRegistry::snapshot()` — the SAME registry Stage 2b already
// reads — instead of a hardcoded `[0u8; 32]` placeholder. This
// supersedes the prior revision's plan to add a new accessor to
// `omega_risk::whitelist::BytecodeWhitelist`: that would have built a
// second, redundant lookup path for data `IntegrityRegistry` already
// exposes via a method (`snapshot()`) that already exists and is
// already used elsewhere in this file. When no manifest is loaded (or a
// given strategy_id isn't in it), this still resolves to `[0u8; 32]`,
// preserving the exact same fail-closed behavior as before — the only
// change is that a REAL registered hash is now used when one exists,
// rather than the placeholder always winning regardless of registry
// state.
//
// ## C5 (this revision): real L1 data fee via ArbGasInfo
//
// Closes the last hardcoded-0 gas-path placeholder: a new "L2d" poll
// loop (15s interval, same cadence as L2c's Chainlink poll) calls the
// new `OmegaRpcClient::fetch_l1_base_fee_estimate_gwei` (omega-rpc's new
// `arb_gas_info.rs` — a real `eth_call` against Arbitrum's documented
// ArbGasInfo precompile at `0x...006C`, same `SolCall`-without-
// `#[sol(rpc)]` pattern already established by `chainlink_agg.rs`) and
// feeds the result into `PerChainOracle::update_l1_data_fee_gwei` (a new
// method on that struct — updates only the `fee.l1_data_fee_gwei` field
// in place, does not bump `state_version` or emit a new `OracleSignal`,
// since a periodic fee poll is not itself a discrete oracle event; see
// that method's own doc comment). `build_check_context`'s
// `current_l1_gas_price_gwei` now reads `sig.l1_data_fee_gwei` (which
// flows from this) instead of an unconditional `u64::MAX` placeholder —
// see that field's own comment for why an absent/failed reading still
// fails closed correctly (0 vs. a nonzero `l1_data_fee_at_creation`
// still trips check 6's gas-spike guard).
//
// Also fixed as part of this same change: `omega-oracle/src/
// per_chain.rs`'s `run_fee_oracle` previously hardcoded
// `l1_data_fee_gwei: 0` on every `FeeOracleEvent` (an L2-base-fee signal,
// independent of the L1 poll) — left as-is, this would have silently
// clobbered whatever real value the ArbGasInfo poll loop had just set,
// on every L2 base-fee update. Fixed to carry forward the current
// snapshot's `l1_data_fee_gwei` instead of resetting it to 0.
//
// ## C5 (this revision, separate task): relay production bootstrap
//
// Replaces the C1 zero-relay `MultiRelayClient` stub with real
// `HttpRelayClient`s, constructed from `RelayConfig` (which relays are
// active for the current `active_phase`) plus per-relay secrets read
// from the environment. Design decisions, stated up front rather than
// left implicit:
//
//   - **Endpoints are never hardcoded.** This engine runs on Arbitrum
//     (`CHAIN_ID = 42_161`), not L1 — there is no verified, current
//     record in this codebase of each provider's Arbitrum-specific
//     bundle-submission endpoint, and guessing one would silently
//     misroute real signed bundles to the wrong place, or to an L1
//     endpoint that doesn't understand Arbitrum bundles at all. Each
//     relay's endpoint is read from `OMEGA_RELAY_ENDPOINT_<NAME>`
//     (upper-cased relay name, e.g. `OMEGA_RELAY_ENDPOINT_FLASHBOTS`).
//     A relay with no matching env var is skipped (`warn!`), never
//     constructed against a fabricated URL.
//   - **Auth follows `signing.rs`'s own documented provider mapping,
//     nothing more.** Flashbots/Titan → `RelayAuth::flashbots_style`
//     (`FLASHBOTS_AUTH_KEY` / `TITAN_AUTH_KEY`, a raw `0x`-prefixed
//     private key hex, per that module's own doc comment on what these
//     two providers expect). bloXroute/Eden → `RelayAuth::BearerToken`
//     (`BLOXROUTE_AUTH_TOKEN` / `EDEN_AUTH_TOKEN`). A relay whose secret
//     is absent is skipped entirely — never constructed with
//     `RelayAuth::None`, since `signing.rs`'s own module doc comment is
//     explicit that unauthenticated submission to these providers
//     either gets rejected outright or silently loses reputation
//     credit, and this file should not let either happen quietly.
//   - **`RelayName::Other(_)` is always skipped, with an error log.**
//     No verified auth convention exists for an arbitrary relay name;
//     `signing.rs` already declines to guess at unverified
//     provider-specific details (see its own "NOT VERIFIED" note on
//     bloXroute/Eden's exact bearer header format) — this file
//     shouldn't go further than that module already goes.
//   - **`confirmation_rpc_url` is a genuinely new, separate requirement:
//     `ARBITRUM_HTTP_RPC_URL`.** `InclusionTracker` (omega-relay's
//     `confirmation.rs`) issues plain HTTP JSON-RPC POSTs
//     (`eth_getTransactionReceipt`) via `reqwest::Client`. The only RPC
//     URL this file already has, `ARBITRUM_RPC_URL`, is documented at
//     the top of this file as a **WebSocket** endpoint — pointing
//     `InclusionTracker` at it would not work. Missing
//     `ARBITRUM_HTTP_RPC_URL` halts startup (`main()` returns `Err`)
//     rather than constructing `InclusionTracker` against an empty or
//     wrong string: without it, `reconcile_inclusions` could never
//     resolve a bundle to `included: true`, which would silently
//     flatten every relay's measured inclusion rate to 0% forever — a
//     worse failure mode than refusing to start.
//   - **`ExecutionAddress` (relay-metrics identity) is still not backed
//     by a real signer.** C6 (KeyManager / transaction signing) has not
//     started — it's explicitly gated on an orchestrator ABI/schema not
//     yet available. This revision reads `OMEGA_EXECUTION_ADDRESS` if
//     set (a plain label used only for metrics/carryover bookkeeping,
//     not a signing capability) and falls back to the same
//     `"0xC1_UNCONFIGURED"` placeholder C1 used, now logged explicitly
//     as still a placeholder rather than silently reused.
//   - **`startup_block` is still `0`.** A real value needs the current
//     chain height at connect time; no method to read that
//     synchronously off the already-connected `rpc` client is visible
//     in this file. Flagged here rather than guessed at.
//
// NOT DONE, deliberately, and explained rather than silently skipped:
// **reorg-guard block wiring (`MultiRelayClient::on_new_block`)**. That
// method needs a real `(block_number, block_hash)` pair per new
// canonical block. Nothing currently wired in this file exposes a block
// hash back to `main()` — `run_block_subscription()` (L1, spawned
// earlier) is fire-and-forget with no channel out, and
// `PerChainOracle::snapshot()`'s `FeeSnapshot` (used elsewhere in this
// file) carries a block number but no hash. Fabricating a hash (e.g.
// hashing the block number itself) would produce a reorg detector that
// LOOKS live while detecting nothing real — strictly worse than leaving
// it off, per this file's own established convention (see C5's L1-fee
// note above, or C4's manifest-vs-no-manifest distinction) of treating
// a fake-but-present signal as worse than an honestly absent one. This
// needs a real cross-crate change (`omega-rpc` surfacing
// `(number, hash)` from its block subscription) before it can be wired
// — not attempted blind in this revision.
//
// `reconcile_inclusions`, by contrast, only needs a block NUMBER (see
// `confirmation.rs`'s own `reconcile(current_block: u64)` signature) —
// so that half of the reconciliation lifecycle IS wired this revision,
// off the same oracle block-number stream `run_scoring_loop` already
// subscribes to.
//
// ## FIX (this revision): omega_core::RelayConfig vs.
// omega_relay::RelayConfig type/field errors
//
// The prior draft of C5's relay-bootstrap block made two unverified
// assumptions about `omega_core::RelayConfig` (the real type of
// `config.relay` — confirmed to exist by the compiler, so that part of
// the assumption was correct) that turned out to be wrong, caught by
// `cargo build`/`cargo check`/`cargo clippy` (E0609 ×2, E0308 ×1):
//
//   1. It assumed `omega_core::RelayConfig` carried `phase_1_relays` /
//      `phase_2plus_relays` fields to select which named relays are
//      active per phase. It does not — the compiler's own diagnostic
//      lists this type's actual fields:
//      `max_bundles_per_relay_per_second`, `cascade_stagger_ms`,
//      `cascade_max_relays`, `inclusion_rate_tie_band_fraction`. No
//      per-phase relay-selection field exists on this type today, and
//      adding one means editing `omega-core::config.rs` — a different
//      crate this file doesn't own — not guessing at more field names
//      here. Fixed: every relay this file has a verified auth
//      convention for (Flashbots/Titan/Bloxroute/Eden — the same four
//      names `signing.rs` documents, matched in the loop below) is now
//      a CANDIDATE for every phase, not phase-gated. This is
//      deliberately the honest absence of a phase policy, not an
//      invented one — if the spec calls for fewer relays active in an
//      earlier phase, that needs a real field on
//      `omega-core::RelayConfig`, wired through here once it exists.
//      Until then, the per-relay endpoint/secret presence checks
//      already in the loop below are what actually gates construction —
//      a relay with no `OMEGA_RELAY_ENDPOINT_<NAME>` or no auth secret
//      set is still skipped regardless of being a "candidate," same
//      fail-closed behavior as every other revision.
//   2. `RelayConfig { confirmation_rpc_url, ..relay_cfg_from_file }`
//      spread `omega_core::RelayConfig`'s fields into a struct-update of
//      `omega_relay::RelayConfig` — two DISTINCT types (defined in two
//      different crates) that merely happen to share a name, per the
//      compiler's own note on this. Fixed: `omega_relay::RelayConfig`
//      is now built from ITS OWN `Default::default()`. Deliberately NOT
//      attempting to hand-map `config.relay`'s numeric fields
//      (`cascade_stagger_ms`, `max_bundles_per_relay_per_second`, etc.)
//      onto whatever differently-named/possibly-differently-typed
//      fields `omega_relay::RelayConfig` has — this file cannot verify
//      those two crates' field types actually line up (a `u32` vs.
//      `u64` mismatch would just trade this compile error for another
//      one, or silently truncate a real operator-configured value), and
//      `omega-execution`'s own `config_translation` module (see its
//      `cascade_max_relays_is_always_reported_unmapped` /
//      `diverging_tie_band_fraction_is_reported_unmapped` tests)
//      already exists specifically to perform this mapping correctly
//      and flag exactly this kind of drift — that module, not a guess
//      added here, is where a real `config.relay` → relay-crate
//      translation belongs. `omega_relay::RelayConfig::default()`'s own
//      values are the same known-good defaults every prior revision of
//      this file has already run with successfully.
//
// ## C6 (this revision): risk-score formula constants ──────────────────
//
// See build_check_context's own "C6" doc comment for the full design.
// Equal weighting across the four named components (spec: "incorporates
// gas volatility, oracle freshness, competition, liquidity depth") is a
// POLICY DEFAULT, not derived from any spec section — §8's actual
// weighting model (if one exists) was never pasted into this
// investigation. Revisit if that section surfaces.
//
// NOTE: "C6" is used twice in this file's history for two different
// things — the risk-score formula (described here and in
// build_check_context) and, separately, transaction signing (the C6
// task in ProductionIntegrationPlan.md, not started, gated on the
// orchestrator ABI). Left as-is rather than renumbered, to avoid
// breaking the trail of doc comments already cross-referencing each
// other by this label; context disambiguates which "C6" is meant at
// each site.
//
// ## C7 (this revision): account exposure cap
//
// See build_check_context's own "C7" doc comment and
// omega_security::exposure's module doc comment for the full design
// (AccountExposureTracker, TTL-by-expiry-block, conservative
// overcounting).
//
// CHOSEN, NOT RISK-APPROVED, VALUE: no real per-strategy or per-account
// exposure policy exists anywhere in this codebase (checked: omega-core
// ::config.rs's VaultConfig has per_transfer_cap_wei/daily_cap_wei, but
// those cap Vault PROFIT release, spec §15.2 — a different concept from
// capital AT RISK, and conflating the two would be exactly the kind of
// same-shaped-field trap this investigation has been careful to avoid
// elsewhere). 1 ETH is a deliberately conservative starting cap — same
// spirit as KillSwitchConfig's placeholder thresholds above (Gap 5):
// flagged, not a policy decision, needs real operator sign-off before
// any real capital is at risk. Unlike KillSwitchConfig's LARGE
// permissive placeholders (losses should be free to accumulate up to a
// generous ceiling before an admittedly-fake threshold trips), this
// leans SMALL — an exposure cap exists specifically to bound capital at
// risk, so erring conservative (rejecting more) is the safer direction
// for an unapproved number, the opposite direction from kill-switch
// placeholders.
//
// ## C7 (this revision, separate task): flashloan integration status —
// verified against real source, not re-litigated blind
//
// `omega-flashloan` itself (provider registry, premium math, real
// ABI-encoded calldata for Aave/Balancer/Uniswap) is genuinely complete
// and tested (22/22 tests green, including a real bug fix in the
// Uniswap token0/token1 encoding). This file's `flashloan.available`
// field below remains `0` — checked this revision to be correct for a
// concrete, evidence-backed reason chain, not left as a stale guess:
//   1. `omega_flashloan::LiquidityRegistry` has NO writer anywhere in
//      this workspace. Checked `omega-oracle/src/per_chain.rs`'s
//      `run_lending_protocol` directly: it publishes `HealthFactor`
//      signals only (liquidation-target health, feeding LA's own
//      scoring) — a different data type from flashloan-provider
//      liquidity (`LiquidityRegistry`'s own doc comment names
//      Supply/Withdraw/Borrow events as its intended input). No
//      ingestion path for the latter exists anywhere in omega-oracle.
//   2. LA (`crates/omega-strategies/src/la.rs`) is the only strategy
//      that calls `omega_flashloan::select_provider` — checked directly.
//      It is NOT registered in this file's `StrategyRegistryBuilder`
//      below (only `CnryStrategy` is). Registering it needs a real
//      `bytecode_hash`/`contract_addr`, exactly the same C4-A blocker
//      (see this file's own "C4" doc comment) already documented for
//      every other non-CNRY strategy — not a new, separate gap.
//   3. Even past (1) and (2), `LaStrategy::build_blueprint` (checked
//      directly, this revision) unconditionally returns `Err` today: it
//      has no real source for `flashloan_token` (which ERC20 the
//      liquidated position's debt is denominated in) and explicitly
//      refuses to fabricate one rather than guess — confirmed by a real,
//      passing regression test in that file,
//      `build_blueprint_fails_without_debt_token_source`.
// SA/MSA/MEV (checked `sa.rs`/`msa.rs`/`mev.rs` directly) do not use
// flashloans by design — each sets `flashloan_provider: Address::ZERO`
// with an explicit `TODO(capital-path)` comment on what a real
// no-flashloan execution path would need — so `flashloan.available: 0`
// is correct for those strategies, not a gap at all.
// NOT DONE, deliberately: constructing a `LiquidityRegistry` here with
// no writer and no registered consumer. That would be dead code with no
// real function — worse than not adding it, per this file's own
// established "a fake-but-present signal is worse than an honestly
// absent one" convention (see the reorg-guard note above for the same
// reasoning applied to a different field). A `LiquidityRegistry`
// belongs in `main()` once LA is actually registerable (needs C4-A)
// AND has a real liquidity-ingestion writer (needs new omega-oracle
// work, not scoped to this file) AND has a real `flashloan_token`
// source (needs the same position-data gap `la.rs` already documents
// resolved first).
//
// ## Fix (this revision, clippy): too_many_arguments on
// build_check_context
//
// C7 added an 8th parameter (`current_account_exposure_wei`) to
// `build_check_context`, crossing clippy's default 7-argument
// threshold, which this workspace's `-D warnings` turns into a hard
// build failure. `run_scoring_loop` and `score_and_admit` already carry
// `#[allow(clippy::too_many_arguments)]` for the exact same reason (both
// were annotated when earlier revisions grew their own parameter counts
// — see run_scoring_loop's own "C4" comment on its allow). This function
// never got the same annotation despite crossing the same threshold. A
// struct-of-params refactor is the "real" fix, but per the same
// reasoning already applied to the other two functions in this file,
// that's a larger refactor than a single added parameter warrants right
// now — allowing the lint here is consistent with the precedent already
// set in this file, not a new exception.
//
// ## C8 (this revision): real flashloan liquidity signal for
// CheckContext.flashloan (Aave V3 + Balancer V2, via omega-rpc's
// flashloan_liq module)
//
// Closes the LAST hardcoded-0 placeholder in build_check_context:
// `flashloan_available_value` was `0` unconditionally (see the C6/C7
// comment on that local, above) because no real liquidity read existed
// anywhere in the workspace. `omega-rpc::flashloan_liq` now provides
// real `eth_call` reads (`fetch_aave_available`, `fetch_balancer_
// available`) against Arbitrum's canonical Aave V3 Protocol Data
// Provider and Balancer V2 Vault — confirmed against that module's own
// real source this revision (not re-guessed). This revision wires those
// into a dedicated "L2e" poll loop (same 15s-interval,
// keep-previous-value-on-error shape as L2d's ArbGasInfo poll) feeding a
// `tokio::sync::watch` channel that `score_and_admit` reads from on
// every scoring cycle, same threading pattern already used for
// `gas_volatility_risk` (C6) and `exposure_tracker` (C7).
//
// **What this is, precisely**: the MAX of the two providers' available
// liquidity for a single tracked asset (`omega_rpc::WETH` — see
// `FlashloanLiquidityState`'s own doc comment for why this must track
// `ORACLE_SNAPSHOT_TOKEN` and the known limitation of that pairing).
//
// **What this is NOT**: this does NOT feed
// `omega_flashloan::LiquidityRegistry` (still unfed — see this file's
// own "C7 (this revision, separate task): flashloan integration
// status" doc comment above, unchanged by C8) and does NOT unblock LA's
// `select_provider` call or its own missing-debt-token guard. Those
// remain exactly as blocked as before. C8 only closes the
// `CheckContext.flashloan` pre-trade SANITY-CHECK field (check 10,
// MissLiquidity) with a real, live, single-asset value instead of an
// unconditional zero — see `FlashloanLiquidityState`'s doc comment for
// the specific limitation of using a MAX-across-providers value for
// that purpose.
//
// ## Fix (this revision, clippy): too_many_arguments on
// build_check_context / score_and_admit / run_scoring_loop, again
//
// C8 adds one more parameter to each of these three functions, all of
// which already carry `#[allow(clippy::too_many_arguments)]` from prior
// revisions for the identical reason (see each function's own comment
// history) — consistent with, not a new exception to, that precedent.

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
// ChainlinkOracle/PythOracle/TwapOracle: real feed caches.
use omega_oracle::{ChainlinkOracle, PerChainOracle, PythOracle, TwapOracle};
// C1: MultiRelayClient + friends.
// C5 (this revision): also RelayAuth + RelayName (crate-root re-exports,
// per omega-relay's lib.rs — RelayAuth comes from the `signing` module,
// RelayName from `config`) — needed to build real per-relay auth and to
// match on the candidate relay list by name. HttpRelayClient itself is
// deliberately NOT in this use list — lib.rs does not re-export it at
// the crate root (only BundlePayload/RelayClient/SubmissionOutcome are),
// so it's referenced via its full path (`omega_relay::client::
// HttpRelayClient`) at the one call site below instead. `RelayConfig`
// here is the omega-relay crate's own type — see this file's module-
// level "FIX (this revision)" doc comment for why it must never be
// confused with `omega_core::RelayConfig`, a distinct type with the
// same name.
use omega_relay::{
    BuilderBlacklist, ExecutionAddress, LaRelayMetrics, MultiRelayClient, RelayAuth, RelayClient,
    RelayConfig, RelayName,
};
// C2: CheckContext + FlashloanSnapshot.
// C6 (this revision): also the three oracle staleness constants
// (CHAINLINK/PYTH/TWAP_STALENESS_SECS), needed to compute the real
// oracle-freshness component of the risk-score formula in
// build_check_context — see that function's own "C6" doc comment.
use omega_risk::context::{
    CheckContext, FlashloanSnapshot, OracleSnapshot, CHAINLINK_STALENESS_SECS,
    PYTH_STALENESS_SECS, TWAP_STALENESS_SECS,
};
// C1: KillSwitchConfig/KillSwitchRegistry.
use omega_risk::kill_switch::{KillSwitchConfig, KillSwitchRegistry};
// C8 (this revision): also WETH — the single asset the new flashloan-
// liquidity poll loop tracks. Re-exported at omega-rpc's crate root from
// its flashloan_liq module (confirmed against that crate's real lib.rs
// this revision: `pub use flashloan_liq::{..., WETH};`).
use omega_rpc::{
    rate_limiter::RpcRateLimiter, run_dex_sync_stream, run_fee_oracle_stream,
    run_lending_protocol_stream, run_mev_share_stream, run_pending_tx_stream, OmegaRpcClient,
    RpcClientConfig, WETH,
};
// C1: IntegrityRegistry.
// C4: DeploymentManifest + strategy_entries_from_manifest — same crate,
// both real, already existed in integrity.rs before this revision; only
// this file's use of them is new.
// C7 (this revision): also AccountExposureTracker — real per-strategy
// exposure tracking for check 14 (see exposure.rs's own doc comment).
use omega_security::{
    strategy_entries_from_manifest, AccountExposureTracker, DeploymentManifest, IntegrityRegistry,
};
use omega_strategies::{registry::StrategyRegistryBuilder, CnryStrategy, StrategyRegistry};
use omega_zk::{config::ProverTierConfig, ProofQueue, ProofWorkerPool, ZkConfig};
// C1: only used for the relay_clients: HashMap<String, Arc<dyn RelayClient>>
// type annotation below — the map itself is intentionally empty until C5
// populates it from real config/secrets.
use std::collections::HashMap;

const CHAIN_ID: u64 = 42_161;
const DEFAULT_RPS: u32 = 500;
const SHUTDOWN_DRAIN_S: u64 = 5;
const DEFAULT_CONFIG: &str = "config/default.toml";

// C1: conventional builder-blacklist path.
const BUILDER_BLACKLIST_PATH: &str = "config/builder_blacklist.toml";

// C4: conventional deployment-manifest path. No default constructor and
// no placeholder file is ever written for this one (unlike
// BUILDER_BLACKLIST_PATH above) — see this file's module-level "C4" doc
// comment for why an absent manifest and a malformed one are handled
// differently, and why neither path fabricates deployment data.
const DEPLOYMENT_MANIFEST_PATH: &str = "config/deployment_manifest.toml";

// C5 FIX (this revision): the set of relay names this file has a
// verified auth convention for (see the module-level "FIX (this
// revision)" doc comment, item 1, and the match on `name` inside
// main()'s relay-bootstrap block below). Every relay here is a
// CANDIDATE for every active_phase — not phase-gated, since
// `omega_core::RelayConfig` has no real per-phase relay-selection field
// to gate on. Actual construction still requires a real
// `OMEGA_RELAY_ENDPOINT_<NAME>` and the matching auth secret to be
// present in the environment; this list alone constructs nothing.
const KNOWN_RELAY_NAMES: [RelayName; 4] = [
    RelayName::Flashbots,
    RelayName::Titan,
    RelayName::Bloxroute,
    RelayName::Eden,
];

// ── C6 (this revision): risk-score formula constants ──────────────────────────
//
// See build_check_context's own "C6" doc comment for the full design.
// Equal weighting across the four named components (spec: "incorporates
// gas volatility, oracle freshness, competition, liquidity depth") is a
// POLICY DEFAULT, not derived from any spec section — §8's actual
// weighting model (if one exists) was never pasted into this
// investigation. Revisit if that section surfaces.
const RISK_WEIGHT_GAS_VOLATILITY: f64 = 0.25;
const RISK_WEIGHT_ORACLE_FRESHNESS: f64 = 0.25;
const RISK_WEIGHT_COMPETITION: f64 = 0.25;
const RISK_WEIGHT_LIQUIDITY: f64 = 0.25;

// Compile-time guard: the four weights above must sum to 1.0, or
// risk_score would systematically over/under-report regardless of its
// inputs. Caught at compile time rather than only by a unit test, since
// this is a structural invariant of the formula itself, not a case the
// formula needs to behave correctly under.
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
/// CHOSEN, NOT DERIVED, VALUE: with `competition_risk` and
/// `liquidity_risk` both still pinned at their fail-closed maximum
/// (1.0 each — see build_check_context's "C6" doc comment for why
/// neither has a real source yet), the MINIMUM possible `risk_score`
/// under equal 0.25 weighting is:
///
///   0.25×0 (best-case gas volatility) + 0.25×0 (best-case oracle
///   freshness) + 0.25×1 (competition, pinned) + 0.25×1 (liquidity,
///   pinned) = 0.50
///
/// Setting this threshold to 0.45 — strictly below that floor —
/// guarantees check 12 still fails closed for every blueprint today,
/// exactly as the prior revision's unconditional `max_risk_score: 0.0`
/// did, but now as an arithmetic consequence of real weights rather
/// than an unconditional placeholder. The day competition_risk and/or
/// liquidity_risk get real sources, this floor calculation changes and
/// this constant should be revisited alongside them — it is NOT
/// automatically correct once those two inputs become real.
///
/// ## C8 addendum (this revision) — the floor above just changed, and
/// this constant was DELIBERATELY NOT re-tuned
///
/// `liquidity_risk` is no longer pinned at 1.0 as of C8 (see
/// `build_check_context`'s "C8" doc comment) — it's now real, and can
/// legitimately be `0.0` when Aave/Balancer WETH liquidity is healthy.
/// With only `competition_risk` still pinned, the best-case floor is now
/// `0.25×0 + 0.25×0 + 0.25×1 + 0.25×0 = 0.25`, not `0.50`. That means
/// check 12 no longer unconditionally fails closed for every blueprint —
/// a blueprint with low gas volatility, fresh oracles, and healthy
/// liquidity can now score under 0.45 and pass. This is a REAL,
/// DELIBERATE consequence of shipping a real liquidity signal, not a
/// bug — but `0.45` itself was never derived from spec even under the
/// old arithmetic (see the paragraph above), and I have not re-derived
/// or re-approved it under the new arithmetic either. Flagging this
/// loudly rather than quietly leaving the constant unchanged and
/// implying nothing about its meaning has shifted: whoever owns risk
/// policy should treat `0.45` as needing a fresh look now that it can
/// actually bind, not as still-conservative-by-construction the way it
/// was through C7.
const RISK_SCORE_MAX_THRESHOLD: f64 = 0.45;

// ── C7 (this revision): account exposure cap ──────────────────────────────────
//
// See build_check_context's own "C7" doc comment and
// omega_security::exposure's module doc comment for the full design
// (AccountExposureTracker, TTL-by-expiry-block, conservative
// overcounting).
//
// CHOSEN, NOT RISK-APPROVED, VALUE: no real per-strategy or per-account
// exposure policy exists anywhere in this codebase (checked: omega-core
// ::config.rs's VaultConfig has per_transfer_cap_wei/daily_cap_wei, but
// those cap Vault PROFIT release, spec §15.2 — a different concept from
// capital AT RISK, and conflating the two would be exactly the kind of
// same-shaped-field trap this investigation has been careful to avoid
// elsewhere). 1 ETH is a deliberately conservative starting cap — same
// spirit as KillSwitchConfig's placeholder thresholds above (Gap 5):
// flagged, not a policy decision, needs real operator sign-off before
// any real capital is at risk. Unlike KillSwitchConfig's LARGE
// permissive placeholders (losses should be free to accumulate up to a
// generous ceiling before an admittedly-fake threshold trips), this
// leans SMALL — an exposure cap exists specifically to bound capital at
// risk, so erring conservative (rejecting more) is the safer direction
// for an unapproved number, the opposite direction from kill-switch
// placeholders.
const MAX_ACCOUNT_EXPOSURE_WEI_PLACEHOLDER: u128 = 1_000_000_000_000_000_000; // 1 ETH

// ── C8 (this revision): flashloan liquidity poll cadence ──────────────────────
//
// Same reasoning as L2d's ArbGasInfo poll: on-chain liquidity pools
// don't move enough block-to-block on a chain like Arbitrum to justify
// polling every block, and 15s (matching L2c/L2d's existing cadence)
// hasn't been measured against real chain behavior to justify a
// tighter or looser interval — a starting value, not a derived one.
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

/// C4: load a real `DeploymentManifest` from `path`, if present.
///
/// Returns `Ok(None)` when the file simply doesn't exist yet — a
/// legitimate, expected state before any deployment has happened (or in
/// an environment that intentionally runs with zero registered
/// strategies), handled by the caller as "warn and continue with an
/// empty registry," same as every previous revision's behavior.
///
/// Returns `Err` when the file exists but is not valid TOML, or does not
/// deserialize into `DeploymentManifest`'s shape (`strategies: Vec<
/// StrategyDeployment>`, each with `strategy_id`/`bytecode_hash`/
/// `contract_address`/`min_phase` — see integrity.rs). This function
/// does NOT itself validate hex encoding, byte length, or reject
/// placeholder zero data — that validation is `strategy_entries_from_
/// manifest`'s job (already real, already tested in integrity.rs) and
/// is applied by the caller immediately after this returns, so a
/// malformed-but-well-typed manifest (e.g. a real TOML shape with a
/// placeholder all-zero hash) is still caught, just one call later.
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
/// token symbol a per-cycle `OracleSnapshot` represents.
const ORACLE_SNAPSHOT_TOKEN: &str = "WETH";

/// C8 (this revision): live flashloan-liquidity snapshot for
/// `CheckContext::flashloan`, populated by the "L2e" poll loop in
/// `main()` and read once per scoring cycle in `score_and_admit`.
///
/// `available_wei` is the MAX of the two real, currently-implemented
/// liquidity reads (Aave V3 aToken-held-underlying via
/// `omega_rpc::flashloan_liq::fetch_aave_available`, Balancer V2 Vault
/// balance via `fetch_balancer_available`), for a single tracked asset
/// — `omega_rpc::WETH`. This is deliberately the SAME asset
/// `ORACLE_SNAPSHOT_TOKEN` ("WETH") already tracks for the oracle
/// snapshot; the two constants have no shared source of truth today
/// (one is a `&str` cache key into `ChainlinkOracle`/`PythOracle`/
/// `TwapOracle`, the other an `alloy_primitives::Address` re-exported
/// from `omega_rpc`) — same class of manual-sync risk this codebase
/// already flags elsewhere (see e.g. sa.rs's `SA_SLIPPAGE_BPS` vs.
/// `omega_risk::context::MAX_SLIPPAGE_BPS_SA` drift-guard tests). No
/// automated drift guard exists for this particular pairing yet;
/// worth adding one if a second asset is ever tracked.
///
/// KNOWN LIMITATION, flagged rather than silently assumed away: this
/// is a pre-trade SANITY signal for check 10 (`MissLiquidity`), not a
/// guarantee that whichever specific provider
/// `omega_flashloan::select_provider` ultimately picks for a given
/// blueprint has THIS MUCH liquidity available. `select_provider` runs
/// per-blueprint, inside the strategy's own `build_blueprint`, off its
/// own `LiquidityRegistry` — a DIFFERENT, currently-unfed data source
/// (see this file's own "C7 (this revision, separate task): flashloan
/// integration status" doc comment for why `LiquidityRegistry` has no
/// writer yet; C8 does not change that). Taking the MAX across
/// providers here is the conservative choice for a sanity check
/// (reject if even the best available provider can't cover it) but is
/// NOT the same claim as "the provider this blueprint will actually
/// use has this liquidity." Closing that gap needs `LiquidityRegistry`
/// fed from real data — the same blocker C4-A's LA-registration and
/// C7's flashloan notes already point at, not solved by this poll
/// loop.
#[derive(Debug, Clone, Default)]
struct FlashloanLiquidityState {
    /// Real, live available liquidity in the tracked asset's smallest
    /// unit (wei for WETH). `0` both genuinely (no liquidity found) and
    /// as the pre-first-successful-poll default — see the L2e poll
    /// loop for why that ambiguity is safe here: check 10's liquidity
    /// comparison treats a low/zero value as "reject," the fail-closed
    /// direction, either way.
    available_wei: u128,
    /// Which provider produced `available_wei` — `"aave"` or
    /// `"balancer"`, whichever read was larger on the most recent
    /// successful poll. Empty string before the first successful poll.
    protocol_id: String,
}

/// Builds a live `OracleSnapshot` for the pre-trade risk checks
/// (`omega_risk::checks`) and the hot-path lane (`omega_hot_path`) from
/// the three real feed caches.
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

/// Maps a strategy to omega-risk's own real per-strategy-class slippage
/// cap constant. Cross-checked against every real strategy constant:
/// SA=30/cap30, MSA=40/cap50, LA=50/cap100, MEV=30/cap30 — all pass.
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

/// C4: resolve the real registered bytecode hash for `strategy_id` from
/// `IntegrityRegistry`, falling back to the `[0u8; 32]` fail-closed
/// placeholder when no manifest is loaded or this strategy isn't in it.
///
/// This is the SAME registry (and the same `snapshot()` method) Stage 2b
/// already uses inside `ExecutionPipeline::execute_inner` — reusing it
/// here means check 4 (`omega_risk::checks`, this file's `CheckContext`)
/// and Stage 2b (`omega_security::IntegrityRegistry::full_integrity_
/// check`, inside the pipeline) are guaranteed to compare every
/// blueprint against the IDENTICAL expected hash, with no risk of the
/// two ever drifting onto two different "expected hash" values for the
/// same strategy.
///
/// Supersedes the prior revision's placeholder plan of adding a new
/// accessor to `omega_risk::whitelist::BytecodeWhitelist` — that would
/// have built a second, redundant lookup for data this function already
/// gets from a registry that's already real and already in scope here.
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

/// Builds the `CheckContext` passed to `ExecutionPipeline::execute`'s
/// Stage 2c (15 pre-trade checks).
///
/// ## C3: strategy_max_gas, max_slippage_bps, l1_adaptive_buffer,
/// latest_blueprint_nonce are real (see per-field comments below).
///
/// ## C4: strategy_bytecode_hash is now sourced from IntegrityRegistry
/// via `resolve_strategy_bytecode_hash` — see that function's doc
/// comment.
///
/// ## C5: current_l1_gas_price_gwei is now real, via the ArbGasInfo
/// poll loop (see this file's module-level "C5" doc comment).
///
/// ## C6 (this revision): risk_score is now a real formula over four
/// named components, two of which are themselves still fail-closed
/// placeholders
///
/// `checks.rs`'s own comment describes the intended risk score as
/// incorporating "gas volatility, oracle freshness, competition,
/// liquidity depth." This revision implements that as an explicit,
/// equal-weighted (0.25 each — see `RISK_WEIGHT_*` constants, a POLICY
/// DEFAULT not derived from spec) linear combination:
///
///   - **gas_volatility_risk**: real. `gas_volatility_risk` parameter,
///     computed once per scoring cycle via `PerChainOracle::
///     l1_gas_volatility_risk()` (coefficient-of-variation over the
///     last 20 ArbGasInfo readings — see that method's own doc comment).
///   - **oracle_freshness_risk**: real. Computed inline below from
///     `oracle_snapshot`'s three ages against `omega_risk::context`'s
///     real staleness constants — the freshest feed's age/threshold
///     ratio, clamped to `[0.0, 1.0]`.
///   - **competition_risk**: STILL A PLACEHOLDER. Reuses the same
///     hardcoded `1.0` this file has used since the C2 revision for
///     `ctx.competition_probability` — no real competition-probability
///     source exists yet (see that field's own comment below). Pinned
///     at maximum risk, not computed.
///   - **liquidity_risk**: as of C7 was STILL A PLACEHOLDER (pinned at
///     1.0, derived from the hardcoded `flashloan.available: 0`). As of
///     C8 (this revision) this is now REAL — see the "C8" note directly
///     below and this function's `flashloan_snapshot` parameter.
///
/// ## C8 (this revision): liquidity_risk / CheckContext.flashloan are
/// now real, sourced from `flashloan_snapshot`
///
/// `flashloan_available_value` (feeding both `liquidity_risk` in the
/// risk-score formula AND `CheckContext.flashloan.available` directly)
/// is now read from the new `flashloan_snapshot: FlashloanLiquidityState`
/// parameter instead of the unconditional `0` every revision through C7
/// used. That value is populated by a dedicated "L2e" poll loop in
/// `main()` from real Aave V3 / Balancer V2 `eth_call` reads
/// (`omega-rpc`'s `flashloan_liq` module) — see `FlashloanLiquidityState`'s
/// own doc comment for exactly what this is and its known limitation
/// (a MAX-across-providers sanity signal, not a guarantee that matches
/// whichever provider a given blueprint's own `select_provider` call
/// would pick). `liquidity_risk`'s `> 0` fail-closed test is UNCHANGED
/// from C6/C7 — only the value feeding it is now real.
///
/// See `RISK_SCORE_MAX_THRESHOLD`'s own "C8 addendum" doc comment for
/// the resulting change in check 12's fail-closed guarantee — that
/// arithmetic no longer holds unconditionally now that this field is
/// live.
///
/// `max_risk_score` is `RISK_SCORE_MAX_THRESHOLD` (0.45) — see that
/// constant's own doc comment (including its C8 addendum) for the
/// current state of check 12's fail-closed guarantee.
///
/// ## C7 (this revision): current_account_exposure_wei is now real,
/// via a new AccountExposureTracker (per-strategy, TTL-by-expiry-block)
///
/// See `omega_security::exposure`'s own module doc comment for the full
/// design and `MAX_ACCOUNT_EXPOSURE_WEI_PLACEHOLDER`'s doc comment for
/// why the cap itself is still a conservative, non-risk-approved
/// number. Unlike C6's risk_score (where the threshold was chosen
/// specifically to guarantee failure regardless of two still-fake
/// inputs), current_account_exposure_wei has no fake inputs left to
/// compensate for — it's a real sum over real recorded blueprints, so
/// this field can legitimately be 0 for a strategy that has genuinely
/// taken on no flashloan exposure (SA/MSA/MEV, by design, not as a
/// placeholder). Safety for a strategy that DOES carry real exposure
/// (LA) still comes from check 4 remaining the deterministic backstop
/// today (LA additionally fails earlier still, at its own missing-
/// debt-token guard in build_blueprint) — same layered-safety
/// methodology as every other field wired real this session.
///
/// The remaining fields (`competition_probability`/
/// `max_competition_probability`, `rollout_tier`) remain deliberate
/// fail-closed placeholders — see each field's comment for the
/// specific evidence.
///
/// ## Fix (this revision, clippy): this function's 9 parameters
/// (grown by C7's `current_account_exposure_wei`, then C8's
/// `flashloan_snapshot`) cross clippy's default `too_many_arguments`
/// threshold — see this file's module-level "Fix (this revision,
/// clippy)" doc comments for why `#[allow(...)]` here is consistent
/// with `run_scoring_loop` and `score_and_admit`'s existing precedent
/// rather than a new exception.
#[allow(clippy::too_many_arguments)]
fn build_check_context(
    sig: &omega_core::SignalState,
    oracle_snapshot: OracleSnapshot,
    strategy_max_gas: u64,
    max_slippage_bps: u16,
    latest_blueprint_nonce: u64,
    strategy_bytecode_hash: [u8; 32],
    gas_volatility_risk: f64,
    current_account_exposure_wei: u128,
    // C8 (this revision): real live liquidity snapshot — see this
    // function's module-level "C8" doc comment and
    // `FlashloanLiquidityState`'s own doc comment for what this is and
    // its known limitation.
    flashloan_snapshot: FlashloanLiquidityState,
) -> CheckContext {
    // ── C6: risk-score component computation ──────────────────────────
    //
    // Oracle freshness: the freshest of the three feeds' age/threshold
    // ratios, clamped to [0.0, 1.0]. A ratio near 0 means "just
    // updated" (low risk); near/at 1.0 means "right at or past its
    // staleness threshold" (high risk). Using u64::MAX-sentinel ages
    // (an oracle that has never been read — see build_oracle_snapshot's
    // own doc comment) produces an astronomically large finite f64
    // ratio, which .min(1.0) correctly clamps to maximum risk rather
    // than overflowing or panicking.
    let oracle_freshness_risk = {
        let cl_ratio = oracle_snapshot.chainlink_age_s as f64 / CHAINLINK_STALENESS_SECS as f64;
        let pyth_ratio = oracle_snapshot.pyth_age_s as f64 / PYTH_STALENESS_SECS as f64;
        let twap_ratio = oracle_snapshot.twap_age_s as f64 / TWAP_STALENESS_SECS as f64;
        cl_ratio.min(pyth_ratio).min(twap_ratio).min(1.0)
    };

    // Competition: still a placeholder — see this function's own "C6"
    // doc comment. Extracted to a local so the SAME value feeds both
    // the risk-score formula below and the corresponding CheckContext
    // field further down, rather than two independently hardcoded
    // literals that could drift apart from each other the same way SA's
    // slippage constant drifted from context.rs's cap earlier this
    // session.
    let competition_probability_value = 1.0_f64;
    let max_competition_probability_value = 0.0_f64;

    // C8 (this revision): flashloan_available_value/flashloan_protocol_id
    // are now REAL, sourced from the live `flashloan_snapshot` parameter
    // (populated by the L2e poll loop in main() from real Aave/Balancer
    // eth_call reads) rather than the unconditional `0`/empty-string
    // pair every prior revision used. See this function's own "C8" doc
    // comment above.
    let flashloan_available_value: u128 = flashloan_snapshot.available_wei;
    let flashloan_protocol_id: String = flashloan_snapshot.protocol_id;

    let competition_risk = competition_probability_value;
    // C8: now driven by a real value — see the local's own comment just
    // above. `> 0` remains the correct fail-closed test, unchanged from
    // C6/C7: a genuine zero reading (both provider reads failed, or both
    // are legitimately empty) still maps to max risk.
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
        // ── Real, live data ──────────────────────────────────────────
        expected_chain_id: CHAIN_ID,
        current_block: sig.block_number,
        current_l2_base_fee_gwei: sig.base_fee_gwei,
        oracle: oracle_snapshot,

        // C3: real — StrategyTrait::gas_budget().
        strategy_max_gas,

        // C3: real — omega-risk::context's own MAX_SLIPPAGE_BPS_SA/MSA/
        // LA/MEV constants. SA and MEV sit exactly at their cap (zero
        // headroom, not a bug — check 9 is a strict `>`); each strategy
        // file now carries its own drift-guard test mirroring these
        // exact cap values.
        max_slippage_bps,

        // C3: real (correctly implements "no data yet" behavior) —
        // calls omega_risk::gas_model::l1_adaptive_buffer(&[]) directly.
        // Returns L1_BUFFER_MIN (1.30) for an empty price history, since
        // gas_model.rs's L1GasEma rolling-window tracker is never fed
        // (its input, a live L1 gas price stream, does not exist).
        l1_adaptive_buffer: omega_risk::gas_model::l1_adaptive_buffer(&[]),

        // C5 (this revision): real — ArbGasInfo precompile, via
        // OmegaRpcClient::fetch_l1_base_fee_estimate_gwei (omega-rpc's
        // new arb_gas_info.rs) polled into PerChainOracle's FeeSnapshot
        // by a dedicated poll loop in main() (see "L2d" below), read
        // here via sig.l1_data_fee_gwei — the same field every strategy
        // already reads for its own profitability math (confirmed:
        // sa.rs/msa.rs/la.rs/mev.rs all read signal.l1_data_fee_gwei
        // directly). Before the poll loop's first successful cycle (or
        // if ArbGasInfo becomes unreachable), this is genuinely 0 — see
        // PerChainOracle::new's initial_fee — which still fails closed
        // correctly at check 6 (check_gas_spike): comparing a nonzero
        // bp.l1_data_fee_at_creation against a current value of 0
        // produces a 100% "spike," well over the 30% threshold, so an
        // absent live reading rejects rather than silently passing.
        // Supersedes the prior revision's unconditional u64::MAX
        // placeholder — that also failed closed, but never became real
        // once live data existed, unlike this field now.
        current_l1_gas_price_gwei: sig.l1_data_fee_gwei,

        // C8 (this revision): real, live availability for the tracked
        // asset (WETH — see FlashloanLiquidityState's doc comment),
        // sourced from `flashloan_available_value`/`flashloan_protocol_id`
        // above rather than the hardcoded `0`/empty-string pair every
        // revision through C7 used. Still subject to the "MAX across
        // providers, not necessarily the provider a given blueprint will
        // actually use" limitation documented on `FlashloanLiquidityState`
        // — this is a real improvement over the C6/C7 placeholder, not a
        // claim that check 10 is now fully precise for every strategy.
        // (Prior-revision context, still accurate as background: the
        // THREE compounding gaps this file's own "C7 (separate task):
        // flashloan integration status" doc comment documents —
        // LiquidityRegistry unfed, LA unregistered pending C4-A, LA's
        // own missing-debt-token guard — are UNCHANGED by C8. This field
        // is a pre-trade sanity check, not a claim that LA's flashloan
        // path is unblocked.)
        flashloan: FlashloanSnapshot {
            available: flashloan_available_value,
            protocol_id: flashloan_protocol_id,
        },

        // CONFIRMED BLOCKED: competition_probability needs three inputs
        // SignalState doesn't carry; underlying health-factor ingestion
        // is itself an unimplemented placeholder. C6: now reads
        // competition_probability_value / max_competition_probability_
        // value (the same locals the risk-score competition_risk
        // component reads above) instead of two separately hardcoded
        // literals — 1.0 vs. max 0.0 still guarantees check 11 fails
        // closed, unchanged from before.
        competition_probability: competition_probability_value,
        max_competition_probability: max_competition_probability_value,

        // No rollout-tier config exists, and no check reads it today.
        rollout_tier: 0.0,

        // C4: real-or-fail-closed — see resolve_strategy_bytecode_hash's
        // and this function's own doc comment above.
        strategy_bytecode_hash,

        // C6/C8: real formula over four named components (one still a
        // placeholder — competition_risk) — see this function's own
        // "C6"/"C8" doc comments and RISK_SCORE_MAX_THRESHOLD's doc
        // comment (including its "C8 addendum") for the current state
        // of check 12's fail-closed guarantee.
        risk_score,
        max_risk_score: RISK_SCORE_MAX_THRESHOLD,

        // C7: current_account_exposure_wei is now real, via
        // AccountExposureTracker — see this function's own "C7" doc
        // comment. max_account_exposure_wei is still a conservative,
        // non-risk-approved placeholder — see
        // MAX_ACCOUNT_EXPOSURE_WEI_PLACEHOLDER's own doc comment for
        // why 1 ETH, and why this errs small rather than large.
        current_account_exposure_wei,
        max_account_exposure_wei: MAX_ACCOUNT_EXPOSURE_WEI_PLACEHOLDER,

        // C3: real (partially) — NonceRegistry is real and tracked, but
        // nothing calls .advance() on it after a real submission yet
        // (Stage 7 reconciliation, not built) — check 15 only rejects
        // each strategy's very first blueprint until that's wired in.
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

    // ── L2d: ArbGasInfo L1 data fee polling (this revision) ───────────────────
    //
    // Real ingestion for the L1 data fee — closes the last hardcoded-0
    // gas-path placeholder flagged throughout this session
    // ("populated by ArbGasInfo; 0 here as default", per_chain.rs's own
    // long-standing comment). Uses the already-connected `rpc` client,
    // same pattern as L2c's Chainlink poll loop immediately above.
    // 15s cadence matches L2c's own poll interval — a starting value,
    // not derived from any spec section; ArbGasInfo's L1 base fee
    // estimate does not change every block the way L2 base fee does, so
    // a tighter interval may not be warranted, but this hasn't been
    // measured against real chain behavior.
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
                        // Deliberately does NOT touch the oracle on
                        // failure — the previous real value (or the
                        // honest 0 default before the first successful
                        // poll) is left in place, which check 6
                        // (check_gas_spike) already handles correctly
                        // either way (see build_check_context's own
                        // comment on this field). A transient RPC
                        // failure here should not silently reset a
                        // known-good L1 fee reading to something worse.
                        tracing::warn!(error = %e, "ArbGasInfo poll failed — keeping previous value");
                    }
                }
            }
        });
        tracing::info!("L2d ArbGasInfo poll loop started (15s interval)");
    }

    // ── L2e: flashloan liquidity polling (this revision, "C8") ────────────────
    //
    // Real ingestion for CheckContext::flashloan.available — closes the
    // hardcoded `flashloan_available_value: u128 = 0` placeholder in
    // build_check_context (see that function's own C6/C7/C8 doc
    // comments, and this file's module-level "C8" doc comment, for why
    // it was 0 through C7: no liquidity read existed anywhere in the
    // workspace).
    //
    // This does NOT populate omega_flashloan::LiquidityRegistry (still
    // unfed — see main.rs's own "C7 (this revision, separate task):
    // flashloan integration status" doc comment, unchanged by C8). It
    // populates a SEPARATE, simpler signal — `FlashloanLiquidityState`,
    // shared via a `watch` channel — used only for the pre-trade risk
    // check's liquidity sanity component, via the real Aave/Balancer
    // `eth_call` reads in omega-rpc's `flashloan_liq` module.
    //
    // On a read failure (either provider), falls back to whichever
    // single provider succeeded, logging the failure. On BOTH providers
    // failing, deliberately leaves the watch channel untouched — same
    // "keep the previous value, never silently reset it to something
    // worse or better" posture as L2d's ArbGasInfo poll loop above.
    let (flashloan_liq_tx, flashloan_liq_rx) =
        tokio::sync::watch::channel(FlashloanLiquidityState::default());
    {
        let liq_client = rpc.clone();
        let liq_tx = flashloan_liq_tx.clone();
        tokio::spawn(async move {
            // C8: WETH — see FlashloanLiquidityState's doc comment on
            // why this must be kept manually in sync with
            // ORACLE_SNAPSHOT_TOKEN ("WETH") until a shared source of
            // truth exists for "the one asset this engine currently
            // tracks."
            let token = WETH;
            let mut ticker =
                tokio::time::interval(Duration::from_secs(FLASHLOAN_LIQUIDITY_POLL_INTERVAL_S));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                let aave = liq_client.fetch_aave_available(token).await;
                let balancer = liq_client.fetch_balancer_available(token).await;

                let candidate = match (aave, balancer) {
                    (Ok(a), Ok(b)) if a >= b => Some((a, "aave".to_string())),
                    (Ok(_), Ok(b)) => Some((b, "balancer".to_string())),
                    (Ok(a), Err(e)) => {
                        tracing::warn!(
                            error = %e,
                            "C8: Balancer liquidity read failed — using Aave-only reading this cycle"
                        );
                        Some((a, "aave".to_string()))
                    }
                    (Err(e), Ok(b)) => {
                        tracing::warn!(
                            error = %e,
                            "C8: Aave liquidity read failed — using Balancer-only reading this cycle"
                        );
                        Some((b, "balancer".to_string()))
                    }
                    (Err(ea), Err(eb)) => {
                        tracing::warn!(
                            aave_error = %ea,
                            balancer_error = %eb,
                            "C8: both flashloan liquidity reads failed — keeping previous value"
                        );
                        None
                    }
                };

                if let Some((available_wei, protocol_id)) = candidate {
                    tracing::debug!(
                        available_wei,
                        protocol_id = %protocol_id,
                        "C8: flashloan liquidity poll updated"
                    );
                    let _ = liq_tx.send(FlashloanLiquidityState {
                        available_wei,
                        protocol_id,
                    });
                }
            }
        });
        tracing::info!(
            interval_s = FLASHLOAN_LIQUIDITY_POLL_INTERVAL_S,
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

    // ── C1: ExecutionPipeline construction (fail-closed) ─────────────────────
    let kill_switch_cfg = KillSwitchConfig {
        max_cumulative_loss_wei: u128::MAX / 4,
        max_loss_per_window_wei: u128::MAX / 8,
        loss_window: Duration::from_secs(3600),
        max_consecutive_failures: 32,
    };
    let kill_switches =
        Arc::new(KillSwitchRegistry::new(kill_switch_cfg).context("KillSwitchRegistry::new")?);
    tracing::warn!(
        "C1: KillSwitchRegistry constructed with non-production placeholder thresholds (Gap 5)"
    );

    // C1/C4: IntegrityRegistry — no longer unconditionally empty. See
    // this file's module-level "C4" doc comment for the full
    // three-outcome load behavior.
    let integrity_registry = IntegrityRegistry::new();
    match load_deployment_manifest(DEPLOYMENT_MANIFEST_PATH)
        .with_context(|| format!("loading deployment manifest from {DEPLOYMENT_MANIFEST_PATH}"))?
    {
        Some(manifest) => {
            // strategy_entries_from_manifest is real, pre-existing
            // (integrity.rs), and already validates every entry (hex,
            // length, non-placeholder) — one bad entry fails the WHOLE
            // call via `?`, which propagates out of main() and halts
            // startup rather than running with a partially-registered
            // or all-placeholder registry.
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
                "C4: real deployment manifest loaded — strategies registered in IntegrityRegistry"
            );
        }
        None => {
            tracing::warn!(
                path = DEPLOYMENT_MANIFEST_PATH,
                "C4: no deployment manifest found at the conventional path — \
                 IntegrityRegistry empty, every strategy_id will fail Stage 2b \
                 as StrategyUnknown until a real manifest (from forge deploy \
                 output or an on-chain eth_getCode read — never fabricated) is \
                 placed here (Gap 6 not yet resolved for this environment)"
            );
        }
    }
    // Deliberately NOT calling integrity_registry.freeze(...) here for
    // any newly-registered strategy — see this file's module-level "C4"
    // doc comment for why that would be wrong (freeze permanently
    // disables a strategy; it is a governance action, not a startup
    // step).

    // ── C5 (this revision): relay production bootstrap ────────────────────────
    //
    // Replaces C1's zero-relay stub with real HttpRelayClients built from
    // config/secrets. See this file's module-level "C5 (this revision,
    // separate task): relay production bootstrap" doc comment, AND the
    // module-level "FIX (this revision)" doc comment (the E0609/E0308
    // corrections), for the full design and every fallback/skip rule
    // below.
    let relay_http_client = omega_relay::client::HttpRelayClient::build_http_client()
        .context("building shared reqwest client for relay submission")?;

    // FIX (this revision): every relay this file has a verified auth
    // convention for is a CANDIDATE for every phase — see
    // KNOWN_RELAY_NAMES's own doc comment and this file's module-level
    // "FIX (this revision)" doc comment, item 1, for why a real
    // per-phase selection field does not exist on `omega_core::
    // RelayConfig` to gate this on instead.
    let mut relay_clients: HashMap<String, Arc<dyn RelayClient>> = HashMap::new();
    for name in KNOWN_RELAY_NAMES.iter() {
        let endpoint_var = format!("OMEGA_RELAY_ENDPOINT_{}", name.to_string().to_uppercase());
        let endpoint = match std::env::var(&endpoint_var) {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    relay = %name,
                    var = %endpoint_var,
                    "C5: no endpoint configured for this relay — skipped, not guessed at"
                );
                continue;
            }
        };

        let auth = match name {
            RelayName::Flashbots => match std::env::var("FLASHBOTS_AUTH_KEY") {
                Ok(k) => match RelayAuth::flashbots_style(&k) {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::error!(relay = %name, error = %e, "C5: invalid FLASHBOTS_AUTH_KEY — relay skipped");
                        continue;
                    }
                },
                Err(_) => {
                    tracing::warn!(relay = %name, "C5: FLASHBOTS_AUTH_KEY not set — relay skipped");
                    continue;
                }
            },
            RelayName::Titan => match std::env::var("TITAN_AUTH_KEY") {
                Ok(k) => match RelayAuth::flashbots_style(&k) {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::error!(relay = %name, error = %e, "C5: invalid TITAN_AUTH_KEY — relay skipped");
                        continue;
                    }
                },
                Err(_) => {
                    tracing::warn!(relay = %name, "C5: TITAN_AUTH_KEY not set — relay skipped");
                    continue;
                }
            },
            RelayName::Bloxroute => match std::env::var("BLOXROUTE_AUTH_TOKEN") {
                Ok(t) => RelayAuth::BearerToken(t),
                Err(_) => {
                    tracing::warn!(relay = %name, "C5: BLOXROUTE_AUTH_TOKEN not set — relay skipped");
                    continue;
                }
            },
            RelayName::Eden => match std::env::var("EDEN_AUTH_TOKEN") {
                Ok(t) => RelayAuth::BearerToken(t),
                Err(_) => {
                    tracing::warn!(relay = %name, "C5: EDEN_AUTH_TOKEN not set — relay skipped");
                    continue;
                }
            },
            RelayName::Other(raw) => {
                tracing::error!(
                    relay = %raw,
                    "C5: no verified auth convention for this relay name — skipped, \
                     not guessed at (see signing.rs's own documented provider mapping)"
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
        tracing::warn!(
            "C5: zero relay clients constructed (no endpoints/secrets present in \
             environment) — submissions will fail closed, same posture as C1's stub"
        );
    } else {
        tracing::info!(
            relays = ?relay_clients.keys().collect::<Vec<_>>(),
            "C5: real relay clients constructed"
        );
    }

    // C5: ExecutionAddress is still not backed by a real signer — C6
    // (KeyManager / signing) has not started, gated on the orchestrator
    // ABI/schema. This is a label for metrics/carryover bookkeeping only,
    // never a signing capability.
    let execution_address = std::env::var("OMEGA_EXECUTION_ADDRESS")
        .unwrap_or_else(|_| "0xC1_UNCONFIGURED".to_string());
    if execution_address == "0xC1_UNCONFIGURED" {
        tracing::warn!(
            "C5: OMEGA_EXECUTION_ADDRESS not set — relay metrics identity still a \
             placeholder (real value needs C6's KeyManager, not done this revision)"
        );
    }
    let relay_metrics = LaRelayMetrics::new(50, ExecutionAddress(execution_address));

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

    // C5: real, deliberately SEPARATE from ARBITRUM_RPC_URL — see this
    // file's module-level "C5" doc comment for why InclusionTracker's
    // plain-HTTP eth_getTransactionReceipt calls cannot share the
    // WebSocket URL used for the block/log subscriptions above. Missing
    // this halts startup rather than silently degrading every relay's
    // measured inclusion rate to 0% forever.
    let confirmation_rpc_url = std::env::var("ARBITRUM_HTTP_RPC_URL").context(
        "ARBITRUM_HTTP_RPC_URL must be set — a real chain JSON-RPC HTTP endpoint for \
         inclusion confirmation, distinct from ARBITRUM_RPC_URL's WebSocket endpoint",
    )?;

    // FIX (this revision): build `omega_relay::RelayConfig` — the relay
    // crate's OWN type — from ITS OWN `Default::default()`, rather than
    // struct-update-spreading `omega_core::RelayConfig` (a distinct type
    // with the same name) into it. See this file's module-level "FIX
    // (this revision)" doc comment, item 2, for why the rest of
    // `config.relay`'s fields are deliberately NOT hand-mapped here.
    let relay_cfg = RelayConfig {
        confirmation_rpc_url,
        ..Default::default()
    };

    // startup_block: still 0 — see this file's module-level "C5" doc
    // comment for why (no synchronous "current height" read available
    // off `rpc` in this file today). Flagged, not fabricated.
    let (relay, reorg_event_rx) =
        MultiRelayClient::new(relay_clients, relay_metrics, blacklist, &relay_cfg, 0);

    tokio::spawn(async move {
        let mut rx = reorg_event_rx;
        while let Some(ev) = rx.recv().await {
            tracing::debug!(
                ?ev,
                "C5: LaReorgRiskEvent received (on_new_block not wired yet — see \
                 this file's own 'NOT DONE, deliberately' C5 doc comment on why)"
            );
        }
    });

    let signer = Arc::new(UnconfiguredSigner);

    let execution_pipeline = Arc::new(ExecutionPipeline::new(
        Arc::clone(&kill_switches),
        Arc::clone(&integrity_registry),
        Arc::clone(&relay),
        Arc::clone(&dag),
        Arc::clone(&signer),
        CHAIN_ID,
    ));
    tracing::info!(
        idempotency_cache_len = execution_pipeline.idempotency_cache_len(),
        "C1: ExecutionPipeline constructed (fail-closed signer; relay clients per C5 above)"
    );

    // ── C5 (this revision): reconciliation lifecycle ──────────────────────────
    //
    // Drives InclusionTracker::reconcile off the same oracle block-number
    // stream run_scoring_loop already subscribes to below — reconcile()
    // only needs a block NUMBER (confirmation.rs's own signature), so this
    // half of the reconciliation lifecycle does not need the still-missing
    // block-hash stream the reorg-guard wiring above is blocked on.
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
                                "C5: inclusion confirmations reconciled"
                            );
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "C5: reconciliation loop lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        tracing::info!("C5: reconciliation lifecycle task started");
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

    // ── C3: nonce registry ────────────────────────────────────────────────────
    let nonce_registry = omega_security::replay::NonceRegistry::new();
    tracing::warn!(
        "C3: NonceRegistry constructed but never advanced — check 15 only rejects each \
         strategy's very first blueprint (nonce 0) until Stage 7 reconciliation wires \
         advance() in"
    );

    // ── C7: account exposure tracker (this revision) ──────────────────────────
    let exposure_tracker = AccountExposureTracker::new();
    tracing::warn!(
        "C7: AccountExposureTracker constructed — real per-strategy tracking via \
         each blueprint's own expiry_block as a conservative TTL (see \
         omega_security::exposure's doc comment), but max_account_exposure_wei \
         is still a non-risk-approved placeholder (1 ETH) and this tracker is \
         in-memory only (resets on restart)"
    );

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
        let ep3 = Arc::clone(&execution_pipeline);
        let nr3 = nonce_registry.clone();
        // C4: threaded through so score_and_admit can resolve the real
        // registered bytecode hash for check 4.
        let ir3 = Arc::clone(&integrity_registry);
        // C7: threaded through so score_and_admit can record/read real
        // exposure for check 14.
        let et3 = exposure_tracker.clone();
        // C8: threaded through so score_and_admit can read the real
        // live flashloan-liquidity snapshot for check 10 / risk_score —
        // watch::Receiver is cheap (Arc-backed) to clone, same as every
        // other cross-task handle in this block.
        let fl3 = flashloan_liq_rx.clone();
        tokio::spawn(async move {
            run_scoring_loop(
                reg, ora3, cl3, py3, tw3, dag2, tx, pq, halt3, ph, ep3, nr3, ir3, et3, fl3,
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

// C4/C8: grown to 14 args (C4 added integrity_registry, C8 adds
// flashloan_liq_rx) — the existing allow already covers this; not
// re-litigating the struct-refactor question for one more parameter
// added to an already-allowed function.
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
    execution_pipeline: Arc<ExecutionPipeline<UnconfiguredSigner>>,
    nonce_registry: omega_security::replay::NonceRegistry,
    // C4: real IntegrityRegistry, threaded through so score_and_admit
    // can resolve the real registered bytecode hash for check 4.
    integrity_registry: Arc<IntegrityRegistry>,
    // C7: real AccountExposureTracker, threaded through so
    // score_and_admit can record/read real exposure for check 14.
    exposure_tracker: AccountExposureTracker,
    // C8: read side of the flashloan-liquidity watch channel — see
    // main()'s "C8" doc comment and FlashloanLiquidityState's own doc
    // comment.
    flashloan_liq_rx: tokio::sync::watch::Receiver<FlashloanLiquidityState>,
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
                let oracle_snapshot = build_oracle_snapshot(
                    &chainlink_oracle,
                    &pyth_oracle,
                    &twap_oracle,
                    ORACLE_SNAPSHOT_TOKEN,
                );
                // C6: computed once per scoring cycle, same as
                // oracle_snapshot above — every strategy scored in this
                // cycle should see the identical gas-volatility reading,
                // not one recomputed per strategy from a rolling window
                // that could shift between spawns.
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
                    // C8: watch::Receiver is cheap (Arc-backed) to clone
                    // per spawned task, same as every other handle above.
                    let fl2 = flashloan_liq_rx.clone();
                    tokio::spawn(async move {
                        score_and_admit(
                            strategy, s2, dag2, tx2, pq2, h2, ph, os2, ep2, nr2, ir2, gv2, et2,
                            fl2,
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
    execution_pipeline: Arc<ExecutionPipeline<UnconfiguredSigner>>,
    nonce_registry: omega_security::replay::NonceRegistry,
    // C4: real IntegrityRegistry, used to resolve the real registered
    // bytecode hash for this strategy.
    integrity_registry: Arc<IntegrityRegistry>,
    // C6: real gas-volatility risk component, computed once per
    // scoring cycle in run_scoring_loop via PerChainOracle::
    // l1_gas_volatility_risk() — see build_check_context's own "C6"
    // doc comment for how this feeds the risk_score formula.
    gas_volatility_risk: f64,
    // C7: real AccountExposureTracker — recorded into at DAG admission
    // time below, read from just before build_check_context.
    exposure_tracker: AccountExposureTracker,
    // C8: read side of the flashloan-liquidity watch channel. Read once,
    // right before build_check_context, via `.borrow().clone()` — same
    // "snapshot at use time" pattern as everything else CheckContext is
    // built from in this function.
    flashloan_liq_rx: tokio::sync::watch::Receiver<FlashloanLiquidityState>,
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

    // C7: record this blueprint's flashloan exposure the moment it's
    // genuinely admitted (this is the one clean lifecycle point
    // score_and_admit itself owns — see omega_security::exposure's own
    // doc comment for why DAG-slot RELEASE isn't hooked the same way).
    // AccountExposureTracker::record is a no-op for amount_wei == 0
    // (SA/MSA/MEV today), so this line is inert for every strategy
    // except LA without needing a branch here.
    exposure_tracker.record(
        &strategy.strategy_id().to_string(),
        bp.flashloan_amount.try_into().unwrap_or(u128::MAX),
        bp.expiry_block,
    );

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

    // ── C2/C3/C4/C6/C7/C8: ExecutionPipeline::execute — real DAG-slot ownership ─
    let strategy_max_gas = strategy.gas_budget();
    let max_slippage_bps = max_slippage_bps_for(strategy.strategy_id());
    let latest_blueprint_nonce =
        nonce_registry.next_nonce(&strategy.strategy_id().to_string(), CHAIN_ID);
    // C4: real-or-fail-closed bytecode hash for check 4.
    let strategy_bytecode_hash =
        resolve_strategy_bytecode_hash(&integrity_registry, strategy.strategy_id());
    // Moved earlier than its prior position (was computed after
    // build_check_context) — C7's exposure read needs the current block
    // number to prune expired entries, so it must be available before
    // that call now, not just before execute().
    let current_block = signal.block_number;
    // C7: real current exposure for check 14 — see this function's own
    // "C7" comment above the record() call, and build_check_context's
    // own "C7" doc comment.
    let current_account_exposure_wei = exposure_tracker
        .current_exposure_wei(&strategy.strategy_id().to_string(), current_block);
    // C8: snapshot the live liquidity state right before building the
    // check context — `.borrow()` returns a guard; `.clone()` out of it
    // immediately so we're not holding the watch channel's internal lock
    // across the rest of this function.
    let flashloan_snapshot = flashloan_liq_rx.borrow().clone();
    let risk_ctx = build_check_context(
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
            // Expected in practice today for any strategy not in a real,
            // loaded manifest (Stage 2b StrategyUnknown), and for every
            // strategy regardless once past that, until the remaining
            // fail-closed CheckContext fields (competition, primarily)
            // get real sources.
            tracing::debug!(
                hash = %bp.blueprint_hash,
                error = %e,
                "ExecutionPipeline::execute rejected blueprint (expected until remaining \
                 risk-data gaps are closed)"
            );
        }
    }

    // C2: dag.complete() REMOVED here — execute() above is now the SOLE
    // owner of this blueprint's DAG slot via its internal DagSlotGuard.
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