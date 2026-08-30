# docs/Certora_C7_Freeze.md strategy always reverts

**Property:** *A blueprint with a frozen `strategyId` must always fail — on-chain and off-chain.*

## Layers of enforcement

| Layer | Mechanism |
|-------|-----------|
| **Solidity** | `strategy_frozen[id]`; `execute` reverts `StrategyIsFrozen`; `freezeStrategy` is write-once (`DEFAULT_ADMIN_ROLE`) |
| **IntegrityRegistry** | `freeze` / `check_frozen` / `full_integrity_check` (C7 **before** C4 bytecode) |
| **ExecutionPipeline** | Stage 2b calls `full_integrity_check` — frozen → `IntegrityRegistryCheckFailed(StrategyFrozen)` |
| **score_and_admit (this package)** | Early `check_frozen` before score / build / ZK / DAG admit |
| **register (this package)** | Refuses re-registration of a frozen id (mirrors on-chain register) |

## Semantics

- **Write-once:** no unfreeze API. Recovery = new strategy id + new registration.
- **Not a startup step:** `main` never freezes at boot; governance / control plane only.
- **Idempotent freeze:** second freeze is a no-op (no double metrics).

## Tests already on main

- `frozen_strategy_fails_freeze_check`
- `freeze_is_permanent` / isolation across strategies
- `frozen_fails_before_bytecode_check`
- Pipeline: `integrity_registry_frozen_strategy_fails`
- Pipeline: mid-flight freeze never lets a post-freeze execute through
- Property: freeze prevents all relay calls

## This package adds

1. `register` refused when `is_frozen` (cannot “unfreeze” by overwriting entry)
2. `score_and_admit` early C7 gate
3. Test: `register_refused_when_frozen`

## Apply

```bash
cp src/integrity.rs crates/omega-security/src/integrity.rs
cp patches/main.rs src/main.rs   # or apply the score_and_admit hunk only
cargo test -p omega-security
cargo test -p omega-execution -- integrity_registry_frozen
cargo check --workspace
```
