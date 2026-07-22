# ProductionIntegrationPlan.md
# Production Integration Plan — Shadow Mode → Live Execution

**Status: planning document. No code in this file. Every gap below is
backed by a specific finding earlier in this investigation — cited
inline — not a guess.**

---

## 1. Where things actually stand

- `src/main.rs`'s pipeline (`score_and_admit`) ends at two `tracing::info!`
  calls — "blueprint ready" / "proof ready" — and does nothing after.
- `crates/omega-execution` implements Stages 0–6 of the execution
  pipeline, fully tested (31 tests: correctness, panic-safety, property
  invariants, concurrency/load), but is **not called from `main.rs`**.
- Twelve gaps stand between the current state and a live, wired
  `main.rs → omega-execution → real relay` path. None of them are coding
  puzzles this document can solve by writing more Rust — each one either
  needs a real external input (keys, endpoints, deployed addresses) or a
  decision only you can make (risk thresholds, secrets strategy).

This document inventories all twelve, in one place, so nothing found
earlier gets lost.

---

## 2. Gap inventory

### Gap 1 — Transaction Signing
**Status:** Interface exists (`TransactionSigner` trait,
`omega-execution/src/signer.rs`). No production implementation anywhere
in the workspace. `omega_security::BlueprintSigner` authorizes to the
on-chain Orchestrator; it doesn't sign a transaction envelope.
`omega_simulation::SimulationSubmitter` signs real transactions but only
against a local Anvil fork with a dev key, deliberately walled off from
live use.
**Blocks:** Stage 4 (blueprint → signed tx).
**Needs:** An HSM/KMS-backed implementation, or one built on
`omega_security::KeyManager` plus a real RLP transaction encoder.
**Acceptance criteria:**
- Implements `TransactionSigner`.
- Produces a correctly RLP-encoded, EIP-1559 (or Arbitrum-appropriate)
  signed transaction with correct nonce/gas/chain_id.
- Key material never logged, never held longer than the signing call.
- Tested against a real testnet, not just `MockTransactionSigner`.

### Gap 2 — Relay Client Construction ("Relay Bootstrap")
**Status:** `HttpRelayClient` is fully implemented in
`omega-relay/src/client.rs`. Confirmed via exhaustive grep: it has **zero
production call sites** anywhere in the workspace, including the backup.
Every `HashMap<String, Arc<dyn RelayClient>>` construction site found is
test or bench code building `MockRelayClient`s.
**Blocks:** `MultiRelayClient` construction — therefore all of Stage 5.
**Needs:** A `RelayClientFactory` (see §3 — implemented as real code in
this session) that builds one `Arc<dyn RelayClient>` per configured
relay name.
**Acceptance criteria:**
- Implements `RelayClientFactory`.
- Produces one `HttpRelayClient` per relay in
  `RelayConfig.phase_N_relays`.
- Reads endpoint URL + `RelayAuth` from Gap 3's secrets source — never
  hardcoded, never a placeholder value.
- Fails loudly at startup if a configured relay has no corresponding
  endpoint/auth — never silently drops it from the map.

### Gap 3 — Deployment Configuration / Secrets
**Status:** No file anywhere in the workspace contains endpoint URLs or
auth material. Every `config/*.toml` file has been read in this
investigation (`arbitrum.toml`, `base.toml`, `builder_blacklist.toml`,
`default.toml`, `ofa_rules.toml`) — none of them qualify.
**Blocks:** Gap 2.
**Needs:** A decision on where secrets live: environment variables, an
encrypted config file, a secrets manager (Vault, AWS Secrets Manager,
etc.). This is a decision, not an engineering task.
**Acceptance criteria:**
- Relay endpoint URLs + `RelayAuth` retrievable at startup.
- Confirmed excluded from source control regardless of format chosen.
- A rotation procedure exists — `omega_security::KeyManager`'s dual-key
  rotation window is the existing precedent in this codebase for what
  "rotation" should look like; new secrets should follow a comparable
  pattern for consistency, not necessarily reuse that exact code.

### Gap 4 — Config Translation Layer
**Status:** `omega_core::config::RelayConfig` (nested in `OmegaConfig`,
matches `default.toml`'s `[relay]` section exactly) and
`omega_relay::config::RelayConfig` (what `MultiRelayClient::new` actually
consumes) are two different Rust types with almost no field overlap.
Confirmed: no conversion code between them exists anywhere in the
workspace.
**Blocks:** Gap 2 (the factory needs a valid `omega_relay::config::RelayConfig`
to build against).
**Needs:** An explicit adapter — `OmegaConfig.relay` + Gap 3's secrets →
`omega_relay::config::RelayConfig`.
**Acceptance criteria:**
- Pure function, unit-testable without network access.
- Every field of `omega_relay::config::RelayConfig` has one documented
  source (either `OmegaConfig` or Gap 3).
- Fails at startup, not silently, if a required field can't be sourced.

### Gap 5 — Kill Switch Bootstrap
**Status:** `KillSwitchRegistry` is fully implemented and tested
(`omega-risk`). Not constructed anywhere in `main.rs`; `omega-risk` isn't
even a dependency of the binary today.
**Blocks:** Stage 2a of the execution pipeline.
**Needs:** Real values for `KillSwitchConfig`
(`max_cumulative_loss_wei`, `max_loss_per_window_wei`, `loss_window`,
`max_consecutive_failures`). These are risk-tolerance decisions, not
technical unknowns — I can't propose defaults responsibly for a system
handling real capital.
**Acceptance criteria:**
- Each threshold has documented rationale, not just a bare number.
- Constructed once in `main()`, shared via `Arc` into `ExecutionPipeline`.

### Gap 6 — `IntegrityRegistry` Deployment Manifest
**Status:** `strategy_entries_from_manifest()` is implemented and
correctly fails closed on placeholder/zero data (`omega-security`, fixed
earlier this session). No real manifest exists anywhere.
**Blocks:** The bytecode integrity check described in the spec — and see
Gap 9, since this check isn't even wired into `omega-execution` yet.
**Needs:** Real deployed contract addresses + bytecode hashes, one per
active-phase strategy (SA/MSA/LA/MEV).
**Acceptance criteria:**
- One `StrategyDeployment` entry per active strategy contract.
- `bytecode_hash` verified against the actually-deployed contract's
  runtime code via `eth_getCode` — not copy-pasted from a deploy script,
  since a stale value would defeat the entire point of the check.

### Gap 7 — Flashloan Provider Resolution
**Status:** `resolve_flashloan_provider_id()` (`omega-execution`) fails
closed for any non-zero flashloan address, on purpose — no
address→protocol-name table exists anywhere in the workspace.
**Blocks:** Any LA blueprint that actually uses a flashloan — i.e., most
real LA execution.
**Needs:** A real mapping covering the protocols in
`config/arbitrum.toml`'s `[la].protocols` (`aave_v3`, `compound_v3`,
`morpho_blue` — `euler_v2` added in phase 3.1 per that file).
**Acceptance criteria:**
- Covers every protocol listed in `arbitrum.toml`.
- Addresses verified against real **Arbitrum** deployments specifically
  — mainnet Ethereum addresses for the same protocols are different
  contracts and would silently defeat the no-self-flash check if reused
  by mistake.

### Gap 8 — `main.rs` Integration
**Status:** `omega-execution` is complete and tested in isolation. Two
call sites are identified (`score_and_admit`'s hot-path and normal/ZK
branches). Not wired.
**Blocks:** Everything above becomes moot without this, but this is
correctly LAST — wiring it before Gaps 1–7 exist would either fail to
compile (missing real constructors) or require fabricating stand-ins for
exactly the things this plan says not to fabricate.
**Acceptance criteria:**
- Replaces the two `tracing::info!` calls with real
  `ExecutionPipeline::execute()` calls.
- `omega-risk`, `omega-relay`, `omega-security`, `omega-execution` added
  to the root `Cargo.toml`'s `[dependencies]`.
- Gap 10's periodic reconciliation driver added alongside.

### Gap 9 — Bytecode Integrity Check Not Wired Into the Pipeline
**Status:** Newly surfaced while writing this plan, not previously
flagged: `IntegrityRegistry::full_integrity_check()` exists and is
correct, but `ExecutionPipeline::execute_inner()` never calls it. The
6-stage pipeline as implemented has no bytecode-integrity stage at all.
**Blocks:** Nothing structurally today (the pipeline still works without
it) — but it's a real, silent gap in coverage relative to what the spec
describes (Certora C4/C7).
**Acceptance criteria:** Add as its own stage (Stage 2c, alongside the
kill switch and the 15 checks) once Gap 6's manifest exists to check
against.

### Gap 10 — Confirmation Reconciliation Wiring (Stage 7)
**Status:** `MultiRelayClient::reconcile_inclusions()` is fully
implemented. Nothing calls it on a schedule. Wiring its output into
`KillSwitchRegistry::record_outcome` was explicitly left undone earlier
in this session because `confirmation.rs`'s `ConfirmationResult` struct
was never read — guessing its fields would have risked exactly the class
of bug this whole investigation exists to catch.
**Blocks:** The kill switch's realized-loss tracking. It can still trip
on manual triggers and consecutive-failure counts today, just not on
accumulated realized losses from confirmed trades.
**Acceptance criteria:**
- Read `confirmation.rs` to confirm `ConfirmationResult`'s real fields
  before writing this.
- A periodic `tokio::time::interval` driver in `main.rs`, same shape as
  the existing `run_health_monitor`.
- `record_outcome` called with the correct scope + `realized_profit_wei`
  per confirmed result.

### Gap 11 — Idempotency Cache Eviction Scheduling
**Status:** `evict_idempotency_cache()` is public and reachable (fixed
earlier this session). Nothing calls it periodically.
**Blocks:** Nothing immediately — unbounded memory growth over a
long-running process is the failure mode, not a correctness bug.
**Acceptance criteria:** Periodic driver added alongside Gap 10's.

### Gap 12 — `LaReorgRiskEvent` Receiver Ownership
**Status:** `MultiRelayClient::new` returns an
`mpsc::UnboundedReceiver<LaReorgRiskEvent>` that, per `omega-relay`'s own
audit note, silently drops every event if the receiver isn't held.
Nothing outside `omega-relay`'s own tests currently drains it.
**Blocks:** Reorg-risk notifications reaching whatever's supposed to
consume them — not yet identified anywhere in this investigation.
**Acceptance criteria:** `main.rs` owns and drains this receiver. Worth a
targeted search for `LaReorgRiskEvent` consumers before writing Gap 8,
since the right consumer may already exist in a crate not yet read.

---

## 3. Dependency graph

```
Gap 3 (secrets strategy) ──┐
                            ├──► Gap 4 (config translation) ──► Gap 2 (relay bootstrap) ──┐
Gap 5 (risk thresholds) ───┘                                                              │
                                                                                            │
Gap 1 (signer) ─────────────────────────────────────────────────────────────────────────┤
                                                                                            │
Gap 6 (deployment manifest) ──► Gap 9 (wire integrity check into pipeline) ───────────────┤
                                                                                            │
Gap 7 (flashloan table) ───────────────────────────────────────────────────────────────────┤
                                                                                            │
Gap 12 (reorg receiver — needs its own search first) ──────────────────────────────────────┤
                                                                                            │
Gap 10 (confirmation wiring — needs confirmation.rs read first) ──► Gap 11 (eviction) ─────┤
                                                                                            ▼
                                                                              Gap 8 (main.rs integration)
```

---

## 4. Suggested sequencing

**Phase A — decisions, not engineering.** Gap 3 (secrets strategy), Gap 5
(risk thresholds). Nothing downstream can start until these are decided,
and no amount of code review substitutes for your judgment here.

**Phase B — engineering, unblocked once A is decided.** Gap 4 (config
translation), Gap 2 (relay bootstrap — the trait is already written, see
§3 below), Gap 1 (signer implementation).

**Phase C — data gathering, can run in parallel with B.** Gap 6
(deployment manifest — needs real on-chain lookups), Gap 7 (flashloan
table — same), Gap 12's precursor search (find the real
`LaReorgRiskEvent` consumer, if one exists).

**Phase D — wiring, strictly after A–C.** Gap 9, Gap 10 (after reading
`confirmation.rs`), Gap 11, Gap 12, then Gap 8 last — the final
integration that makes everything else load-bearing.

---

## 5. What's implementable now vs. what needs you first

| Gap | Can be implemented once inputs exist | Needs your input/decision first |
|---|---|---|
| 1 Signer | ✅ (needs real key infra) | Which signing backend (HSM/KMS/other) |
| 2 Relay bootstrap | ✅ (trait already written) | — |
| 3 Secrets | — | Secrets strategy decision |
| 4 Config translation | ✅ | — |
| 5 Kill switch | ✅ (mechanical) | Actual threshold values |
| 6 Deployment manifest | — | Real deployed addresses/hashes (from you or your deploy scripts) |
| 7 Flashloan table | — | Real Arbitrum protocol addresses |
| 8 main.rs wiring | ✅ (last, after 1–7) | — |
| 9 Wire integrity check | ✅ (after Gap 6) | — |
| 10 Confirmation wiring | ✅ (after reading confirmation.rs) | — |
| 11 Eviction scheduling | ✅ | — |
| 12 Reorg receiver | Needs a search first | Possibly needs you to confirm intended consumer |