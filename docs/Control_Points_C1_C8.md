# docs/Control_Points_C1_C8.md — audit & fixes

## Status matrix

| ID | Control | Status on `main` | This package |
|----|---------|------------------|--------------|
| **C1** | Kill switch reachable from binary | **Already fixed** — `omega-risk` is a dep; `KillSwitchRegistry` constructed; pipeline guards | Documented |
| **C2** | 15 pre-trade checks | **Already fixed** — `ExecutionPipeline` calls `run_all_checks`; `score_and_admit` → `execute` | Documented |
| **C3** | Bytecode / freeze authoritative | Partially — Stage 2b + manifest; empty registry fails closed at phase≥1 (C4b); **no live eth_getCode** vs on-chain codehash | Documented residual |
| **C4** | No-self-flash / provider id | **Fixed here** — `resolve_flashloan_provider_id` uses `FlashloanProviderType` → `aave`/`balancer`/`uniswap`/`none` | Code change |
| **C5** | Idempotency eviction | **Fixed here** — `run_idempotency_eviction_loop` (60s / 2h) | Code change |
| **C6** | Reorg-risk events owned | **Fixed here** — `record_outcome("global", None, false)` on `LaReorgRiskEvent` | Code change |
| **C7** | Realized inclusion → kill switch | **Fixed here** — reconcile results call `record_outcome(..., included)` | Code change (P&L still `None` until ConfirmationResult carries profit) |
| **C8** | DAG after hot-path/ZK | **Already fixed** — `execute` owns `DagSlotGuard`; early exits call `dag.complete` | Documented |

## Live path (post-fix)

```text
score_and_admit
  → (hot or ZK-gated)
  → ExecutionPipeline::execute
       Stage 1 integrity hash
       Stage 2 kill switch guard
       Stage 2b full_integrity_check (freeze + bytecode)
       Stage 2c run_all_checks (15+)
       Stage 3 idempotency
       Stage 4–6 sign / relay
  → DagSlotGuard Drop releases slot

reconcile_inclusions → KillSwitchRegistry::record_outcome
LaReorgRiskEvent     → KillSwitchRegistry::record_outcome(success=false)
idempotency eviction loop (background)
```

## Residual (honest)

1. **C3 live codehash** — registry hash vs `extcodehash` still only on-chain in Orchestrator; off-chain compares blueprint-claimed hash to manifest.
2. **C5 multi-instance** — cache remains process-local (Redis/DB not in scope).
3. **C7 economic P&L** — inclusion success only; wire realized wei when confirmation carries it.
4. **C6 rescoring** — kill-switch accounting only; LA position rescore still open.

## Apply

```bash
cp src/pipeline.rs crates/omega-execution/src/pipeline.rs
cp patches/main.rs src/main.rs
cargo test -p omega-execution -- aave_type_resolves
cargo check --workspace
```
