# ExecutionPipelineSpecification.md
# Execution Pipeline Specification — Draft v0.1

**Status: DESIGN ONLY.** No code in this document is meant to be copied in
verbatim — every function signature below is illustrative, to pin down
*contracts*, not implementation. Nothing here should be built until the
open questions in §12 are answered by you.

---

## 1. Problem statement

`omega-engine` has three independently correct, independently tested
components that do not currently connect:

| Component | Owns | Does NOT own |
|---|---|---|
| `omega-dag` + `omega-hot-path` + `omega-zk` | Scheduling, admission, simulation, proof generation for an `ExecutionBlueprint` | Anything past "blueprint ready" / "proof ready" |
| `omega-risk` | 15 pre-trade checks (`checks.rs`) + kill switch (`kill_switch.rs`) | Any caller that invokes them |
| `omega-relay` | Reliable, backpressured, deduplicated, reorg-aware bundle submission (`BundlePayload` in, `CascadeResult`/`bool` out) | Anything about where a `BundlePayload` comes from |

`src/main.rs`'s `score_and_admit` is the only code in the workspace that
touches all three blueprint-producing layers, and it terminates at two
`tracing::info!` calls — one per branch (hot-path / normal+ZK) — with a
comment ("blueprint ready" / "proof ready") and nothing after it. No
`BundlePayload` is ever constructed anywhere outside `omega-relay`'s own
tests and benches. `omega-risk` is not a dependency of the binary at all.

This is confirmed, not inferred: every one of `omega-relay`'s manifest,
`omega-dag`'s manifest, `omega-hot-path`'s manifest, and `main.rs`'s actual
`use` statements was read directly, and the v12.0 spec's own §22.1
inter-crate dependency graph does not list a crate that depends on both
`omega-risk` and `omega-relay` at once. **The gap is real at every layer
that was checked, including the spec itself.**

This document specifies the missing stage: the thing that takes a
DAG-admitted, hot-path-or-ZK-simulated `ExecutionBlueprint` and turns it
into a submitted, kill-switch-gated, deduplicated bundle.

---

## 2. Where this component sits

```
omega-strategies::build_blueprint
        │
        ▼
  ExecutionBlueprint
        │
        ▼
   omega-dag::admit()  ──────────► DagError → DropCode (existing)
        │
        ▼
   omega-dag::ready()
        │
        ├── hot_path_eligible? ──► omega-hot-path::HotPathRunner
        │                                   │
        │                                   ▼
        │                          HotPathSimResult (existing)
        │
        └── else ────────────────► omega-zk::ProofQueue
                                              │
                                              ▼
                                     ZK proof (existing)
        │
        ▼
┌───────────────────────────────────────────────────────┐
│         >>> THIS DOCUMENT: the missing stage <<<        │
│                                                         │
│  1. Integrity check   (verify_hash / verify_idempotency_key) │
│  2. Pre-trade risk gate (omega-risk::checks + kill switch)   │
│  3. Submission-layer idempotency dedup                  │
│  4. ExecutionBlueprint → BundlePayload transform         │
│  5. Submit via omega-relay::MultiRelayClient              │
│  6. Post-submission bookkeeping (DAG complete, reorg guard, │
│     kill-switch outcome recording)                       │
└───────────────────────────────────────────────────────┘
        │
        ▼
   omega-relay::MultiRelayClient::cascade_submit / submit_single
        │
        ▼
   CascadeResult / bool  (existing — already returns to caller)
        │
        ▼
   MultiRelayClient::reconcile_inclusions (existing, needs periodic driver)
```

Everything below the box and everything above it already exists and is
tested. This document is scoped to the box only.

---

## 3. Naming

Not `SafeSubmitter` — that name undersells the scope once the real
architecture is visible. This stage validates, gates, transforms, *and*
submits; "submitter" implies only the last step. Candidates, for you to
pick from (or reject):

- `ExecutionPipeline` — matches the diagram shape, avoids implying it's a
  single object rather than a sequence of stages.
- `ExecutionCoordinator` — emphasizes that it orchestrates calls into
  `omega-risk`/`omega-relay` rather than owning their logic itself.
- `BlueprintExecutor` — shortest, ties directly to the type it consumes.

This document uses `ExecutionPipeline` as a placeholder name throughout.
Rename freely; nothing here depends on the name.

---

## 4. Why no existing crate is the right home

Restating the evidence already gathered, for a single source of truth:

- **`omega-dag`**: `Cargo.toml` deps are `omega-core`, `alloy-primitives`,
  `petgraph`, `anyhow`/`thiserror`, `serde`/`serde_json`, `chrono`,
  `tracing`, `uuid`. No `omega-relay`, no `omega-risk`. `scheduler.rs`'s
  own module doc calls its caller "the orchestrator" — i.e., it expects
  to be called *by* this stage, not to *be* it.
- **`omega-hot-path`**: `Cargo.toml` deps are `omega-core`,
  `alloy-primitives`, `tokio`, `anyhow`/`thiserror`, `serde`/`serde_json`,
  `tracing`, `chrono`, `arc-swap`, `uuid`. No `omega-relay`, no
  `omega-risk`. Its own `simulator.rs` doc comment says explicitly: *"In
  the full engine **the orchestrator** holds an `Arc<RevmCacheManager>`...
  The orchestrator then routes through the ZK layer before relay
  submission."* — again, describing a caller it isn't.
- **`omega-relay`**: has no knowledge of `ExecutionBlueprint`,
  `idempotency_key`, or `strategy_id` at all — `BundlePayload` doesn't
  carry those fields. It cannot host the risk gate because it structurally
  cannot see the fields the risk gate needs.
- **`src/main.rs`**: is the one place with visibility into everything, but
  doesn't import `omega-risk` or `omega-relay` today, and its pipeline
  ends at logging.

One discrepancy worth flagging on its own: the v12.0 spec's §22.1
dependency graph states `omega-hot-path ← omega-core, omega-zk,
omega-relay` — but the actual `Cargo.toml` we read has none of that; its
own audit note explains the exclusion of `omega-zk` was deliberate ("keep
compile times low"), and `omega-relay` isn't mentioned in that note at
all, just silently absent. **This means the spec and the implementation
already disagree on whether `omega-hot-path` itself is supposed to reach
`omega-relay` directly.** That disagreement should be resolved as part of
answering §12, not assumed away by this document.

---

## 5. Proposed structural home

Given §4, the two live options are:

**(a) New crate**, e.g. `crates/omega-execution`, depending on
`omega-core`, `omega-risk`, `omega-relay`, `omega-dag`, `omega-hot-path`,
`omega-zk`. Pro: keeps `omega-dag`/`omega-hot-path` dependency-minimal as
their own doc comments say they're designed to be. Con: adds a 24th crate
for one orchestration stage — the earlier "reject Option 2" instinct
wasn't wrong in principle, it was wrong only in application (rejecting it
*before* confirming the gap was real).

**(b) Inline in `src/main.rs`** (the binary crate itself, or a
`src/execution/` module within it), since `main.rs` *already* depends on
every crate this stage needs except `omega-risk` and `omega-relay` — both
of which are trivial one-line manifest additions since they're already in
the workspace. Pro: no new crate, and `main.rs` is already where
`score_and_admit` lives — this stage is a direct continuation of that
function, not a separate service. Con: harder to unit-test in isolation
from the full binary; harder to reuse if a second binary (e.g. a backtest
runner) needs the same logic.

This document does not choose between (a) and (b) — see §12.

---

## 6. Inputs / trigger points

Two call sites already exist in `main.rs::score_and_admit` and are the
natural trigger points — this stage does not need its own polling loop:

```
if hot {
    // existing: hp_tx.try_send(...) → rrx.await → resp.result
    // TODO: on Ok(HotPathSimResult), call ExecutionPipeline::execute(bp, sim_result)
} else {
    // existing: proof_queue.submit(...) → rx.await → proof
    // TODO: on Ok(proof), call ExecutionPipeline::execute(bp, proof)
}
```

Both branches already have the blueprint (`bp`) and a proof of readiness
(`HotPathSimResult` or a ZK `proof`) in scope at the exact point logging
happens today. No new channel or task is required to *receive* work; the
new logic replaces the `tracing::info!` calls, not the surrounding
plumbing.

---

## 7. Stage-by-stage pipeline

### Stage 0 — Phase gate
`active_phase < 1` → do nothing (this already exists as a condition in
`main.rs`; it currently gates the log line, it should gate the entire
pipeline call instead). Matches the spec's "Phase 0: shadow mode — relay
submission suppressed" comment already present in `main.rs`.

### Stage 1 — Integrity check
```
if !bp.verify_hash() || !bp.verify_idempotency_key() {
    // treat identically to SIMULATION_STATE_MISMATCH per blueprint.rs's
    // own doc comment: "discard it, never submit it"
    return Err(DropCode::SimulationStateMismatch);
}
```
Cheapest possible check, catches any accidental post-construction
mutation before any of the more expensive stages run.

### Stage 2 — Pre-trade risk gate
Two independent sub-gates, both must pass:

1. **Kill switch.** `KillSwitchRegistry::guard(&bp.strategy_id.to_string())`.
   Scope key must be decided (§12) — strategy-level (`"LA"`, `"SA"`, ...)
   matches every registry example seen in `kill_switch.rs`'s own tests.
2. **15 pre-trade checks.** `omega_risk::checks::run_all_checks(&fields,
   &ctx)` where `fields: BlueprintFields` is derived from `bp`, and `ctx:
   CheckContext` is assembled fresh from live state at submission time
   (not from anything cached at blueprint-construction time — several of
   the 15 checks, e.g. gas spike since creation and oracle freshness,
   exist specifically to catch drift between construction time and
   submission time). `CheckContext` needs, at minimum: current block,
   current L1 gas price, an `OracleSnapshot`, a `FlashloanSnapshot`,
   competition probability, and a risk score — all of which come from
   crates `main.rs` already holds handles to (`oracle: Arc<PerChainOracle>`
   today) or will need new handles added for.

On failure of either sub-gate: record the `DropCode`/`TripReason`,
**do not proceed to Stage 3**, call `dag.complete(bp.blueprint_hash)` so
the DAG slot is freed (mirroring what `score_and_admit` already does on
every exit path today), and return.

### Stage 3 — Submission-layer idempotency dedup
Distinct from `omega-relay::dedup::SequencerRestartHandler`, which is
keyed on `PositionKey` (a liquidation position identity) and lives inside
`omega-relay`. This stage needs a **separate** cache keyed on
`bp.idempotency_key` (a `B256`), checked *before* a `BundlePayload` is
ever built — this is the exact guard `ExecutionBlueprint`'s own module doc
describes as "catching the step before" the RPC-layer dedup, i.e. a
duplicated scorer invocation of the same trade at the same nonce.

```
match idempotency_cache.entry(bp.idempotency_key) {
    Entry::Vacant(e) => { e.insert(now); /* proceed */ }
    Entry::Occupied(_) => return Err(DropCode::DuplicateIdempotencyKey),
    // DropCode::DuplicateIdempotencyKey already exists — see
    // omega-core/src/errors.rs, confirmed via grep in this investigation.
}
```
Should reuse the same atomic-`Entry`-match pattern already used correctly
in `omega-relay::dedup::SequencerRestartHandler::try_submit` and
`KillSwitchRegistry::get_or_create` (both fixed earlier in this
conversation for exactly this TOCTOU class of bug) — not a
`contains_key` + separate insert.

### Stage 4 — `ExecutionBlueprint` → `BundlePayload` transform
Exact field mapping (see §8 for the full table). The core question this
stage answers that no existing code answers: **how is `bp.calldata`
turned into `BundlePayload.txs: Vec<String>`?** `BundlePayload.txs` is
"signed transaction hex strings" per `client.rs`'s doc comment —
`ExecutionBlueprint` has no signing key or nonce-aware transaction
builder anywhere in scope in this investigation. This is very likely
where `omega-security::signer.rs` ("Sign the keccak256 of every
ExecutionBlueprint before relay submission") plugs in — that crate was
referenced but never opened in this investigation. **This is a hard
dependency this document cannot resolve without reading
`omega-security/src/signer.rs`.**

### Stage 5 — Submission
```
if bp.lane == Lane::Microtx && /* cascade not required */ {
    relay_client.submit_single(payload).await
} else {
    relay_client.cascade_submit(vec![payload]).await
}
```
The hot_path/microtx-vs-cascade split needs a decision (§12) — nothing in
`omega-relay` currently signals which mode a given `ExecutionBlueprint`
should use; `submit_single` and `cascade_submit` are both public and
tested but the *choice* between them isn't specified anywhere yet.

### Stage 6 — Post-submission bookkeeping
All of the following already exist as methods and simply need to be
called in sequence, using data this stage already has in scope:
- `dag.complete(bp.blueprint_hash)` — already called unconditionally at
  the end of `score_and_admit` today; needs to move to *after*
  submission rather than immediately after hot-path/ZK readiness.
- `reorg_guard.on_bundle_submitted(tx_hash, block)` (via
  `MultiRelayClient::on_bundle_submitted`) — currently never called by
  anything in the workspace outside `omega-relay`'s own tests.
- `kill_switch_registry.record_outcome(scope, realized_profit_wei,
  success)` — this can only be populated **after** on-chain confirmation
  (Stage 7), not at submission time, since `realized_profit_wei` isn't
  known until then. This means Stage 6 has two parts on two different
  timelines: an immediate part (DAG completion, reorg registration) and a
  deferred part (kill-switch outcome recording), and the deferred part
  needs to be wired into whatever already calls
  `reconcile_inclusions`/`ConfirmationResult`.

### Stage 7 — Confirmation reconciliation
Already fully implemented (`MultiRelayClient::reconcile_inclusions`), but
**nothing in the workspace currently calls it on a schedule** — it's
tested directly in `omega-relay`'s own integration tests but has no
periodic driver in `main.rs`. This stage needs a new periodic task in
`main.rs` (a `tokio::time::interval` loop, same shape as
`run_health_monitor`), calling `reconcile_inclusions(current_block)` and
feeding each `ConfirmationResult` into `KillSwitchRegistry::record_outcome`.

---

## 8. Data contract: `ExecutionBlueprint` → `BundlePayload`

| `BundlePayload` field | Source | Notes |
|---|---|---|
| `bundle_hash` | ? | **Not** `bp.blueprint_hash` directly per `ExecutionBlueprint`'s own doc comment, which says `blueprint_hash` is a pre-signing content hash; `bundle_hash` is described in `client.rs` as "keccak256 of the **serialised bundle**" — i.e. computed from the signed `txs`, which don't exist until Stage 4/signing happens. Needs `omega-security::signer.rs` to resolve. |
| `txs` | `bp.calldata` + signing | See Stage 4 — blocked on `omega-security`. |
| `block_number` | Live chain tip (hex) | From `omega-rpc`/oracle snapshot, not from `bp`. |
| `min_timestamp` / `max_timestamp` | Derived from `bp.expiry_block` and current block time | Not a direct field copy — needs a block→timestamp conversion, not present on `bp`. |
| `priority_fee_gwei` | `bp.priority_fee_gwei` | Direct copy — this one's unambiguous. |

Four of five fields are **not** direct copies and require either another
crate (`omega-security` for signing) or live chain state (`omega-rpc`)
this stage doesn't yet have a specified way to obtain. This table is the
single most concrete deliverable of this document: it shows the
blueprint→bundle transform is not a trivial struct-literal mapping, it's
its own sub-component with a real dependency on the signing layer.

---

## 9. Failure taxonomy at this stage

All failure modes should terminate in an existing `DropCode` where one
already fits, rather than inventing new ones:

| Failure | `DropCode` | Source |
|---|---|---|
| `verify_hash()`/`verify_idempotency_key()` fails | `SimulationStateMismatch` (reused) | Precedent: `blueprint.rs` doc comment explicitly says to treat hash-verification failure the same as this code |
| Kill switch tripped | *(no DropCode — `KillSwitchError::Tripped`)* | `omega-risk::kill_switch` — this stage should log and skip, not map to a `DropCode`, since kill-switch trips aren't part of the 13/15-check `DropCode` enum |
| Any of the 15 pre-trade checks fail | Whatever `CheckResult::Fail(code)` returns | `omega-risk::checks::run_all_checks` already returns the correct code |
| Duplicate `idempotency_key` | `DuplicateIdempotencyKey` | Already exists in `omega_core::errors::DropCode`, confirmed via grep |
| Signing failure | *(new — not found anywhere yet)* | Depends on what `omega-security::signer.rs` actually exposes; unresolved |
| Relay submission: no relay accepted | *(no DropCode — `RelayError::AllRelaysFailed`)* | `omega-relay`'s own error type already covers this |

---

## 10. Concurrency & ownership

This stage needs `Arc`-shared handles to:
- `Arc<KillSwitchRegistry>` (new — not constructed anywhere in `main.rs`
  today)
- `Arc<MultiRelayClient>` (new — not constructed anywhere in `main.rs`
  today; note `MultiRelayClient::new` returns `(Arc<Self>,
  mpsc::UnboundedReceiver<LaReorgRiskEvent>)` — something needs to own
  and drain that receiver, which nothing in `main.rs` does yet either)
- `Arc<PerChainOracle>` (already exists in `main.rs`)
- Whatever `omega-security::signer.rs` exposes (unknown — unread)
- A new idempotency cache (`Arc<DashMap<B256, DateTime<Utc>>>` or similar,
  following the same `Entry`-match pattern as the other two dedup
  structures already in the codebase)

All of these should be constructed once in `main()` alongside the
existing L0–L15 layer setup, and passed into whatever owns Stage 0–7 —
mirroring exactly how `dag`, `hp_tx`, and `proof_queue` are already
threaded through `score_and_admit` today.

---

## 11. Testing requirements (once implemented)

Mirroring the rigor already present in every file touched in this
investigation:
- Stage ordering: a blueprint that would fail both the kill switch and a
  pre-trade check must fail with the kill-switch outcome if checked first
  — the order across Stage 1–3 needs to be fixed and tested, the same way
  `checks.rs`'s fast-fail order is tested exhaustively.
- Idempotency: two blueprints with identical `idempotency_key` but
  different `signal_id` (the exact scenario `blueprint.rs`'s own test
  `same_nonce_same_params_same_idempotency_key` sets up) must result in
  exactly one submission.
- DAG slot release: every exit path (Stage 1 through Stage 6 failure) must
  call `dag.complete()` exactly once — a leaked DAG slot on a rejected
  blueprint would silently shrink capacity over time.
- Reconciliation: `reconcile_inclusions` outcomes must correctly reach
  `KillSwitchRegistry::record_outcome` with the right scope and signed
  `realized_profit_wei`.

---

## 12. Open questions — answer these before any code is written

1. **Crate placement** (§5): new `omega-execution` crate, or inline in
   `src/main.rs`?
2. **`omega-security::signer.rs`**: what does it actually expose? This
   blocks Stage 4/§8 entirely and needs to be read before the
   blueprint→bundle transform can be specified precisely rather than
   sketched.
3. **Kill-switch scope key**: strategy-level (`"LA"`, `"SA"`, `"MSA"`,
   `"MEV"`) as used in every `kill_switch.rs` example, or something
   finer-grained (per chain? per strategy+chain)?
4. **Cascade vs. single submission**: what decides which
   `MultiRelayClient` method a given blueprint uses? Lane? Strategy?
   Explicit field on `ExecutionBlueprint` that doesn't exist yet?
5. **`hot-path ↔ relay` spec/code mismatch** (§4): does `omega-hot-path`
   get an `omega-relay` dependency added (matching the v12.0 spec), or
   does the spec's dependency graph get corrected to remove it (matching
   the current, deliberately-minimal implementation)?
6. **`LaReorgRiskEvent` receiver**: who owns and drains it once
   `MultiRelayClient` is constructed in `main.rs`? Right now only
   `omega-relay`'s own tests consume it.