// crates/omega-execution/src/pipeline.rs
//
// ExecutionPipeline — implements Stages 0-6 of
// ExecutionPipelineSpecification.md. Stage 7 (confirmation reconciliation)
// is deliberately NOT part of this type — it's already fully implemented
// as omega_relay::MultiRelayClient::reconcile_inclusions and only needs a
// periodic caller-owned interval loop (same shape as main.rs's existing
// run_health_monitor), which belongs in the binary that constructs
// everything else, not in this library crate. Wiring reconcile_inclusions'
// output into KillSwitchRegistry::record_outcome additionally requires
// confirming ConfirmationResult's exact field set (does it carry
// strategy_id / realized_profit_wei, or only relay/included?) —
// confirmation.rs was not read in the investigation that produced this
// crate, so that wiring is left undone here rather than guessed at.
//
// ## Resolutions to ExecutionPipelineSpecification.md §12's open questions
//
// 1. Crate placement: new `omega-execution` crate (this one), not inline
//    in main.rs. Reason found after the spec was written: omega-simulation
//    ::traits.rs's own doc comment names the live BundleSubmitter
//    implementation's future home as "omega-execution" by that exact
//    name — third independent reference to a dedicated crate (after
//    omega-dag's and omega-hot-path's "the orchestrator"), and the only
//    one that's a literal crate name rather than a description.
// 2. omega-security::signer.rs: read — see signer.rs's TransactionSigner
//    doc comment for the full resolution (it authorizes, doesn't sign a
//    tx envelope; this pipeline depends on a trait instead of fabricating
//    a raw-tx signer).
// 3. Kill-switch scope key: strategy-level (bp.strategy_id.to_string()),
//    matching every KillSwitchRegistry example already in this codebase.
// 4. Cascade vs. single submission: Lane::Microtx -> submit_single,
//    Lane::Normal -> cascade_submit. CORRECTION (this revision): this was
//    previously presented alongside genuinely spec-verified resolutions
//    (like #1 and #2) in a way that blurred the distinction. It is NOT
//    documented in blueprint.rs, the spec, or any file read while
//    building this crate — it's this pipeline's own inferred policy
//    (latency-sensitive hot path -> single relay; LA-style trades ->
//    multi-relay backpressure per spec S11.2), stated in code so it's a
//    visible, overridable decision rather than a silent one. Treat it as
//    a placeholder policy pending confirmation from whoever owns the
//    actual trading-strategy design — not as a resolved open question.
// 5. hot-path <-> relay spec/code mismatch: NOT resolved here — orthogonal
//    to this crate's own correctness, since omega-execution depends on
//    omega-relay directly regardless of what omega-hot-path does. Left
//    open for a separate decision.
// 6. LaReorgRiskEvent receiver: NOT resolved here — still the caller's
//    (main.rs's) responsibility to own and drain once MultiRelayClient is
//    constructed. This pipeline only calls the synchronous
//    `on_bundle_submitted` bookkeeping method, which doesn't require
//    draining that channel.
//
// ## Fix (this revision): expected_profit_net overflow fails open
//
// `blueprint_to_check_fields` previously mapped
// `bp.expected_profit_net.try_into().unwrap_or(u128::MAX)`. Confirmed
// against `omega_risk::checks::check_dynamic_profit`'s real body
// (`bp.expected_profit_net_wei < bp.dynamic_min_profit_wei`) that an
// overflow coerced to `u128::MAX` makes this comparison false for any
// realistic threshold — i.e. the exact check meant to reject an
// unprofitable blueprint would instead PASS a garbage-large one. Fixed
// by rejecting the overflow via the new `ExecutionError::
// BlueprintFieldOverflow` variant (see error.rs) instead of coercing it.
// `dynamic_min_profit` and `flashloan_amount` are unchanged — their
// overflow direction fails CLOSED (confirmed against
// `check_dynamic_profit`, `check_flashloan_liquidity`, and
// `check_account_exposure`'s real bodies), so coercing to `u128::MAX`
// there is the conservative, correct behavior already.

use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256};
use omega_core::types::blueprint::ExecutionBlueprint;
use omega_core::types::lane::Lane;
use omega_relay::MultiRelayClient;
use omega_risk::checks::{run_all_checks, BlueprintFields, CheckResult};
use omega_risk::context::CheckContext;
use omega_risk::kill_switch::KillSwitchRegistry;

use crate::error::ExecutionError;
use crate::idempotency::IdempotencyCache;
use crate::signer::TransactionSigner;
use crate::transform::build_bundle_payload;

// ─── DAG slot RAII guard ───────────────────────────────────────────────────
//
// ## Audit fix (this revision): DAG slot could leak on a panic
//
// `execute()` previously called `dag.complete(bp.blueprint_hash)` as a
// plain statement placed AFTER `execute_inner(...).await` returned. If a
// panic unwound through `execute_inner` before it returned — nothing in
// Stage 0/1 panics today (no unwrap/expect/indexing in that path), but
// nothing structurally prevented one being introduced later — that
// cleanup line would simply never run, permanently leaking that
// blueprint's DAG slot. `DagSlotGuard` fixes this with `Drop`: the slot
// is released either explicitly via `release()` on the normal path, or
// by `Drop` if a panic skips that call.
//
// This requires the DAG to be behind a `std::sync::Mutex`, not
// `tokio::sync::Mutex` as in the previous revision — `Drop::drop` cannot
// run async code, so an async mutex can't be locked inside it.
// `omega_dag::scheduler.rs`'s `ExecutionDag` methods contain no `.await`
// points internally, so a synchronous mutex was always the more correct
// choice here, not a downgrade; the only new concern it introduces is
// lock poisoning (which `tokio::sync::Mutex` doesn't have), handled below
// by recovering the poisoned guard rather than panicking again inside
// cleanup code.
//
// Caveat, stated once here rather than repeated at every call site: this
// protects against `panic = "unwind"` (Rust's default). It provides no
// protection under `panic = "abort"`, since no `Drop` impl anywhere can
// run in that configuration — that is a build setting outside this
// crate's control, not something this guard can compensate for.
struct DagSlotGuard {
    dag: Arc<Mutex<omega_dag::ExecutionDag>>,
    blueprint_hash: B256,
    released: bool,
}

impl DagSlotGuard {
    fn new(dag: Arc<Mutex<omega_dag::ExecutionDag>>, blueprint_hash: B256) -> Self {
        Self {
            dag,
            blueprint_hash,
            released: false,
        }
    }

    /// Explicit release on the normal (non-panicking) path, so the slot
    /// frees as soon as Stage 6 actually finishes rather than being
    /// deferred to `Drop` unnecessarily.
    fn release(mut self) {
        self.do_release();
        self.released = true;
        // `self` drops here; the Drop impl below sees `released == true`
        // and no-ops, so the slot is freed exactly once.
    }

    fn do_release(&self) {
        match self.dag.lock() {
            Ok(mut g) => {
                g.complete(self.blueprint_hash);
            }
            Err(poisoned) => {
                // A prior panic while some other call held this lock
                // already poisoned it. Recover the guard anyway (best
                // effort) rather than panicking again inside cleanup
                // code — a second panic here would abort a panic
                // already in progress instead of letting it unwind.
                let mut g = poisoned.into_inner();
                g.complete(self.blueprint_hash);
            }
        }
    }
}

impl Drop for DagSlotGuard {
    fn drop(&mut self) {
        if !self.released {
            self.do_release();
        }
    }
}

/// Outcome of a successful `ExecutionPipeline::execute` call.
#[derive(Debug, Clone)]
pub enum ExecutionOutcome {
    /// `active_phase < 1` — pipeline intentionally did nothing (Stage 0,
    /// matches main.rs's existing "Phase 0: shadow mode — relay
    /// submission suppressed" behavior).
    Suppressed,
    /// Submitted via `MultiRelayClient::submit_single`.
    SubmittedSingle { any_accepted: bool },
    /// Submitted via `MultiRelayClient::cascade_submit`.
    SubmittedCascade {
        any_accepted: bool,
        relay_count: usize,
    },
}

/// Bridges `ExecutionBlueprint` to `omega_relay::MultiRelayClient`, gated
/// by `omega_risk`'s kill switch, `omega_security`'s bytecode integrity
/// registry, and 15 pre-trade checks. Generic over `S: TransactionSigner`
/// so this type is fully constructible and testable without a real
/// transaction signer existing yet — see signer.rs.
pub struct ExecutionPipeline<S: TransactionSigner> {
    kill_switches: Arc<KillSwitchRegistry>,
    integrity_registry: Arc<omega_security::IntegrityRegistry>,
    relay: Arc<MultiRelayClient>,
    dag: Arc<Mutex<omega_dag::ExecutionDag>>,
    idempotency: IdempotencyCache,
    signer: Arc<S>,
    chain_id: u64,
}

impl<S: TransactionSigner> ExecutionPipeline<S> {
    pub fn new(
        kill_switches: Arc<KillSwitchRegistry>,
        integrity_registry: Arc<omega_security::IntegrityRegistry>,
        relay: Arc<MultiRelayClient>,
        dag: Arc<Mutex<omega_dag::ExecutionDag>>,
        signer: Arc<S>,
        chain_id: u64,
    ) -> Self {
        Self {
            kill_switches,
            integrity_registry,
            relay,
            dag,
            idempotency: IdempotencyCache::new(),
            signer,
            chain_id,
        }
    }

    /// Run Stages 0-6 for `bp`. `risk_ctx` must be assembled fresh by the
    /// caller from live state at submission time (spec Stage 2) — this
    /// pipeline does not construct a `CheckContext` itself, since it has
    /// no oracle/flashloan/competition/risk-score sources of its own to
    /// build one from; see open question 2's Stage-2 discussion in the
    /// spec for why that assembly is the caller's responsibility.
    ///
    /// PRECONDITION: `bp` must already have been admitted to `dag` (via
    /// `ExecutionDag::admit`) before this call — this pipeline frees a
    /// DAG slot in Stage 6, it does not claim one in Stage 0. This
    /// mirrors main.rs::score_and_admit's existing structure, where
    /// `dag.admit(...)` happens before hot-path/ZK dispatch, i.e. before
    /// this pipeline would ever be invoked. Calling `execute()` for a
    /// blueprint that was never admitted is not a crash (`ExecutionDag::
    /// complete` on an unknown hash logs a warning and returns an empty
    /// Vec rather than panicking), but it is a caller bug worth guarding
    /// against with an assertion in integration code, since it silently
    /// signals a slot was never actually held.
    ///
    /// Safe to call concurrently for distinct blueprints from multiple
    /// tasks: no lock spans the whole call. `kill_switches` and
    /// `idempotency` are `DashMap`-backed (lock-free-ish concurrent
    /// access), `relay` is designed for concurrent submission
    /// internally, and `dag` is only briefly locked at the very end via
    /// `DagSlotGuard` — Stages 0 through 5 hold no lock on it at all. See
    /// `tests::concurrent_distinct_blueprints_do_not_serialize` and
    /// `tests::concurrent_duplicate_blueprint_exactly_one_wins` for the
    /// concurrency guarantees this claim rests on, not just this comment.
    pub async fn execute(
        &self,
        bp: ExecutionBlueprint,
        active_phase: u8,
        risk_ctx: &CheckContext,
        current_block: u64,
        current_block_timestamp_secs: u64,
    ) -> Result<ExecutionOutcome, ExecutionError> {
        // RAII guard: guarantees the DAG slot is released even if a panic
        // unwinds through execute_inner before Stage 6 would otherwise
        // run — see DagSlotGuard's doc comment above, including the
        // panic=unwind vs panic=abort caveat.
        let guard = DagSlotGuard::new(Arc::clone(&self.dag), bp.blueprint_hash);

        let result = self
            .execute_inner(
                &bp,
                active_phase,
                risk_ctx,
                current_block,
                current_block_timestamp_secs,
            )
            .await;

        // Stage 6 (partial): free the DAG slot on every exit path, success
        // or failure — mirrors what main.rs::score_and_admit already does
        // unconditionally today, just moved to run after submission
        // instead of immediately after hot-path/ZK readiness. Deferred
        // kill-switch outcome recording (realized_profit_wei) happens
        // later, in Stage 7 reconciliation, not here — profit isn't known
        // until on-chain confirmation.
        guard.release();

        result
    }

    /// Evict idempotency cache entries older than `max_age`. Exposed so a
    /// caller can wire this into a periodic task (same shape as
    /// `KillSwitchRegistry`'s and `SequencerRestartHandler`'s own
    /// eviction patterns) — nothing calls this automatically inside this
    /// crate, since this crate has no timer/scheduler of its own to drive
    /// one. Without a caller doing this periodically, the cache grows
    /// unbounded across a long-running process — see this crate's top
    /// doc comment for this exact caveat; this method makes eviction
    /// *possible*, it does not make it *automatic*.
    pub fn evict_idempotency_cache(
        &self,
        max_age: chrono::Duration,
        now: chrono::DateTime<chrono::Utc>,
    ) {
        self.idempotency.evict_older_than(max_age, now);
    }

    /// Current idempotency cache size, for observability / driving an
    /// eviction schedule.
    pub fn idempotency_cache_len(&self) -> usize {
        self.idempotency.len()
    }

    async fn execute_inner(
        &self,
        bp: &ExecutionBlueprint,
        active_phase: u8,
        risk_ctx: &CheckContext,
        current_block: u64,
        current_block_timestamp_secs: u64,
    ) -> Result<ExecutionOutcome, ExecutionError> {
        // ── Stage 0: phase gate ──────────────────────────────────────────
        if active_phase < 1 {
            tracing::debug!(
                blueprint_hash = %bp.blueprint_hash,
                "execution pipeline: phase 0 — relay submission suppressed"
            );
            return Ok(ExecutionOutcome::Suppressed);
        }

        // ── Stage 1: integrity check ─────────────────────────────────────
        if !bp.verify_hash() || !bp.verify_idempotency_key() {
            tracing::error!(
                blueprint_hash = %bp.blueprint_hash,
                "execution pipeline: blueprint failed integrity check — discarding, \
                 never submitting (same handling as SIMULATION_STATE_MISMATCH)"
            );
            return Err(ExecutionError::IntegrityFailure);
        }

        // ── Stage 2a: kill switch ────────────────────────────────────────
        let scope = bp.strategy_id.to_string();
        if let Err(e) = self.kill_switches.guard(&scope) {
            tracing::warn!(
                blueprint_hash = %bp.blueprint_hash,
                strategy = %scope,
                error = %e,
                "execution pipeline: kill switch tripped — dropping blueprint"
            );
            return Err(ExecutionError::KillSwitchTripped {
                scope,
                reason: e.to_string(),
            });
        }

        // ── Stage 2b: bytecode integrity registry (Gap 9) ────────────────
        //
        // Checks the blueprint's OWN claimed strategy_bytecode_hash
        // against IntegrityRegistry's expected hash + freeze status.
        // This is NOT equivalent to the on-chain Orchestrator's live
        // check (`keccak256(abi.encodePacked(stratAddr.codehash))`,
        // computed fresh from the currently-deployed contract at
        // execution time) — this pipeline has no RPC client wired in to
        // perform that live read (see production-integration-plan.md,
        // a further gap beyond this one). What this DOES catch: a
        // blueprint built against a stale or wrong bytecode assumption,
        // and — the more commonly hit case — a strategy that's been
        // frozen by governance, since `full_integrity_check` runs the
        // freeze check first.
        if let Err(e) = self
            .integrity_registry
            .full_integrity_check(&scope, &bp.strategy_bytecode_hash.0)
        {
            tracing::error!(
                blueprint_hash = %bp.blueprint_hash,
                strategy = %scope,
                error = %e,
                "execution pipeline: bytecode integrity check failed — dropping blueprint"
            );
            return Err(ExecutionError::IntegrityRegistryCheckFailed(e));
        }

        // ── Stage 2c: 15 pre-trade checks ────────────────────────────────
        let fields = blueprint_to_check_fields(bp)?;
        if let CheckResult::Fail(code) = run_all_checks(&fields, risk_ctx) {
            tracing::warn!(
                blueprint_hash = %bp.blueprint_hash,
                strategy = %scope,
                drop_code = ?code,
                "execution pipeline: pre-trade check failed — dropping blueprint"
            );
            return Err(ExecutionError::RiskCheckFailed(code));
        }

        // ── Stage 3: submission-layer idempotency dedup ──────────────────
        self.idempotency.check_and_mark(bp.idempotency_key)?;

        // ── Stage 4: ExecutionBlueprint -> BundlePayload transform ───────
        let payload = build_bundle_payload(
            bp,
            self.chain_id,
            current_block,
            current_block_timestamp_secs,
            self.signer.as_ref(),
        )
        .await?;

        // Captured before `payload` is moved into submission, for reorg
        // guard registration below.
        let bundle_hash_for_reorg_guard = payload.bundle_hash.clone();

        // ── Stage 5: submission ───────────────────────────────────────────
        // Resolution to open question 4: cascade for Normal lane (LA-style
        // — benefits from multi-relay backpressure per spec S11.2), single
        // for Microtx (hot-path — latency-sensitive, single relay). Stated
        // here as the decision, not deferred.
        let outcome = match bp.lane {
            Lane::Microtx => {
                let any_accepted = self.relay.submit_single(payload).await?;
                ExecutionOutcome::SubmittedSingle { any_accepted }
            }
            Lane::Normal => {
                let results = self.relay.cascade_submit(vec![payload]).await;
                let any_accepted = results.iter().any(|r| r.any_accepted);
                let relay_count = results.first().map(|r| r.relay_outcomes.len()).unwrap_or(0);
                ExecutionOutcome::SubmittedCascade {
                    any_accepted,
                    relay_count,
                }
            }
        };

        // ── Stage 6 (partial): reorg guard registration ───────────────────
        // Real, not deferred — MultiRelayClient::on_bundle_submitted
        // already exists and, per this investigation's earlier grep, was
        // called by nothing outside omega-relay's own tests before this
        // pipeline existed.
        self.relay.on_bundle_submitted(
            omega_relay::TxHash(bundle_hash_for_reorg_guard),
            current_block,
        );

        Ok(outcome)
    }
}

/// Extracts the subset of `ExecutionBlueprint` fields
/// `omega_risk::checks::BlueprintFields` needs. A direct 1:1 mapping for
/// every field except `flashloan_provider_id` — see
/// `resolve_flashloan_provider_id`'s doc comment for why that one field
/// fails closed instead of being filled with a placeholder.
///
/// ## U256 -> u128 mapping policy (this revision)
///
/// - `expected_profit_net`: MUST fit in `u128`. Overflow returns
///   `ExecutionError::BlueprintFieldOverflow` rather than coercing to
///   `u128::MAX` — confirmed against `check_dynamic_profit`'s real body
///   that coercion fails OPEN (`MAX < dynamic_min_profit_wei` is false
///   for any realistic threshold). See this file's module-level "Fix
///   (this revision)" note and error.rs's matching note.
/// - `dynamic_min_profit` / `flashloan_amount`: overflow still maps to
///   `u128::MAX` — confirmed this direction fails CLOSED in checks 5,
///   10, and 14 respectively, so coercion remains the correct,
///   conservative behavior for these two fields specifically. NOT
///   changed to the same overflow-rejecting treatment as
///   `expected_profit_net`, deliberately — the two fields have opposite
///   failure directions and should not be treated identically.
///
/// ## Fix (earlier revision): missing `nonce` field
///
/// `BlueprintFields` gained a `nonce: u64` field (see omega-risk::checks's
/// own audit note, check 15 / `StaleBlueprint`) that this mapping was
/// never updated for — `cargo build` fails with `E0063: missing field
/// nonce`. `bp.nonce` is a direct 1:1 copy, same as every other field
/// below; no derivation or fallback needed.
fn blueprint_to_check_fields(bp: &ExecutionBlueprint) -> Result<BlueprintFields, ExecutionError> {
    let flashloan_provider_id = resolve_flashloan_provider_id(bp.flashloan_provider)?;

    let expected_profit_net_wei: u128 =
        bp.expected_profit_net
            .try_into()
            .map_err(|_| ExecutionError::BlueprintFieldOverflow {
                field: "expected_profit_net",
            })?;

    Ok(BlueprintFields {
        chain_id: bp.chain_id,
        expiry_block: bp.expiry_block,
        l2_exec_gas_estimate: bp.l2_exec_gas_estimate,
        l1_data_gas_estimate: bp.l1_data_gas_estimate,
        extraction_gas: bp.extraction_gas,
        expected_profit_net_wei,
        dynamic_min_profit_wei: bp.dynamic_min_profit.try_into().unwrap_or(u128::MAX),
        l1_data_fee_at_creation: bp.l1_data_fee_at_creation,
        slippage_bps: bp.slippage_bps,
        flashloan_amount: bp.flashloan_amount.try_into().unwrap_or(u128::MAX),
        flashloan_provider_id,
        strategy_id: strategy_id_label(bp.strategy_id),
        strategy_bytecode_hash: bp.strategy_bytecode_hash.0,
        price_impact_bps: bp.price_impact_bps,
        ofa_compliant: bp.ofa_compliant,
        nonce: bp.nonce,
    })
}

/// Maps a flashloan provider's on-chain address to the protocol
/// identifier string `omega_risk::checks::check_flashloan_liquidity`'s
/// no-self-flash rule compares against `CheckContext::flashloan::protocol_id`.
///
/// NO ADDRESS-TO-PROTOCOL-NAME TABLE EXISTS ANYWHERE IN THE OMEGA-ENGINE
/// WORKSPACE as read in this investigation — `ExecutionBlueprint` carries
/// only a raw `Address`, never a protocol name string, and no other file
/// read in this thread maps one to the other. Fabricating real Aave/
/// Balancer/Compound/Morpho/Euler deployment addresses here would be
/// exactly the kind of placeholder data ruled out earlier in this
/// conversation. Fails closed (returns an error for any non-zero address)
/// rather than filling in an unmatchable sentinel string — a check that
/// LOOKS like it's running the no-self-flash rule but can never actually
/// fire is more dangerous than a loud, blocking error, since it would
/// silently defeat exactly the protection `check_flashloan_liquidity`
/// exists to provide.
fn resolve_flashloan_provider_id(addr: Address) -> Result<&'static str, ExecutionError> {
    if addr == Address::ZERO {
        // No flashloan used (ExecutionBlueprint's own doc comment: "Zero
        // address signals no flashloan — capital sourced from PIL") —
        // provider_id is genuinely irrelevant to the no-self-flash rule
        // in this case, so "none" is accurate, not a placeholder.
        return Ok("none");
    }
    Err(ExecutionError::UnknownFlashloanProvider {
        address: format!("{addr:?}"),
    })
}

fn strategy_id_label(id: omega_core::types::blueprint::StrategyId) -> &'static str {
    use omega_core::types::blueprint::StrategyId;
    match id {
        StrategyId::Sa => "SA",
        StrategyId::Cnry => "CNRY",
        StrategyId::Msa => "MSA",
        StrategyId::La => "LA",
        StrategyId::Mev => "MEV",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signer::MockTransactionSigner;
    use alloy_primitives::{Bytes, B256, U256};
    use omega_core::types::blueprint::StrategyId;
    use omega_core::types::lane::Simulator;
    use omega_dag::{DagConfig, ExecutionDag};
    use omega_relay::{
        BuilderBlacklist, ExecutionAddress, LaRelayMetrics, RelayClient, RelayConfig,
        SubmissionOutcome,
    };
    use omega_risk::context::{CheckContext, FlashloanSnapshot, OracleSnapshot};
    use omega_risk::kill_switch::{KillSwitchConfig, KillSwitchRegistry};
    use std::collections::HashMap;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use uuid::Uuid;

    // ── Local always-accepts relay client — avoids depending on
    // omega-relay's own #[cfg(test)]-only MockRelayClient across the
    // crate boundary, where that cfg wouldn't apply. RelayClient is a
    // public trait; implementing it locally in this crate's own tests is
    // the correct pattern, not a shortcut. ─────────────────────────────
    struct AlwaysAcceptRelay;

    #[async_trait::async_trait]
    impl RelayClient for AlwaysAcceptRelay {
        async fn submit_bundle(
            &self,
            bundle: omega_relay::BundlePayload,
        ) -> omega_relay::RelayResult<SubmissionOutcome> {
            Ok(SubmissionOutcome {
                accepted: true,
                relay_bundle_id: Some(bundle.bundle_hash),
            })
        }
        fn name(&self) -> &str {
            "always-accept"
        }
    }

    fn blacklist_file() -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "").unwrap(); // empty blacklist is valid
        f
    }

    fn make_relay() -> Arc<MultiRelayClient> {
        make_relay_with(Arc::new(AlwaysAcceptRelay), 100)
    }

    /// Generalized relay builder — lets property/load tests inject a
    /// custom `RelayClient` (latency, call-counting) and a custom
    /// per-relay rate limit, without duplicating `MultiRelayClient`
    /// construction (blacklist file, metrics, config) at every call site.
    fn make_relay_with(
        client: Arc<dyn RelayClient>,
        max_per_second: usize,
    ) -> Arc<MultiRelayClient> {
        let f = blacklist_file();
        let blacklist = BuilderBlacklist::load(f.path()).unwrap();
        let metrics = LaRelayMetrics::new(50, ExecutionAddress("0xTEST".into()));
        let mut clients: HashMap<String, Arc<dyn RelayClient>> = HashMap::new();
        clients.insert("flashbots".into(), client);
        let cfg = RelayConfig {
            stagger_ms: 0,
            max_bundles_per_relay_per_second: max_per_second,
            confirmation_rpc_url: "http://localhost:1".into(),
            ..Default::default()
        };
        let (mr, _event_rx) = MultiRelayClient::new(clients, metrics, blacklist, &cfg, 0);
        mr
    }

    fn make_kill_switches() -> Arc<KillSwitchRegistry> {
        Arc::new(
            KillSwitchRegistry::new(KillSwitchConfig {
                max_cumulative_loss_wei: 1_000_000_000_000_000_000,
                max_loss_per_window_wei: 200_000_000_000_000_000,
                loss_window: std::time::Duration::from_secs(3600),
                max_consecutive_failures: 5,
            })
            .unwrap(),
        )
    }

    fn make_dag() -> Arc<Mutex<ExecutionDag>> {
        make_dag_with_capacity(16)
    }

    /// Generalized DAG builder — load tests admitting hundreds/thousands
    /// of concurrent blueprints need `microtx_slots` raised well past the
    /// default 16, or every admit past capacity would correctly (not a
    /// bug) fail with `DagError::LaneFull` before the pipeline is ever
    /// invoked, which would test the DAG's capacity gate instead of this
    /// crate's concurrency behavior.
    fn make_dag_with_capacity(microtx_slots: usize) -> Arc<Mutex<ExecutionDag>> {
        Arc::new(Mutex::new(ExecutionDag::new(DagConfig {
            microtx_slots,
            normal_slots: 4,
            eviction_log_capacity: 10_000,
        })))
    }

    fn passing_ctx(strategy_bytecode_hash: [u8; 32]) -> CheckContext {
        CheckContext {
            expected_chain_id: 42161,
            current_block: 500,
            current_l1_gas_price_gwei: 50,
            current_l2_base_fee_gwei: 1,
            l1_adaptive_buffer: 1.30,
            oracle: OracleSnapshot {
                chainlink_price: 2000.0,
                pyth_price: 2001.0,
                twap_price: 1999.0,
                chainlink_age_s: 10,
                pyth_age_s: 10,
                twap_age_s: 60,
            },
            flashloan: FlashloanSnapshot {
                available: 2_000_000,
                protocol_id: String::from("balancer"),
            },
            competition_probability: 0.50,
            max_competition_probability: 0.90,
            strategy_max_gas: 500_000,
            max_slippage_bps: 30,
            rollout_tier: 1.0,
            strategy_bytecode_hash,
            risk_score: 0.30,
            max_risk_score: 0.80,
            current_account_exposure_wei: 0,
            max_account_exposure_wei: 10_000_000_000_000_000_000,
            latest_blueprint_nonce: 0,
        }
    }

    fn sample_bp(hash_byte: u8) -> ExecutionBlueprint {
        let signal_id = Uuid::from_bytes([hash_byte; 16]);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(StrategyId::Sa, 42161, 1, signal_id);
        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO,
            chain_id: 42161,
            strategy_id: StrategyId::Sa,
            lane: omega_core::types::lane::Lane::Microtx,
            simulator: Simulator::Revm,
            signal_state_hash: B256::ZERO,
            state_version: 1,
            signal_id,
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::from(1_000_000u64),
            flashloan_available: U256::from(2_000_000u64),
            calldata: Bytes::new(),
            strategy_bytecode_hash: B256::from([0xaa; 32]),
            l2_exec_gas_estimate: 100_000,
            l1_data_gas_estimate: 5_000,
            extraction_gas: 45_000,
            expected_profit_net: U256::from(1_000_000_000_000_000_000u128),
            dynamic_min_profit: U256::from(100_000_000_000_000_000u128),
            l2_buffer_factor: 1.15,
            l1_data_buffer_factor: 1.10,
            slippage_bps: 20,
            base_fee_at_creation: 50,
            l1_data_fee_at_creation: 40,
            priority_fee_gwei: 10,
            price_impact_bps: None,
            ofa_compliant: false,
            expiry_block: 1_000,
            nonce: 1,
            confirmation_depth: 12,
            client_order_id,
            idempotency_key: B256::ZERO,
            relay_targets: vec!["flashbots".to_string()],
            zk_proof_commitment: None,
        };
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();
        bp
    }

    /// Registers "SA" with the exact bytecode hash `sample_bp`/
    /// `sample_bp_wide` use ([0xaa; 32]) — every existing test blueprint
    /// is `StrategyId::Sa`, so this must match or every test would now
    /// fail Stage 2b (Gap 9's bytecode integrity check) regardless of
    /// what it's actually testing.
    fn make_integrity_registry() -> Arc<omega_security::IntegrityRegistry> {
        let reg = omega_security::IntegrityRegistry::new();
        reg.register(omega_security::StrategyEntry {
            strategy_id: "SA".into(),
            bytecode_hash: [0xaa; 32],
            contract_address: [0x01; 20],
            min_phase: 1,
        });
        reg
    }

    fn make_pipeline() -> ExecutionPipeline<MockTransactionSigner> {
        ExecutionPipeline::new(
            make_kill_switches(),
            make_integrity_registry(),
            make_relay(),
            make_dag(),
            Arc::new(MockTransactionSigner { should_fail: false }),
            42161,
        )
    }

    /// Generalized pipeline builder for property/load tests that need to
    /// hold their own handles to the DAG/relay/kill-switches (to admit
    /// blueprints, trip the switch, or inspect state independently of the
    /// pipeline) or need a non-default signer (artificial latency).
    fn make_pipeline_with<S: TransactionSigner>(
        dag: Arc<Mutex<ExecutionDag>>,
        relay: Arc<MultiRelayClient>,
        kill_switches: Arc<KillSwitchRegistry>,
        signer: Arc<S>,
    ) -> ExecutionPipeline<S> {
        ExecutionPipeline::new(
            kill_switches,
            make_integrity_registry(),
            relay,
            dag,
            signer,
            42161,
        )
    }

    // ── Stage 0 ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn phase_0_suppresses_submission() {
        let pipeline = make_pipeline();
        let mut dag_guard = pipeline.dag.lock().unwrap();
        dag_guard.admit(sample_bp(1), &[]).unwrap();
        drop(dag_guard);

        let bp = sample_bp(1);
        let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
        let result = pipeline.execute(bp, 0, &ctx, 500, 1_700_000_000).await;
        assert!(matches!(result, Ok(ExecutionOutcome::Suppressed)));
    }

    // ── Stage 1 ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn tampered_blueprint_fails_integrity_check() {
        let pipeline = make_pipeline();
        let mut bp = sample_bp(2);
        pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
        bp.expected_profit_net = U256::from(999u64); // desyncs blueprint_hash

        let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
        let result = pipeline.execute(bp, 1, &ctx, 500, 1_700_000_000).await;
        assert!(matches!(result, Err(ExecutionError::IntegrityFailure)));
    }

    // ── Stage 2a ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn tripped_kill_switch_blocks_submission() {
        let pipeline = make_pipeline();
        let bp = sample_bp(3);
        pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
        pipeline
            .kill_switches
            .trip_manual("SA", "alice", "test trip");

        let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
        let result = pipeline.execute(bp, 1, &ctx, 500, 1_700_000_000).await;
        assert!(matches!(
            result,
            Err(ExecutionError::KillSwitchTripped { .. })
        ));
    }

    // ── Stage 2b (bytecode integrity registry — Gap 9) ──────────────────

    #[tokio::test]
    async fn integrity_registry_pass_allows_submission() {
        // Explicit, dedicated pass-case for Stage 2b — the happy-path
        // tests elsewhere in this file exercise it implicitly (via
        // make_integrity_registry()'s SA/[0xaa;32] fixture matching every
        // sample blueprint), but this test names the property directly:
        // a blueprint whose strategy is registered with the matching
        // hash, unfrozen, must pass this stage.
        let pipeline = make_pipeline();
        let bp = sample_bp(60);
        pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
        let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
        let result = pipeline.execute(bp, 1, &ctx, 500, 1_700_000_000).await;
        assert!(
            result.is_ok(),
            "registered strategy with matching hash must pass Stage 2b"
        );
    }

    #[tokio::test]
    async fn integrity_registry_unregistered_strategy_fails() {
        let dag = make_dag_with_capacity(16);
        let kill_switches = make_kill_switches();
        // Empty registry — SA is never registered.
        let integrity_registry = omega_security::IntegrityRegistry::new();
        let relay = make_relay();
        let pipeline = ExecutionPipeline::new(
            kill_switches,
            integrity_registry,
            relay,
            Arc::clone(&dag),
            Arc::new(MockTransactionSigner { should_fail: false }),
            42161,
        );

        let bp = sample_bp(61);
        dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
        let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
        let result = pipeline.execute(bp, 1, &ctx, 500, 1_700_000_000).await;
        assert!(matches!(
            result,
            Err(ExecutionError::IntegrityRegistryCheckFailed(
                omega_security::SecurityError::StrategyUnknown { .. }
            ))
        ));
    }

    #[tokio::test]
    async fn integrity_registry_wrong_hash_fails() {
        let dag = make_dag_with_capacity(16);
        let kill_switches = make_kill_switches();
        let integrity_registry = omega_security::IntegrityRegistry::new();
        integrity_registry.register(omega_security::StrategyEntry {
            strategy_id: "SA".into(),
            bytecode_hash: [0xbb; 32], // deliberately does NOT match sample_bp's [0xaa; 32]
            contract_address: [0x01; 20],
            min_phase: 1,
        });
        let relay = make_relay();
        let pipeline = ExecutionPipeline::new(
            kill_switches,
            integrity_registry,
            relay,
            Arc::clone(&dag),
            Arc::new(MockTransactionSigner { should_fail: false }),
            42161,
        );

        let bp = sample_bp(62);
        dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
        let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
        let result = pipeline.execute(bp, 1, &ctx, 500, 1_700_000_000).await;
        assert!(matches!(
            result,
            Err(ExecutionError::IntegrityRegistryCheckFailed(
                omega_security::SecurityError::BytecodeMismatch { .. }
            ))
        ));
    }

    #[tokio::test]
    async fn integrity_registry_frozen_strategy_fails() {
        let dag = make_dag_with_capacity(16);
        let kill_switches = make_kill_switches();
        let integrity_registry = make_integrity_registry();
        integrity_registry.freeze("SA");
        let relay = make_relay();
        let pipeline = ExecutionPipeline::new(
            kill_switches,
            integrity_registry,
            relay,
            Arc::clone(&dag),
            Arc::new(MockTransactionSigner { should_fail: false }),
            42161,
        );

        let bp = sample_bp(63);
        dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
        let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
        let result = pipeline.execute(bp, 1, &ctx, 500, 1_700_000_000).await;
        assert!(matches!(
            result,
            Err(ExecutionError::IntegrityRegistryCheckFailed(
                omega_security::SecurityError::StrategyFrozen { .. }
            ))
        ));
    }

    #[tokio::test]
    async fn integrity_registry_reregistering_same_strategy_id_is_not_an_error() {
        // "Duplicate strategy IDs" — DashMap::insert on re-registration
        // is last-write-wins, not an error. Confirms the pipeline doesn't
        // need any special handling for this and that the newer entry
        // takes effect.
        let registry = omega_security::IntegrityRegistry::new();
        registry.register(omega_security::StrategyEntry {
            strategy_id: "SA".into(),
            bytecode_hash: [0x11; 32],
            contract_address: [0x01; 20],
            min_phase: 1,
        });
        registry.register(omega_security::StrategyEntry {
            strategy_id: "SA".into(),
            bytecode_hash: [0xaa; 32], // overwrites the entry above
            contract_address: [0x01; 20],
            min_phase: 1,
        });
        assert!(registry.check_bytecode("SA", &[0xaa; 32]).is_ok());
        assert!(registry.check_bytecode("SA", &[0x11; 32]).is_err());
    }

    #[tokio::test]
    async fn integrity_registry_freeze_mid_flight_never_lets_a_post_freeze_call_through() {
        // Concurrency property for Stage 2b: IntegrityRegistry uses
        // DashMap/DashSet internally (confirmed in omega-security's own
        // source — no external synchronization needed), so `freeze()`
        // (&self, not &mut self) can genuinely run concurrently with
        // in-flight `execute()` calls, the same "point-in-time snapshot
        // check" semantics as the kill switch. This test proves the
        // specific safety property that matters: once `freeze()` has
        // returned, EVERY execute() call started strictly after that
        // point must fail — never silently pass a frozen strategy.
        let dag = make_dag_with_capacity(16);
        let kill_switches = make_kill_switches();
        let integrity_registry = make_integrity_registry();
        let relay = make_relay();
        let pipeline = Arc::new(ExecutionPipeline::new(
            kill_switches,
            Arc::clone(&integrity_registry),
            relay,
            Arc::clone(&dag),
            Arc::new(MockTransactionSigner { should_fail: false }),
            42161,
        ));

        // Freeze completes fully before any execute() call starts —
        // this is the deterministic half of the property (see this
        // file's existing "trip-then-burst" kill-switch test for the
        // same reasoning about why a genuinely straddling race isn't
        // meaningfully assertable).
        integrity_registry.freeze("SA");

        let mut handles = Vec::new();
        for i in 64u8..70u8 {
            let bp = sample_bp(i);
            dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
            let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
            let p = Arc::clone(&pipeline);
            handles.push(tokio::spawn(async move {
                p.execute(bp, 1, &ctx, 500, 1_700_000_000).await
            }));
        }
        for h in handles {
            assert!(matches!(
                h.await.unwrap(),
                Err(ExecutionError::IntegrityRegistryCheckFailed(
                    omega_security::SecurityError::StrategyFrozen { .. }
                ))
            ));
        }
    }

    // ── Stage 2c (15 pre-trade checks) ───────────────────────────────────

    #[tokio::test]
    async fn failing_pretrade_check_blocks_submission() {
        let pipeline = make_pipeline();
        let bp = sample_bp(4);
        pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();

        let mut ctx = passing_ctx(bp.strategy_bytecode_hash.0);
        ctx.current_block = 9999; // > expiry_block (1000) -> MissExpiry
        let result = pipeline.execute(bp, 1, &ctx, 500, 1_700_000_000).await;
        assert!(matches!(result, Err(ExecutionError::RiskCheckFailed(_))));
    }

    // ── expected_profit_net overflow (this revision) ─────────────────────

    /// Regression guard for this revision's fix: an `expected_profit_net`
    /// that doesn't fit in u128 must be REJECTED at the mapping step, not
    /// silently coerced to u128::MAX (which would fail open in
    /// check_dynamic_profit — see this file's and error.rs's module-level
    /// notes for the confirmed reasoning).
    #[test]
    fn expected_profit_overflow_fails_closed_at_mapping() {
        let mut bp = sample_bp(0xEE);
        // U256 wider than u128::MAX — guaranteed overflow on try_into::<u128>().
        bp.expected_profit_net = U256::from(u128::MAX) + U256::from(1u64);
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();

        let err = blueprint_to_check_fields(&bp).unwrap_err();
        assert!(matches!(
            err,
            ExecutionError::BlueprintFieldOverflow {
                field: "expected_profit_net"
            }
        ));
    }

    /// Confirms the OTHER two U256->u128 mappings still saturate to MAX
    /// on overflow rather than erroring — the deliberately different
    /// treatment this revision's doc comments describe, since their
    /// overflow direction fails closed in checks 10/14 rather than open.
    #[test]
    fn dynamic_min_profit_and_flashloan_amount_still_saturate_on_overflow() {
        let mut bp = sample_bp(0xEF);
        bp.dynamic_min_profit = U256::from(u128::MAX) + U256::from(1u64);
        bp.flashloan_amount = U256::from(u128::MAX) + U256::from(1u64);
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();

        let fields = blueprint_to_check_fields(&bp)
            .expect("overflow on these two fields must NOT error — they saturate instead");
        assert_eq!(fields.dynamic_min_profit_wei, u128::MAX);
        assert_eq!(fields.flashloan_amount, u128::MAX);
    }

    // ── Stage 3 ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn duplicate_idempotency_key_second_call_blocked() {
        let pipeline = make_pipeline();
        let bp = sample_bp(5);
        {
            let mut g = pipeline.dag.lock().unwrap();
            g.admit(bp.clone(), &[]).unwrap();
        }
        let ctx = passing_ctx(bp.strategy_bytecode_hash.0);

        let first = pipeline
            .execute(bp.clone(), 1, &ctx, 500, 1_700_000_000)
            .await;
        assert!(first.is_ok());

        // Second call with the identical blueprint (identical idempotency_key)
        pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).ok(); // may be a dup DAG admit; irrelevant to this test
        let second = pipeline.execute(bp, 1, &ctx, 500, 1_700_000_000).await;
        assert!(matches!(
            second,
            Err(ExecutionError::DuplicateIdempotencyKey)
        ));
    }

    #[tokio::test]
    async fn concurrent_duplicate_blueprint_exactly_one_wins() {
        // Pipeline-level version of Q3 ("can a duplicate idempotency key
        // race through under concurrency?") — the lower-level
        // IdempotencyCache test already proves the cache primitive is
        // race-free; this proves the whole execute() call is too, under
        // genuine concurrent invocation from two tasks racing on the
        // IDENTICAL blueprint, not just the cache in isolation.
        let pipeline = Arc::new(make_pipeline());
        let bp = sample_bp(9);
        pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
        // ExecutionDag::complete() on Stage 6 tolerates being called for
        // an already-removed hash (logs a warning, returns an empty Vec —
        // see scheduler.rs), so a single real admit() above is sufficient
        // for both racing execute() calls; no second admit needed or
        // attempted here.

        let ctx = Arc::new(passing_ctx(bp.strategy_bytecode_hash.0));

        let p1 = Arc::clone(&pipeline);
        let bp1 = bp.clone();
        let ctx1 = Arc::clone(&ctx);
        let p2 = Arc::clone(&pipeline);
        let bp2 = bp.clone();
        let ctx2 = Arc::clone(&ctx);

        let (r1, r2) = tokio::join!(
            tokio::spawn(async move { p1.execute(bp1, 1, &ctx1, 500, 1_700_000_000).await }),
            tokio::spawn(async move { p2.execute(bp2, 1, &ctx2, 500, 1_700_000_000).await }),
        );
        let r1 = r1.unwrap();
        let r2 = r2.unwrap();

        let ok_count = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
        let dup_count = [&r1, &r2]
            .iter()
            .filter(|r| matches!(r, Err(ExecutionError::DuplicateIdempotencyKey)))
            .count();
        assert_eq!(ok_count, 1, "exactly one concurrent call must succeed");
        assert_eq!(
            dup_count, 1,
            "the other must fail as a duplicate, not silently succeed or panic"
        );
    }

    #[tokio::test]
    async fn concurrent_distinct_blueprints_do_not_serialize() {
        // Throughput/no-bottleneck guarantee: N distinct blueprints
        // executed concurrently must all succeed independently — proves
        // no coarse lock (DAG or otherwise) forces cross-blueprint
        // serialization. Uses 8 concurrent blueprints as a representative
        // burst, not a formal load test.
        let pipeline = Arc::new(make_pipeline());
        let mut handles = Vec::new();

        for i in 20u8..28u8 {
            let bp = sample_bp(i);
            pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
            let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
            let p = Arc::clone(&pipeline);
            handles.push(tokio::spawn(async move {
                p.execute(bp, 1, &ctx, 500, 1_700_000_000).await
            }));
        }

        let mut ok_count = 0;
        for h in handles {
            if h.await.unwrap().is_ok() {
                ok_count += 1;
            }
        }
        assert_eq!(
            ok_count, 8,
            "all 8 distinct blueprints must succeed independently"
        );
        assert_eq!(
            pipeline.dag.lock().unwrap().microtx_count(),
            0,
            "every slot must be freed — no leaked slots under concurrent load"
        );
    }

    // ── Stage 4/5 happy path ───────────────────────────────────────────

    #[tokio::test]
    async fn microtx_lane_submits_single() {
        let pipeline = make_pipeline();
        let bp = sample_bp(6);
        pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
        let ctx = passing_ctx(bp.strategy_bytecode_hash.0);

        let result = pipeline.execute(bp, 1, &ctx, 500, 1_700_000_000).await;
        assert!(matches!(
            result,
            Ok(ExecutionOutcome::SubmittedSingle { any_accepted: true })
        ));
    }

    #[tokio::test]
    async fn normal_lane_submits_cascade() {
        let pipeline = make_pipeline();
        let mut bp = sample_bp(7);
        bp.lane = omega_core::types::lane::Lane::Normal;
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();
        pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
        let ctx = passing_ctx(bp.strategy_bytecode_hash.0);

        let result = pipeline.execute(bp, 1, &ctx, 500, 1_700_000_000).await;
        assert!(matches!(
            result,
            Ok(ExecutionOutcome::SubmittedCascade {
                any_accepted: true,
                ..
            })
        ));
    }

    // ── Stage 6 ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn dag_slot_freed_on_every_exit_path() {
        let pipeline = make_pipeline();
        let bp = sample_bp(8);
        pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
        assert_eq!(pipeline.dag.lock().unwrap().microtx_count(), 1);

        let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
        let _ = pipeline
            .execute(bp.clone(), 1, &ctx, 500, 1_700_000_000)
            .await;

        assert_eq!(
            pipeline.dag.lock().unwrap().microtx_count(),
            0,
            "DAG slot must be freed even though the blueprint was never re-admitted"
        );
    }

    #[tokio::test]
    async fn dag_slot_freed_even_when_stage1_fails_before_stage2() {
        // Regression guard specifically for the panic-safety fix: proves
        // the guard-based release fires on an EARLY exit path (Stage 1),
        // not just the happy path exercised by the test above.
        let pipeline = make_pipeline();
        let mut bp = sample_bp(10);
        pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
        bp.expected_profit_net = U256::from(1u64); // desyncs blueprint_hash -> Stage 1 fails

        let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
        let result = pipeline.execute(bp, 1, &ctx, 500, 1_700_000_000).await;
        assert!(matches!(result, Err(ExecutionError::IntegrityFailure)));
        assert_eq!(
            pipeline.dag.lock().unwrap().microtx_count(),
            0,
            "DAG slot must be freed even on the earliest possible failure path"
        );
    }

    // ── Idempotency cache eviction (Q9) ───────────────────────────────

    #[tokio::test]
    async fn idempotency_cache_eviction_is_reachable_from_the_pipeline() {
        let pipeline = make_pipeline();
        let bp = sample_bp(11);
        pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
        let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
        pipeline
            .execute(bp, 1, &ctx, 500, 1_700_000_000)
            .await
            .unwrap();

        assert_eq!(pipeline.idempotency_cache_len(), 1);
        let far_future = chrono::Utc::now() + chrono::Duration::days(365);
        pipeline.evict_idempotency_cache(chrono::Duration::seconds(60), far_future);
        assert_eq!(
            pipeline.idempotency_cache_len(),
            0,
            "eviction must be reachable and effective from outside the pipeline"
        );
    }

    // ── Test doubles for property/load tests ────────────────────────────

    /// Counts how many times `submit_bundle` was actually invoked — used
    /// to prove a blocked pipeline stage (e.g. a tripped kill switch)
    /// never reaches the relay at all, not just that the pipeline
    /// *returns* an error.
    struct CountingRelay {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl CountingRelay {
        fn new() -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn call_count(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl RelayClient for CountingRelay {
        async fn submit_bundle(
            &self,
            bundle: omega_relay::BundlePayload,
        ) -> omega_relay::RelayResult<SubmissionOutcome> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(SubmissionOutcome {
                accepted: true,
                relay_bundle_id: Some(bundle.bundle_hash),
            })
        }
        fn name(&self) -> &str {
            "counting"
        }
    }

    /// Sleeps for `delay` before accepting, to simulate real relay/network
    /// latency — used to prove concurrent `execute()` calls don't
    /// accidentally serialize against each other.
    struct SlowRelay {
        delay: std::time::Duration,
    }

    #[async_trait::async_trait]
    impl RelayClient for SlowRelay {
        async fn submit_bundle(
            &self,
            bundle: omega_relay::BundlePayload,
        ) -> omega_relay::RelayResult<SubmissionOutcome> {
            tokio::time::sleep(self.delay).await;
            Ok(SubmissionOutcome {
                accepted: true,
                relay_bundle_id: Some(bundle.bundle_hash),
            })
        }
        fn name(&self) -> &str {
            "slow"
        }
    }

    /// Sleeps for `delay` before signing, to simulate real HSM/KMS
    /// signing latency — same purpose as `SlowRelay`, for Stage 4 instead
    /// of Stage 5.
    struct SlowSigner {
        delay: std::time::Duration,
    }

    #[async_trait::async_trait]
    impl TransactionSigner for SlowSigner {
        async fn sign_transaction(
            &self,
            bp: &ExecutionBlueprint,
            _chain_id: u64,
        ) -> Result<crate::signer::SignedTransaction, ExecutionError> {
            tokio::time::sleep(self.delay).await;
            Ok(crate::signer::SignedTransaction {
                raw_tx_hex: format!("0x{}", hex::encode(bp.blueprint_hash.as_slice())),
            })
        }
    }

    // ── Property tests ───────────────────────────────────────────────────
    //
    // Distinct from the scenario-based tests above: each of these asserts
    // an invariant that must hold across *any* run, not just one specific
    // input/output pair.

    #[tokio::test]
    async fn property_dag_occupancy_never_goes_negative_on_over_release() {
        // Simulates a hypothetical double-release bug: complete() called
        // more times than admit(). ExecutionDag's own saturating_sub
        // already guarantees this can't underflow-panic; this test proves
        // that guarantee holds when driven through this crate's actual
        // usage pattern, not just in omega-dag's own unit tests.
        let dag = make_dag_with_capacity(16);
        let bp = sample_bp(30);
        dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
        assert_eq!(dag.lock().unwrap().microtx_count(), 1);

        // Release it, then release again — the second call hits
        // ExecutionDag::complete's "unknown blueprint" path.
        dag.lock().unwrap().complete(bp.blueprint_hash);
        dag.lock().unwrap().complete(bp.blueprint_hash);
        dag.lock().unwrap().complete(bp.blueprint_hash);

        assert_eq!(
            dag.lock().unwrap().microtx_count(),
            0,
            "occupancy must never go negative regardless of over-release"
        );
    }

    #[tokio::test]
    async fn property_every_admitted_blueprint_released_exactly_once_mixed_outcomes() {
        // Runs a mix of blueprints that succeed, fail the kill switch,
        // and fail integrity — asserts every admitted slot is freed
        // exactly once regardless of WHICH stage rejected it.
        let dag = make_dag_with_capacity(16);
        let kill_switches = make_kill_switches();
        let relay = make_relay();
        let pipeline = make_pipeline_with(
            Arc::clone(&dag),
            relay,
            Arc::clone(&kill_switches),
            Arc::new(MockTransactionSigner { should_fail: false }),
        );

        // 1. Succeeds.
        let bp_ok = sample_bp(31);
        dag.lock().unwrap().admit(bp_ok.clone(), &[]).unwrap();
        let ctx_ok = passing_ctx(bp_ok.strategy_bytecode_hash.0);
        assert!(pipeline
            .execute(bp_ok, 1, &ctx_ok, 500, 1_700_000_000)
            .await
            .is_ok());

        // 2. Fails integrity (tampered after admit).
        let mut bp_bad_hash = sample_bp(32);
        dag.lock().unwrap().admit(bp_bad_hash.clone(), &[]).unwrap();
        let ctx_bad_hash = passing_ctx(bp_bad_hash.strategy_bytecode_hash.0);
        bp_bad_hash.expected_profit_net = U256::from(1u64);
        assert!(matches!(
            pipeline
                .execute(bp_bad_hash, 1, &ctx_bad_hash, 500, 1_700_000_000)
                .await,
            Err(ExecutionError::IntegrityFailure)
        ));

        // 3. Fails kill switch.
        kill_switches.trip_manual("SA", "alice", "mixed-outcome test");
        let bp_ks = sample_bp(33);
        dag.lock().unwrap().admit(bp_ks.clone(), &[]).unwrap();
        let ctx_ks = passing_ctx(bp_ks.strategy_bytecode_hash.0);
        assert!(matches!(
            pipeline
                .execute(bp_ks, 1, &ctx_ks, 500, 1_700_000_000)
                .await,
            Err(ExecutionError::KillSwitchTripped { .. })
        ));

        assert_eq!(
            dag.lock().unwrap().microtx_count(),
            0,
            "all three admitted slots must be freed regardless of which stage rejected each one"
        );
    }

    #[tokio::test]
    async fn property_successful_submission_yields_exactly_one_idempotency_entry() {
        let pipeline = make_pipeline();
        let bp = sample_bp(34);
        pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
        let ctx = passing_ctx(bp.strategy_bytecode_hash.0);

        assert_eq!(pipeline.idempotency_cache_len(), 0);
        pipeline
            .execute(bp, 1, &ctx, 500, 1_700_000_000)
            .await
            .unwrap();
        assert_eq!(
            pipeline.idempotency_cache_len(),
            1,
            "exactly one idempotency entry must exist per successful submission"
        );
    }

    #[tokio::test]
    async fn property_kill_switch_trip_prevents_all_relay_calls() {
        // Proves the relay is never actually reached when the kill switch
        // is tripped — not just that execute() returns an error, but that
        // Stage 5 provably never ran.
        let dag = make_dag_with_capacity(16);
        let kill_switches = make_kill_switches();
        let counting_relay = Arc::new(CountingRelay::new());
        let relay = make_relay_with(Arc::clone(&counting_relay) as Arc<dyn RelayClient>, 1000);
        let pipeline = make_pipeline_with(
            Arc::clone(&dag),
            relay,
            Arc::clone(&kill_switches),
            Arc::new(MockTransactionSigner { should_fail: false }),
        );

        kill_switches.trip_manual("SA", "alice", "block all relay calls");

        for i in 40u8..45u8 {
            let bp = sample_bp(i);
            dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
            let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
            let result = pipeline.execute(bp, 1, &ctx, 500, 1_700_000_000).await;
            assert!(matches!(
                result,
                Err(ExecutionError::KillSwitchTripped { .. })
            ));
        }

        assert_eq!(
            counting_relay.call_count(),
            0,
            "relay must never be called for any blueprint once the kill switch is tripped"
        );
    }

    #[tokio::test]
    async fn property_no_duplicate_submission_under_wide_concurrent_interleaving() {
        // Strengthens the 2-way race already tested elsewhere to a 16-way
        // race on the identical blueprint — exactly one winner regardless
        // of how many tasks interleave.
        let pipeline = Arc::new(make_pipeline());
        let bp = sample_bp(50);
        pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
        let ctx = Arc::new(passing_ctx(bp.strategy_bytecode_hash.0));

        let mut handles = Vec::new();
        for _ in 0..16 {
            let p = Arc::clone(&pipeline);
            let bp_clone = bp.clone();
            let ctx_clone = Arc::clone(&ctx);
            handles.push(tokio::spawn(async move {
                p.execute(bp_clone, 1, &ctx_clone, 500, 1_700_000_000).await
            }));
        }

        let mut ok_count = 0;
        let mut dup_count = 0;
        for h in handles {
            match h.await.unwrap() {
                Ok(_) => ok_count += 1,
                Err(ExecutionError::DuplicateIdempotencyKey) => dup_count += 1,
                Err(other) => panic!("unexpected error in 16-way race: {other:?}"),
            }
        }
        assert_eq!(ok_count, 1, "exactly one of 16 racing calls must succeed");
        assert_eq!(
            dup_count, 15,
            "the other 15 must all fail as duplicates, none silently lost or double-counted"
        );
    }

    // ── Load tests ────────────────────────────────────────────────────
    //
    // These prove CORRECTNESS under concurrency and ABSENCE of accidental
    // serialization (via artificial latency + wall-clock bounds). They do
    // NOT produce a real throughput/TPS number — AlwaysAcceptRelay and
    // MockTransactionSigner have no real network/HSM latency, so any
    // "requests per second" figure derived from them would not reflect
    // production capacity. That number can only come from testing
    // against real infrastructure, which this sandbox does not have.

    #[tokio::test]
    async fn load_100_concurrent_distinct_blueprints_all_succeed() {
        let pipeline = Arc::new(ExecutionPipeline::new(
            make_kill_switches(),
            make_integrity_registry(),
            make_relay_with(Arc::new(AlwaysAcceptRelay), 10_000),
            make_dag_with_capacity(200),
            Arc::new(MockTransactionSigner { should_fail: false }),
            42161,
        ));

        let mut handles = Vec::new();
        for i in 0u16..100u16 {
            let bp = sample_bp_wide(i + 1000);
            pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
            let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
            let p = Arc::clone(&pipeline);
            handles.push(tokio::spawn(async move {
                p.execute(bp, 1, &ctx, 500, 1_700_000_000).await
            }));
        }

        let mut ok = 0;
        for h in handles {
            if h.await.unwrap().is_ok() {
                ok += 1;
            }
        }
        assert_eq!(ok, 100);
        assert_eq!(pipeline.dag.lock().unwrap().microtx_count(), 0);
    }

    #[tokio::test]
    async fn load_1000_concurrent_distinct_blueprints_all_succeed() {
        let pipeline = Arc::new(ExecutionPipeline::new(
            make_kill_switches(),
            make_integrity_registry(),
            make_relay_with(Arc::new(AlwaysAcceptRelay), 100_000),
            make_dag_with_capacity(1_500),
            Arc::new(MockTransactionSigner { should_fail: false }),
            42161,
        ));

        let mut handles = Vec::new();
        for i in 0u16..1000u16 {
            let bp = sample_bp_wide(i + 2000);
            pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
            let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
            let p = Arc::clone(&pipeline);
            handles.push(tokio::spawn(async move {
                p.execute(bp, 1, &ctx, 500, 1_700_000_000).await
            }));
        }

        let mut ok = 0;
        for h in handles {
            if h.await.unwrap().is_ok() {
                ok += 1;
            }
        }
        assert_eq!(
            ok, 1000,
            "all 1000 concurrent distinct blueprints must succeed independently"
        );
        assert_eq!(
            pipeline.dag.lock().unwrap().microtx_count(),
            0,
            "no leaked slots at 1000-way concurrency"
        );
    }

    #[tokio::test]
    async fn load_mixed_success_and_rejection_under_concurrency() {
        // Half the blueprints are valid, half are deliberately tampered
        // (fail Stage 1) — run concurrently, assert exact counts and that
        // rejections never contaminate the success count or vice versa.
        let pipeline = Arc::new(ExecutionPipeline::new(
            make_kill_switches(),
            make_integrity_registry(),
            make_relay_with(Arc::new(AlwaysAcceptRelay), 10_000),
            make_dag_with_capacity(200),
            Arc::new(MockTransactionSigner { should_fail: false }),
            42161,
        ));

        let mut handles = Vec::new();
        for i in 0u16..100u16 {
            let mut bp = sample_bp_wide(i + 3000);
            pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
            let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
            if i % 2 == 1 {
                bp.expected_profit_net = U256::from(1u64); // desyncs hash -> Stage 1 failure
            }
            let p = Arc::clone(&pipeline);
            handles.push(tokio::spawn(async move {
                p.execute(bp, 1, &ctx, 500, 1_700_000_000).await
            }));
        }

        let mut ok = 0;
        let mut integrity_failures = 0;
        for h in handles {
            match h.await.unwrap() {
                Ok(_) => ok += 1,
                Err(ExecutionError::IntegrityFailure) => integrity_failures += 1,
                Err(other) => panic!("unexpected error: {other:?}"),
            }
        }
        assert_eq!(ok, 50, "exactly the untampered half must succeed");
        assert_eq!(
            integrity_failures, 50,
            "exactly the tampered half must fail integrity, nothing else"
        );
        assert_eq!(pipeline.dag.lock().unwrap().microtx_count(), 0);
    }

    #[tokio::test]
    async fn load_concurrent_with_artificial_relay_latency_does_not_serialize() {
        const N: usize = 20;
        const PER_CALL_DELAY_MS: u64 = 50;

        let pipeline = Arc::new(ExecutionPipeline::new(
            make_kill_switches(),
            make_integrity_registry(),
            make_relay_with(
                Arc::new(SlowRelay {
                    delay: std::time::Duration::from_millis(PER_CALL_DELAY_MS),
                }),
                10_000, // high rate limit so governor doesn't confound the timing measurement
            ),
            make_dag_with_capacity(50),
            Arc::new(MockTransactionSigner { should_fail: false }),
            42161,
        ));

        let mut handles = Vec::new();
        for i in 0u16..N as u16 {
            let bp = sample_bp_wide(i + 4000);
            pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
            let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
            let p = Arc::clone(&pipeline);
            handles.push(tokio::spawn(async move {
                p.execute(bp, 1, &ctx, 500, 1_700_000_000).await
            }));
        }

        let start = std::time::Instant::now();
        for h in handles {
            h.await.unwrap().unwrap();
        }
        let elapsed = start.elapsed();

        // Fully sequential would take N * PER_CALL_DELAY_MS = 1000ms.
        // True concurrency should land close to one call's latency
        // (~50-150ms with scheduling overhead). Asserting well under the
        // sequential bound (not right up against the concurrent bound)
        // to avoid a flaky test on a loaded CI box, while still failing
        // decisively if a real accidental bottleneck were introduced.
        assert!(
            elapsed < std::time::Duration::from_millis(N as u64 * PER_CALL_DELAY_MS / 2),
            "elapsed {elapsed:?} suggests calls serialized instead of running concurrently \
             (fully sequential would be ~{}ms)",
            N as u64 * PER_CALL_DELAY_MS
        );
    }

    #[tokio::test]
    async fn load_concurrent_with_artificial_signer_latency_does_not_serialize() {
        const N: usize = 20;
        const PER_CALL_DELAY_MS: u64 = 50;

        let pipeline = Arc::new(ExecutionPipeline::new(
            make_kill_switches(),
            make_integrity_registry(),
            make_relay_with(Arc::new(AlwaysAcceptRelay), 10_000),
            make_dag_with_capacity(50),
            Arc::new(SlowSigner {
                delay: std::time::Duration::from_millis(PER_CALL_DELAY_MS),
            }),
            42161,
        ));

        let mut handles = Vec::new();
        for i in 0u16..N as u16 {
            let bp = sample_bp_wide(i + 5000);
            pipeline.dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
            let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
            let p = Arc::clone(&pipeline);
            handles.push(tokio::spawn(async move {
                p.execute(bp, 1, &ctx, 500, 1_700_000_000).await
            }));
        }

        let start = std::time::Instant::now();
        for h in handles {
            h.await.unwrap().unwrap();
        }
        let elapsed = start.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(N as u64 * PER_CALL_DELAY_MS / 2),
            "elapsed {elapsed:?} suggests signing calls serialized instead of running \
             concurrently (fully sequential would be ~{}ms)",
            N as u64 * PER_CALL_DELAY_MS
        );
    }

    #[tokio::test]
    async fn load_kill_switch_trip_during_burst_blocks_subsequent_calls() {
        // "During heavy load" interpreted deterministically: trip the
        // switch, THEN fire a concurrent burst — every call in the burst
        // must be blocked, since each call's guard() check happens
        // strictly after the trip completed. (A test asserting a specific
        // split of successes-before-trip vs failures-after-trip would be
        // inherently racy and not a meaningful deterministic assertion —
        // this test only claims the property that IS deterministic: no
        // successful submission can cross a completed trip.)
        let dag = make_dag_with_capacity(50);
        let kill_switches = make_kill_switches();
        let counting_relay = Arc::new(CountingRelay::new());
        let relay = make_relay_with(Arc::clone(&counting_relay) as Arc<dyn RelayClient>, 10_000);
        let pipeline = Arc::new(make_pipeline_with(
            Arc::clone(&dag),
            relay,
            Arc::clone(&kill_switches),
            Arc::new(MockTransactionSigner { should_fail: false }),
        ));

        kill_switches.trip_manual("SA", "alice", "trip before burst");

        let mut handles = Vec::new();
        for i in 0u16..30u16 {
            let bp = sample_bp_wide(i + 6000);
            dag.lock().unwrap().admit(bp.clone(), &[]).unwrap();
            let ctx = passing_ctx(bp.strategy_bytecode_hash.0);
            let p = Arc::clone(&pipeline);
            handles.push(tokio::spawn(async move {
                p.execute(bp, 1, &ctx, 500, 1_700_000_000).await
            }));
        }

        for h in handles {
            assert!(matches!(
                h.await.unwrap(),
                Err(ExecutionError::KillSwitchTripped { .. })
            ));
        }

        assert_eq!(
            counting_relay.call_count(),
            0,
            "no call in the post-trip burst may reach the relay"
        );
    }

    /// Like `sample_bp`, but accepts a wider index so load tests can
    /// generate hundreds/thousands of blueprints with distinct
    /// `signal_id`s (and therefore distinct `idempotency_key`s) without
    /// colliding — `sample_bp`'s `u8` parameter wraps at 256, which is
    /// too narrow for the 1000-blueprint load test.
    fn sample_bp_wide(index: u16) -> ExecutionBlueprint {
        let bytes = index.to_be_bytes();
        let mut hash_seed = [0u8; 16];
        hash_seed[14] = bytes[0];
        hash_seed[15] = bytes[1];
        let signal_id = Uuid::from_bytes(hash_seed);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(StrategyId::Sa, 42161, 1, signal_id);
        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO,
            chain_id: 42161,
            strategy_id: StrategyId::Sa,
            lane: omega_core::types::lane::Lane::Microtx,
            simulator: Simulator::Revm,
            signal_state_hash: B256::ZERO,
            state_version: 1,
            signal_id,
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::from(1_000_000u64),
            flashloan_available: U256::from(2_000_000u64),
            calldata: Bytes::new(),
            strategy_bytecode_hash: B256::from([0xaa; 32]),
            l2_exec_gas_estimate: 100_000,
            l1_data_gas_estimate: 5_000,
            extraction_gas: 45_000,
            expected_profit_net: U256::from(1_000_000_000_000_000_000u128),
            dynamic_min_profit: U256::from(100_000_000_000_000_000u128),
            l2_buffer_factor: 1.15,
            l1_data_buffer_factor: 1.10,
            slippage_bps: 20,
            base_fee_at_creation: 50,
            l1_data_fee_at_creation: 40,
            priority_fee_gwei: 10,
            price_impact_bps: None,
            ofa_compliant: false,
            expiry_block: 1_000,
            nonce: 1,
            confirmation_depth: 12,
            client_order_id,
            idempotency_key: B256::ZERO,
            relay_targets: vec!["flashbots".to_string()],
            zk_proof_commitment: None,
        };
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();
        bp
    }

    // ── Flashloan provider resolution ─────────────────────────────────

    #[test]
    fn zero_address_resolves_to_none() {
        assert_eq!(
            resolve_flashloan_provider_id(Address::ZERO).unwrap(),
            "none"
        );
    }

    #[test]
    fn nonzero_address_fails_closed() {
        let result = resolve_flashloan_provider_id(Address::from([0x11u8; 20]));
        assert!(matches!(
            result,
            Err(ExecutionError::UnknownFlashloanProvider { .. })
        ));
    }
}
