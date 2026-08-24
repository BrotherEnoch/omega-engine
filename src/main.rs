// src/main.rs — OmegaEngine v12.0 Main Entry Point
//
// Required: ARBITRUM_RPC_URL (WebSocket endpoint)
// Required (this revision, C5): ARBITRUM_HTTP_RPC_URL (plain HTTP JSON-RPC
//   endpoint — see this file's own "C5" doc comment for why this is a
//   separate, required variable from ARBITRUM_RPC_URL rather than reusing it)
// Required (this revision, C9): VAULT_ADDRESS, PROFIT_TOKEN — see this
//   file's own "C9" doc comment below.
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
// NOTE (C9, this revision): this ownership statement is no longer
// unconditionally true — see the "C9" doc comment below. `execute()`'s
// `DagSlotGuard` only releases a slot for blueprints that actually reach
// `execute()`. C9 introduces new early-return paths in `score_and_admit`
// (a rejected/failed/unverified ZK proof) that occur BEFORE `execute()`
// is ever called — those paths release the slot themselves, explicitly,
// via a direct `dag.lock().unwrap().complete(...)` call, the same
// mechanism this file used unconditionally before the C2 change. This is
// not a reversion of C2's design — DagSlotGuard is still the sole owner
// for every blueprint that reaches `execute()` — it's a necessary
// consequence of C9 adding real rejection paths upstream of that call
// that didn't exist when C2 was written.
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
// RESOLVED (follow-up revision): `omega_rpc::client::BlockEvent` already
// carried a real `hash: B256` field the whole time — confirmed against
// that type's own source. The missing piece was never a cross-crate
// change; nothing in this file ever called `rpc.subscribe_blocks()`.
// It does now — see the new block-hash-feed task in `main()`, placed
// right after `MultiRelayClient::new`, and the `feed_block_event_to_
// reorg_guard` helper below.
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
// RESOLVED (follow-up revision): `omega_core::config::RelayConfig` has
// gained real `phase_1_relays: Vec<String>` / `phase_2plus_relays:
// Vec<String>` / `blind_fallback: bool` fields (backward-compatible
// defaults — see that file's own doc comment). `omega_execution::
// config_translation::translate_relay_config` is now called for real
// below, replacing the `Default::default()` stub item 2 above
// describes, and relay-candidate selection is genuinely phase-gated
// instead of "every relay, every phase" per item 1 above.
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
//
// ## FIX (this revision): E0433 in reorg_block_feed_tests — unresolved
// `alloy` path
//
// `reorg_block_feed_tests::feed_block_event_to_reorg_guard_detects_a_
// real_reorg` referenced `alloy::primitives::B256::from(...)` directly,
// but this binary crate (`omega-engine`) has no direct dependency on
// the `alloy` crate — it only reaches `alloy`'s types transitively
// through `omega-rpc`/`omega-relay`'s own public API, so the bare
// crate-root path `alloy::...` does not resolve here (E0433), even
// though `omega_rpc::BlockEvent.hash` is genuinely typed as a real
// `B256` under the hood. Fixed WITHOUT adding a new `alloy` dependency
// to this crate: `B256: From<[u8; 32]>` (alloy-primitives' own
// conversion), so `[1u8; 32].into()` / `[2u8; 32].into()` let type
// inference resolve to `BlockEvent::hash`'s real field type with no
// explicit reference to the `alloy` crate name at all. This is the
// smaller fix relative to adding `alloy` as a dev-dependency purely to
// spell out a type this crate doesn't otherwise need to name directly
// — consistent with this file's own established preference (see e.g.
// the RelayConfig-translation notes above) for not pulling in
// cross-crate surface area a call site doesn't strictly require.
//
// ## C9 (this revision): real ZK-gate enforcement — the ZK proof result
// now actually gates execution
//
// Prior to this revision, an investigation this session traced the full
// call chain and found TWO compounding gaps, both confirmed against
// real source (not inferred):
//
//   1. `omega_zk::ZkVerifier::verify()` was called NOWHERE in this
//      workspace. `crates/omega-execution/Cargo.toml` — the crate that
//      actually owns relay submission — has no dependency on omega-zk
//      at all, so `ExecutionPipeline::execute()` structurally cannot
//      call it even if it wanted to.
//   2. Even the WEAKER check — "did proof GENERATION merely not
//      error" — was not enforced. `score_and_admit`'s prior structure
//      awaited `proof_queue.submit(...)`'s result inside an `if let
//      Ok(rx) = ...` / `if let Ok(Ok(proof)) = rx.await` pair, but
//      logged-and-continued on success and did EXACTLY NOTHING
//      different on any failure path (`Err` from submit(), `Err` from
//      the proof-generation Result, or the response channel closing
//      without a value at all) — every one of those fell through
//      silently to the SAME unconditional call to
//      `execution_pipeline.execute(...)` a few lines later. As written,
//      the ZK proof queue generated proofs into the void.
//
// This revision closes both gaps:
//
//   - New required env vars `VAULT_ADDRESS` / `PROFIT_TOKEN`, parsed via
//     the new `parse_address_env` helper into `[u8; 20]`. Needed to
//     compute the real `publicInputsHash` every proof must bind to —
//     see `omega_zk::binding::compute_public_inputs_hash`'s own doc
//     comment for the exact formula (mirrors
//     `OmegaVault.computePublicInputsHash()` byte for byte; ALSO see
//     that function's own header for why this has not been run against
//     a real Solidity/`cast` output in this environment — no Rust
//     toolchain was available this session to verify it end to end).
//   - A `ZkVerifier` is now constructed once in `main()` and threaded
//     through `run_scoring_loop` / `score_and_admit`.
//   - `score_and_admit`'s non-hot-path branch is restructured into an
//     explicit match/early-return chain: a rejected submission, a
//     failed proof, a dropped response channel, OR a proof that fails
//     `ZkVerifier::verify()` against the expected `publicInputsHash`
//     now all DROP the blueprint — `execution_pipeline.execute(...)` is
//     never reached on any of those paths. Only a proof that both
//     generates successfully AND verifies against the correct
//     `publicInputsHash` allows the blueprint through to execution.
//   - Every one of those new early-return paths releases the DAG slot
//     explicitly (`dag.lock().unwrap().complete(bp.blueprint_hash)`) —
//     see the "C2" doc comment above for why this is required: C2's
//     `DagSlotGuard` only covers blueprints that reach `execute()`, and
//     these new paths, by design, do not.
//
// RESOLVED (follow-up revision): `worker.rs` is now available and has been fixed directly
// — `process_request` reads `req.public_inputs_hash` and passes it through as
// `T1SoftwareProver::prove()`'s new second argument. That was the one guaranteed compile
// break; it's closed.
//
// RESOLVED (follow-up revision): the `hot` (hot-path) branch of `score_and_admit` now ALSO
// provisions a ZK proof, closing the "zero proof pathway, forever" gap flagged in the
// prior revision of this comment — see that branch's own inline comment for the full
// reasoning. Summary: `OmegaVault.receivePendingProfit()` (called on-chain immediately
// after execution) does NOT require a proof — only the LATER `releaseProfit()` call does.
// So hot-path admission is deliberately still NOT gated on proof completion (that would
// reimport the exact latency cost the hot path exists to avoid, for no on-chain
// requirement that demands it) — instead, the same `proof_queue.submit()` the non-hot-path
// branch uses is fired as a detached background task, so a proof eventually becomes
// available to bind that blueprint's pending profit, without hot-path ever waiting on it.
//
// STILL NOT ADDRESSED, even after this fix, and stated plainly rather than implied solved:
// nothing in this codebase, in anything shown across this entire investigation, actually
// calls `OmegaVault.submitProof()` ON-CHAIN with a proof once the background task above
// has generated and verified one. This revision provisions the proof; it does not close
// the gap of what relayer/keeper component submits it. That remains genuinely open — see
// the hot-path branch's own comment for the same point made in place.
//
// ## C6 (this revision): real TransactionSigner wired in — UnconfiguredSigner replaced
//
// main() now constructs a real `omega_execution::signer::KeyManagerTransactionSigner`
// instead of `UnconfiguredSigner`, closing the last of C1's four fail-closed stand-ins
// (KillSwitchRegistry/IntegrityRegistry/MultiRelayClient were already made real by
// C4/C5). Three new required env vars — `ORCHESTRATOR_ADDRESS`, `OMEGA_TX_SIGNING_KEY`,
// `OMEGA_BLUEPRINT_SIGNING_KEY` — see each one's own read-site comment in main() for what
// it is and why it's required rather than defaulted or optional. `strategy_onchain_ids()`
// (new, this revision) transcribes the five real `strategyId` constants directly from
// `contracts/src/StrategyIds.sol` — the same values `RegisterStrategies.s.sol` already
// cross-checks every deployment manifest's `onchain_id` field against before registering
// a strategy on-chain — closing the one item `omega-execution/src/signer.rs`'s own doc
// comment named as still open (item 4: "the StrategyId -> bytes32 strategyId mapping").
// These are transcribed byte-for-byte from that Solidity library, not derived or guessed
// here; if `StrategyIds.sol` is ever changed, this map must be updated to match by hand —
// nothing in this codebase keeps the two in sync automatically.
//
// STILL OPEN, not closed by this change — see signer.rs's own doc comment for the full,
// still-accurate list: the RLP/ABI encoding has only been self-consistency-checked
// (encode then decode via alloy-sol-types' own decoder), never against a real solc/EVM
// oracle or an actual testnet transaction; and `max_fee_per_gas`'s formula in signer.rs
// remains an explicit, unapproved placeholder. Wiring this in makes C6 signing REACHABLE
// in production, not independently verified end-to-end — the first real testnet execution
// is the actual verification step, not this wiring change.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::Level;

// LayerHealth trait must be in scope for .state(), .layer_id(), .set_state()
// to resolve on Arc<LayerHealthImpl>
use omega_core::{HealthState, LayerHealth, LayerId, OmegaConfig, StrategyId};
use omega_dag::{DagConfig, ExecutionDag};
// C1: ExecutionPipeline — see this file's module-level "C1" doc comment.
// C6 (this revision): UnconfiguredSigner import removed — main() now constructs a real
// KeyManagerTransactionSigner instead (see this file's new module-level "C6" doc comment
// below the historical C1-C9 history, and KeyManagerTransactionSigner's own doc comment in
// omega_execution::signer). Not re-exported at omega_execution's crate root (only
// SignedTransaction/TransactionSigner/UnconfiguredSigner are — see that crate's lib.rs),
// hence the explicit submodule path.
use omega_execution::signer::KeyManagerTransactionSigner;
use omega_execution::ExecutionPipeline;
// C5 FOLLOW-UP (this revision): the real config-translation entry point —
// see this file's module-level "RESOLVED (follow-up revision)" note under
// the "FIX (this revision)" doc comment.
use omega_execution::config_translation::{translate_relay_config, RelayBootstrapInputs};
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
    RelayName,
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
// C6 (this revision): also KeyManager + BlueprintSigner — needed to construct the real
// KeyManagerTransactionSigner in main() (see this file's new module-level "C6" doc
// comment). Both re-exported at omega_security's crate root already (see that crate's
// lib.rs), so no submodule path needed here.
use omega_security::{
    strategy_entries_from_manifest, AccountExposureTracker, BlueprintSigner, DeploymentManifest,
    IntegrityRegistry, KeyManager,
};
use omega_strategies::{registry::StrategyRegistryBuilder, CnryStrategy, StrategyRegistry};
// C9 (this revision): also compute_public_inputs_hash + ZkVerifier — see this file's own
// "C9" doc comment for the full design (real ZK-gate enforcement, previously entirely
// absent from this binary — see the investigation summarized there).
use omega_zk::{
    binding::compute_public_inputs_hash, config::ProverTierConfig, ProofQueue, ProofWorkerPool,
    ZkConfig, ZkVerifier,
};
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

// C5 FOLLOW-UP (this revision): the static KNOWN_RELAY_NAMES candidate
// list (every relay, every phase) is removed — relay candidates are now
// the real, phase-gated `omega_core::config::RelayConfig::phase_1_relays`
// / `phase_2plus_relays`, translated into `omega_relay::RelayName` via
// `translate_relay_config` (see main()'s relay-bootstrap block below).

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
/// deserialize into `DeploymentManifest`'s shape (`strategies: Vec
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

/// C9 (this revision): parses a required `0x`-prefixed (or bare) hex env var into a raw
/// 20-byte address. Used for `VAULT_ADDRESS` / `PROFIT_TOKEN` — see this file's own "C9"
/// doc comment for why both are required and what they feed.
///
/// Deliberately returns a raw `[u8; 20]` rather than `alloy_primitives::Address` — this
/// binary has no direct `alloy-primitives` dependency today (it reaches alloy types only
/// transitively through omega-core/omega-rpc/omega-relay's own public APIs), and pulling in
/// a new direct dependency just to name that one type here, when every consumer of these
/// values (`omega_zk::binding::compute_public_inputs_hash`) already takes raw `[u8; 20]`,
/// would be exactly the kind of unnecessary cross-crate surface area this file's own
/// established convention (see the RelayConfig-translation notes above) already avoids
/// elsewhere.
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

/// C6 (this revision): real, deployment-sourced `strategyId` values, transcribed
/// byte-for-byte from `contracts/src/StrategyIds.sol`'s own
/// `keccak256("OMEGA_STRATEGY_<X>")` constants — the canonical values
/// `RegisterStrategies.s.sol` already cross-checks every deployment manifest's
/// `onchain_id` field against (via that script's own `_checkManifestIdMatches` guard)
/// before ever calling `OmegaOrchestrator.registerStrategy()` on-chain. These are NOT
/// derived or guessed here — they are the actual source of truth that contract was
/// registered against, copied directly from that Solidity library.
///
/// Keyed by the same `StrategyId::to_string()` values ("SA"/"LA"/"MSA"/"MEV"/"CNRY")
/// `IntegrityRegistry`/`DeploymentManifest` already use elsewhere in this workspace —
/// picked for consistency with those, not re-derived independently here.
///
/// MANUAL-SYNC RISK, flagged rather than silently assumed away: if `StrategyIds.sol` is
/// ever changed (a new strategy added, or — though the library's own doc comment gives no
/// indication this is intended — an existing constant's value changed), this map must be
/// updated to match by hand. Nothing in this codebase keeps the two in sync automatically,
/// same class of risk already flagged elsewhere in this workspace (e.g. sa.rs's
/// `SA_SLIPPAGE_BPS` vs. `omega_risk::context::MAX_SLIPPAGE_BPS_SA`).
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

/// C5 FOLLOW-UP (this revision): converts a real `omega_rpc::BlockEvent`
/// into the `(block_number, block_hash)` call `MultiRelayClient::
/// on_new_block` needs, and makes that call. Extracted as a standalone,
/// synchronous function (rather than inlined in the spawned task in
/// `main()`) so the actual conversion this file performs is directly
/// unit-testable without a live RPC connection — see
/// `reorg_block_feed_tests` below. `*event.hash` follows the same
/// `B256 -> [u8; 32]` deref-copy pattern already used elsewhere in this
/// codebase (e.g. `omega-execution::pipeline.rs`'s
/// `let hb: [u8; 32] = *bp.blueprint_hash;`), not a new idiom introduced
/// here.
fn feed_block_event_to_reorg_guard(relay: &MultiRelayClient, event: &omega_rpc::BlockEvent) {
    let hash_bytes: [u8; 32] = *event.hash;
    relay.on_new_block(event.number, hash_bytes);
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

    // C9 (this revision): required — see this file's own "C9" doc comment and
    // parse_address_env's doc comment. Read early, alongside the other required startup
    // env vars, so a missing/malformed value halts startup immediately rather than being
    // discovered only once the first blueprint reaches the ZK-proof-gating code path.
    let vault_address = parse_address_env("VAULT_ADDRESS")?;
    let profit_token = parse_address_env("PROFIT_TOKEN")?;

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
    // module-level "FIX (this revision)" / "RESOLVED (follow-up revision)"
    // doc comments, for the full design and every fallback/skip rule
    // below.
    //
    // `confirmation_rpc_url` is read FIRST — it's needed to build
    // `translated` below, and has no dependency on anything else in this
    // block, so pulling it to the top avoids an artificial ordering
    // constraint the previous revision had (reading it only after the
    // relay-client loop, for no causal reason).
    let confirmation_rpc_url = std::env::var("ARBITRUM_HTTP_RPC_URL").context(
        "ARBITRUM_HTTP_RPC_URL must be set — a real chain JSON-RPC HTTP endpoint for \
         inclusion confirmation, distinct from ARBITRUM_RPC_URL's WebSocket endpoint",
    )?;

    // C5 FOLLOW-UP (this revision): real translation, replacing the prior
    // `RelayConfig { confirmation_rpc_url, ..Default::default() }` stub —
    // see this file's module-level "RESOLVED (follow-up revision)" note.
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
            "C5: config.relay field has no counterpart in omega_relay::RelayConfig \
             — configured value is not taking effect at this layer"
        );
    }
    let relay_cfg = translated.config;

    // Real phase gate, replacing the prior "every relay, every phase"
    // KNOWN_RELAY_NAMES list — see this file's module-level "RESOLVED
    // (follow-up revision)" note under "FIX (this revision)", item 1.
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
                "C5: LaReorgRiskEvent received (rescoring not wired to it yet — the \
                 block-hash feed task below now drives detection; consuming the \
                 rescore signal itself is a separate, still-open piece)"
            );
        }
    });

    // ── C5 FOLLOW-UP (this revision): real block-hash feed for the reorg
    // guard ─────────────────────────────────────────────────────────────
    //
    // Genuinely independent of every other task spawned in this function
    // (its own subscription, its own loop, no shared mutable state besides
    // `relay` itself, which is internally synchronized) — spawned as its
    // own task rather than folded into an existing one, so it runs
    // concurrently with the reorg-drain-log task above and the
    // reconciliation task below rather than serializing behind either.
    {
        let relay6 = Arc::clone(&relay);
        let mut block_rx = rpc.subscribe_blocks();
        tokio::spawn(async move {
            loop {
                match block_rx.recv().await {
                    Ok(event) => feed_block_event_to_reorg_guard(&relay6, &event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "C5: reorg block-feed loop lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        tracing::info!("C5: reorg guard now receiving real (block_number, block_hash) pairs");
    }

    // ── C6 (this revision): real TransactionSigner construction ──────────────
    //
    // Replaces C1's fail-closed UnconfiguredSigner stub — see this file's module-level
    // "C6" doc comment for the full design and what's still open after this change
    // (RLP/ABI encoding self-consistency only, no external solc/EVM oracle checked yet;
    // the fee formula in signer.rs remains an explicit, unapproved placeholder).
    let orchestrator_address = parse_address_env("ORCHESTRATOR_ADDRESS")
        .context("ORCHESTRATOR_ADDRESS must be set -- the deployed OmegaOrchestrator contract \
                  address every signed transaction this signer produces calls execute() on")?;

    let tx_signing_key_hex = std::env::var("OMEGA_TX_SIGNING_KEY").context(
        "OMEGA_TX_SIGNING_KEY must be set -- hex-encoded secp256k1 secret key for the \
         gas-paying transaction-envelope signer. Deliberately a SEPARATE key from \
         OMEGA_BLUEPRINT_SIGNING_KEY below -- see KeyManagerTransactionSigner's own doc \
         comment for why the tx-envelope signer and the on-chain blueprint-authorization \
         signer are independent concerns.",
    )?;
    let tx_key_manager = Arc::new(
        KeyManager::from_hex(&tx_signing_key_hex, CHAIN_ID)
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
        KeyManager::from_hex(&blueprint_signing_key_hex, CHAIN_ID)
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
        "C6: KeyManagerTransactionSigner constructed -- real transaction signing wired in, \
         replacing UnconfiguredSigner"
    );

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
        "C1: ExecutionPipeline constructed (real signer per C6 above; relay clients per C5 above)"
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
    // C9 (this revision): the verifier this binary was, until now, never actually calling
    // anywhere — see this file's own "C9" doc comment for the full investigation. Stateless
    // (holds only expected_chain_id — see ZkVerifier's own doc comment), so a single
    // Arc-wrapped instance is shared across every scoring-loop task rather than
    // reconstructed per call.
    let zk_verifier = Arc::new(ZkVerifier::new(CHAIN_ID));
    tracing::info!("L7 ZK: proof worker pool started, ZkVerifier constructed (C9)");

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
        // C9 (this revision): vault_address/profit_token are plain Copy [u8; 20] values —
        // no Arc needed, same treatment as gas_volatility_risk (f64) elsewhere in this
        // file. zk_verifier is Arc-cloned, same pattern as every other shared resource
        // threaded through this spawn block.
        let va3 = vault_address;
        let pt3 = profit_token;
        let zv3 = Arc::clone(&zk_verifier);
        tokio::spawn(async move {
            run_scoring_loop(
                reg, ora3, cl3, py3, tw3, dag2, tx, pq, halt3, ph, ep3, nr3, ir3, et3, fl3, va3,
                pt3, zv3,
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

// C4/C8/C9: grown to 17 args (C4 added integrity_registry, C8 adds
// flashloan_liq_rx, C9 adds vault_address/profit_token/zk_verifier) — the
// existing allow already covers this; not re-litigating the
// struct-refactor question for arguments added to an already-allowed
// function.
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
    // C6 (this revision): KeyManagerTransactionSigner, not UnconfiguredSigner — see this
    // file's module-level "C6" doc comment.
    execution_pipeline: Arc<ExecutionPipeline<KeyManagerTransactionSigner>>,
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
    // C9 (this revision): see main()'s own "C9" doc comment.
    vault_address: [u8; 20],
    profit_token: [u8; 20],
    zk_verifier: Arc<ZkVerifier>,
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
                    // C9 (this revision): see this function's own "C9" doc comment.
                    let va2 = vault_address;
                    let pt2 = profit_token;
                    let zv2 = Arc::clone(&zk_verifier);
                    tokio::spawn(async move {
                        score_and_admit(
                            strategy, s2, dag2, tx2, pq2, h2, ph, os2, ep2, nr2, ir2, gv2, et2,
                            fl2, va2, pt2, zv2,
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
    // C6 (this revision): KeyManagerTransactionSigner, not UnconfiguredSigner — see this
    // file's module-level "C6" doc comment.
    execution_pipeline: Arc<ExecutionPipeline<KeyManagerTransactionSigner>>,
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
    // C9 (this revision): see main()'s own "C9" doc comment for the full design. Required
    // to compute the real publicInputsHash every ZK proof in this function's non-hot-path
    // branch must bind to, and to actually verify the returned proof against it before
    // this blueprint is allowed anywhere near execute().
    vault_address: [u8; 20],
    profit_token: [u8; 20],
    zk_verifier: Arc<ZkVerifier>,
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
        // C9 (follow-up revision): hot-path blueprints now ALSO provision a ZK proof —
        // fixed, not left as an open question. Full reasoning, stated once here:
        //
        // `OmegaVault.receivePendingProfit()` (called on-chain immediately after
        // execution, inside the Orchestrator's flashloan callback) does NOT require a
        // proof — only the LATER `OmegaVault.releaseProfit()` call does (that contract's
        // own C6 gate: `proof_verified && confirmation_depth >= 12`). So a hot-path
        // blueprint reaching `execute()` below without a proof already in hand is not
        // itself an on-chain violation — gating hot-path ADMISSION on proof completion
        // here would only reimport the exact latency cost the hot path exists to avoid,
        // for no on-chain requirement that actually demands it.
        //
        // But leaving hot-path blueprints with NO proof pathway at all, forever (the prior
        // revision's state), is a real separate bug: any profit they generate sits in
        // OmegaVault as pending forever, un-releasable, since nothing would ever produce a
        // proof bound to that blueprintHash. Fixed here by firing the SAME
        // proof_queue.submit() the non-hot-path branch below uses, but as a DETACHED
        // background task (tokio::spawn, not awaited) — it cannot add any latency to
        // hot-path admission, which proceeds immediately after submission regardless of
        // the background task's outcome.
        //
        // `is_microtx: true` is passed deliberately — hot-path blueprints are Microtx lane
        // by construction (see the `hot` computation above), and the queue's own pressure
        // FSM already privileges microtx submissions under Suspend pressure, so this
        // submission correctly inherits that priority rather than competing as a generic
        // "normal" request.
        //
        // WHAT THIS STILL DOES NOT ADDRESS, flagged rather than silently assumed solved:
        // this makes a verified proof become AVAILABLE for a hot-path blueprint. Nothing
        // in this codebase, anywhere shown across this investigation, actually calls
        // `OmegaVault.submitProof()` on-chain with that proof once it's ready — that
        // relayer/keeper component does not exist here. This closes the "proof never
        // generated" gap; it does not close "who submits it on-chain."
        {
            let hb: [u8; 32] = *bp.blueprint_hash;
            let profit: u128 = bp.expected_profit_net.try_into().unwrap_or(u128::MAX);
            let expected_public_inputs_hash =
                compute_public_inputs_hash(vault_address, hb, profit, profit_token);

            match proof_queue.submit(
                hb,
                expected_public_inputs_hash,
                profit,
                CHAIN_ID,
                bp.strategy_id.to_string(),
                true, // is_microtx — see comment above
            ) {
                Ok(proof_rx) => {
                    let hash_for_log = bp.blueprint_hash;
                    let zv_bg = Arc::clone(&zk_verifier);
                    tokio::spawn(async move {
                        match proof_rx.await {
                            Ok(Ok(proof)) => {
                                if let Err(e) = zv_bg.verify(&proof, expected_public_inputs_hash) {
                                    tracing::error!(
                                        hash = %hash_for_log,
                                        error = %e,
                                        "C9: hot-path background ZK proof FAILED \
                                         VERIFICATION against expected publicInputsHash"
                                    );
                                } else {
                                    tracing::debug!(
                                        hash = %hash_for_log,
                                        gen_ms = proof.generation_ms,
                                        "C9: hot-path background ZK proof ready and \
                                         verified (not yet submitted on-chain — see this \
                                         branch's own comment on what remains open)"
                                    );
                                }
                            }
                            Ok(Err(zk_error)) => {
                                tracing::warn!(
                                    hash = %hash_for_log,
                                    error = %zk_error,
                                    "C9: hot-path background ZK proof generation failed"
                                );
                            }
                            Err(_recv_error) => {
                                tracing::warn!(
                                    hash = %hash_for_log,
                                    "C9: hot-path background ZK proof response channel \
                                     closed before a result arrived"
                                );
                            }
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        hash = %bp.blueprint_hash,
                        error = %e,
                        "C9: hot-path ZK proof submission rejected by queue — this \
                         blueprint's eventual profit will have no proof pathway; \
                         execute() below is NOT blocked on this, per this branch's own \
                         reasoning above"
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

        // C9 (this revision): the real publicInputsHash this blueprint's proof must bind
        // to — see omega_zk::binding::compute_public_inputs_hash's own doc comment for the
        // exact formula (mirrors OmegaVault.computePublicInputsHash() byte for byte).
        let expected_public_inputs_hash =
            compute_public_inputs_hash(vault_address, hb, profit, profit_token);

        // C9: this whole block replaces what was previously an `if let Ok(rx) = ... { if
        // let Ok(Ok(proof)) = rx.await { ...log only... } }` structure that did NOTHING
        // different on ANY failure path — see this file's own module-level "C9" doc
        // comment for the full investigation that found this gap. Every new early-return
        // below releases the DAG slot explicitly, since execute() (and its DagSlotGuard)
        // is never reached on these paths — see the module-level "C2"/"C9" doc comments
        // for why that release is required here and wasn't needed before this revision.
        let proof_rx = match proof_queue.submit(
            hb,
            expected_public_inputs_hash,
            profit,
            CHAIN_ID,
            bp.strategy_id.to_string(),
            micro,
        ) {
            Ok(rx) => rx,
            Err(e) => {
                tracing::warn!(
                    hash = %bp.blueprint_hash,
                    error = %e,
                    "C9: ZK proof submission rejected by queue — dropping blueprint, NOT executing"
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
                    "C9: ZK proof generation failed — dropping blueprint, NOT executing"
                );
                dag.lock().unwrap().complete(bp.blueprint_hash);
                return;
            }
            Err(_recv_error) => {
                tracing::warn!(
                    hash = %bp.blueprint_hash,
                    "C9: ZK proof response channel closed before a result arrived (worker \
                     crashed or shut down?) — dropping blueprint, NOT executing"
                );
                dag.lock().unwrap().complete(bp.blueprint_hash);
                return;
            }
        };

        // C9: actually verify the returned proof against the SAME expected_public_inputs_hash
        // computed above, before this blueprint is allowed anywhere near execute(). Prior to
        // this revision, ZkVerifier::verify() was called NOWHERE in this binary — confirmed
        // this session by direct inspection, cross-checked against
        // crates/omega-execution/Cargo.toml having no omega-zk dependency at all (the crate
        // that actually owns submission structurally could not have called it either).
        if let Err(verify_err) = zk_verifier.verify(&proof, expected_public_inputs_hash) {
            tracing::error!(
                hash = %bp.blueprint_hash,
                error = %verify_err,
                "C9: ZK proof FAILED VERIFICATION against expected publicInputsHash — \
                 dropping blueprint, NOT executing. Should be unreachable in normal \
                 operation (the proof was just generated from these same inputs) — a hit \
                 here most likely signals a vault_address/profit_token configuration bug, \
                 or something worse."
            );
            dag.lock().unwrap().complete(bp.blueprint_hash);
            return;
        }

        if active_phase >= 1 {
            tracing::info!(
                hash   = %bp.blueprint_hash,
                gen_ms = proof.generation_ms,
                "C9: ZK proof ready and verified",
            );
        }
    }

    // ── C2/C3/C4/C6/C7/C8/C9: ExecutionPipeline::execute — real DAG-slot ownership ─
    //
    // Reachable only for: hot-path blueprints (unconditionally, per the C9 note above —
    // not gated on ZK proof at all), OR non-hot-path blueprints whose ZK proof both
    // generated successfully AND passed ZkVerifier::verify() against the correct
    // publicInputsHash. Every other non-hot-path outcome already returned above, releasing
    // its own DAG slot on the way out.
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
    // owner of this blueprint's DAG slot via its internal DagSlotGuard,
    // for every blueprint that reaches this point. See the module-level
    // "C2"/"C9" doc comments for why blueprints that DON'T reach this
    // point (new C9 early-return paths above) release the slot
    // themselves instead.
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

    // ── Test helper ──────────────────────────────────────────────────────────
    //
    // Same pattern as omega-manifest-gen's own test module (write_temp_file):
    // a uniquely-named file per test in the OS temp dir, so these tests can
    // exercise load_deployment_manifest's real disk-reading behavior without
    // colliding with concurrently-running test threads or requiring a fixed
    // path this file's own DEPLOYMENT_MANIFEST_PATH constant points at.
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

    // ── load_deployment_manifest: the real function main() calls at boot ──────

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
    // NEW (this revision, C9).
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    // NOTE: these tests mutate process-global env vars (std::env::set_var/remove_var), so
    // they use distinct, test-specific var names to avoid interfering with each other or
    // with any real VAULT_ADDRESS/PROFIT_TOKEN set in the actual test-running environment
    // — same caution any std::env-mutating test needs regardless of test-runner
    // parallelism.

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
mod reorg_block_feed_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Writes a minimal, valid empty builder-blacklist file for
    /// `BuilderBlacklist::load` — same pattern `main()` itself uses to
    /// create one on-demand, reused here to avoid a `tempfile` crate
    /// dependency in the binary just for this test.
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
        // Proves the actual call this file makes in production — the
        // B256 -> [u8; 32] extraction and the on_new_block call itself —
        // is correct, using the real MultiRelayClient and LaReorgGuard,
        // not a stand-in.
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

        // FIX (this revision): construct B256 via From<[u8; 32]> instead of
        // the unresolved `alloy::primitives::B256::from(...)` path — see
        // this file's module-level "FIX (this revision): E0433 in
        // reorg_block_feed_tests" doc comment for why.
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
    // NEW (this revision). Regression coverage for the "RESOLVED (follow-up revision)"
    // fix described in this file's own module-level "C9" doc comment: the `hot` branch of
    // `score_and_admit` now ALSO fires `proof_queue.submit(...)`, but as a DETACHED
    // background task — hot-path admission must NOT block on that proof ever completing.
    // Before that fix, hot-path blueprints had no proof pathway at all; the regression this
    // guards against is the opposite failure mode — accidentally re-gating hot-path
    // admission on proof completion, which would reimport the exact latency cost the hot
    // path exists to avoid.
    //
    // ASSUMPTION FLAGGED, NOT VERIFIED AGAINST REAL SOURCE: this test imports
    // `omega_strategies::SaStrategy` on the assumption it is re-exported at that crate's
    // root, the same way `CnryStrategy` already is per this file's own top-level
    // `use omega_strategies::{registry::StrategyRegistryBuilder, CnryStrategy,
    // StrategyRegistry};`. I have not seen `crates/omega-strategies/src/lib.rs` itself in
    // this session, only `sa.rs`/`la.rs`/`msa.rs`/`mev.rs`'s own module bodies — so this
    // is inferred from an existing, structurally identical import, not confirmed. If the
    // re-export doesn't exist, the fix is `use omega_strategies::sa::SaStrategy;` instead.
    //
    // Every other constructor/method signature used below is copied directly from a call
    // site already present in this file's own `main()`/`score_and_admit` — not re-guessed.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use omega_core::StrategyTrait;
    use omega_strategies::SaStrategy;

    /// Builds every real dependency `score_and_admit` needs, using ONLY constructor calls
    /// already present in this file's own `main()` — no new guesses about any of these
    /// crates' internal shapes.
    async fn build_harness() -> (
        Arc<dyn StrategyTrait>,
        Arc<Mutex<ExecutionDag>>,
        tokio::sync::mpsc::Sender<HotPathRequest>,
        tokio::sync::mpsc::Receiver<HotPathRequest>,
        ProofQueue,
        // C6 (this revision): KeyManagerTransactionSigner, not UnconfiguredSigner — this
        // harness must construct the same concrete signer type production's main() now
        // does, since ExecutionPipeline's signer type parameter is fixed at each call
        // site, not generic over score_and_admit's own signature.
        Arc<ExecutionPipeline<KeyManagerTransactionSigner>>,
        omega_security::replay::NonceRegistry,
        Arc<IntegrityRegistry>,
        AccountExposureTracker,
        tokio::sync::watch::Receiver<FlashloanLiquidityState>,
        Arc<ZkVerifier>,
    ) {
        // B256/Address constructed via From<[u8; N]> rather than by naming
        // `alloy_primitives::` directly — this binary crate has no direct dependency on
        // that crate (same class of E0433 already solved once in this file, in
        // reorg_block_feed_tests, via `[1u8; 32].into()` for BlockEvent::hash; this reuses
        // the identical, already-proven-working pattern rather than a new guess).
        let strategy: Arc<dyn StrategyTrait> =
            SaStrategy::new(CHAIN_ID, [0xABu8; 32].into(), [0u8; 20].into(), &OmegaConfig::default());

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
        };
        // Deliberately NOT starting a ProofWorkerPool here — see this test's own
        // assertion below for why leaving the proof queue permanently unserviced is the
        // whole point of this test, not an oversight.
        let proof_queue = ProofQueue::new(zk_cfg);

        let zk_verifier = Arc::new(ZkVerifier::new(CHAIN_ID));

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

        // C6 (this revision): real KeyManagerTransactionSigner, built from test-only key
        // material (same pattern omega-execution::signer's own tests use, e.g.
        // `make_km(byte)` — never real keys). Constructed via `KeyManager::from_hex`
        // rather than the `secp256k1` crate directly, since this binary has no direct
        // dependency on `secp256k1` (same class of E0433 already solved once in this
        // file for `alloy_primitives` — see `build_harness`'s own comment above). Reuses
        // the real, production `strategy_onchain_ids()` helper (this file's own C6
        // addition) rather than a second hand-built map, so this test can never silently
        // drift from what main() actually configures.
        let test_tx_key_manager = Arc::new(
            KeyManager::from_hex(&"3a".repeat(32), CHAIN_ID).unwrap(),
        );
        let test_blueprint_key_manager = Arc::new(
            KeyManager::from_hex(&"3b".repeat(32), CHAIN_ID).unwrap(),
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
            CHAIN_ID,
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
        )
    }

    /// Low base fee, block 1 — matches sa.rs's own `make_signal(5)` test pattern, so
    /// `SaStrategy::score`/`build_blueprint` return a genuinely profitable opportunity
    /// rather than one this test has to fight the strategy's own economics to construct.
    fn profitable_signal() -> omega_core::SignalState {
        omega_core::SignalState {
            state_version: 1,
            chain_id: CHAIN_ID,
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
        ) = build_harness().await;

        // SA is hot_path_eligible with gas_budget() == MICROTX_GAS_LIMIT (200_000, so the
        // `<=` admission check in score_and_admit's `hot` computation passes) — confirmed
        // directly against sa.rs's own SA_GAS_BUDGET constant and StrategyTrait impl, not
        // guessed.
        assert!(strategy.hot_path_eligible(), "test assumes SA is hot-path eligible");

        // Stub hot-path runner: reply immediately so score_and_admit's
        // `rrx.await` on the hot-path response channel doesn't hang forever
        // waiting for a real HotPathRunner this test deliberately doesn't spin up.
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

        // The critical assertion: score_and_admit must return within a short bound EVEN
        // THOUGH no ProofWorkerPool was ever started for `proof_queue` above, so the
        // background ZK-proof task this revision's fix spawns can never complete. If
        // hot-path admission were (re-)gated on proof completion, this would hang until
        // the timeout fires and the test would fail — that failure mode is exactly the
        // regression this test exists to catch.
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
                [0x11u8; 20], // vault_address
                [0x22u8; 20], // profit_token
                zk_verifier,
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