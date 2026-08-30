# docs/C4_Security_Integrity_Bootstrap.md

## Status

**Implemented on main.** This package documents the flow and adds residual
fail-closed hardening (C4b).

## Requirements → code

| Requirement | Location |
|-------------|----------|
| `IntegrityRegistry` | `omega_security::integrity::IntegrityRegistry` — DashMap entries + DashSet frozen |
| Deployment manifest | `config/deployment_manifest.toml` via `load_deployment_manifest` → `DeploymentManifest` |
| Real bytecode hashes | `parse_bytecode_hash` rejects malformed / wrong length / **all-zero**; `strategy_entries_from_manifest` |
| Strategy registration | `register` / `register_all` at startup; L13 strategy registry separately uses snapshot for LA/CNRY |
| Freeze behavior | Write-once `freeze` (Certora C7); **not** called at startup — governance only |

## Startup flow (`main.rs`)

```
IntegrityRegistry::new()
  → load_deployment_manifest("config/deployment_manifest.toml")
       Some → strategy_entries_from_manifest(manifest, active_phase)
              → register_all(entries)   // one bad entry fails whole call
       None → warn, empty registry
  → C4b: if phase >= 1 && registered_ids().is_empty() → bail!
  → (never freeze at startup)
```

Hot path: `full_integrity_check` = `check_frozen` then `check_bytecode` (matches on-chain order).

`resolve_strategy_bytecode_hash` feeds CheckContext from the registry snapshot — not `[0u8;32]`.

## Fail closed

| Condition | Behavior |
|-----------|----------|
| Manifest file malformed / invalid entry | `main` returns `Err` |
| All-zero bytecode_hash or address in manifest | `InvalidDeploymentEntry` |
| `register` with zero hash/address (programmatic) | Refused, not inserted (C4b) |
| Empty registry, phase 0 | Warn; Stage 2b rejects all strategies |
| Empty registry, phase ≥ 1 | **Startup `bail!` (C4b)** |
| Frozen strategy | `StrategyFrozen` before bytecode check |
| Unknown strategy | `StrategyUnknown` |
| Freeze twice | Idempotent (C4b) |

## Freeze semantics

- Matches Orchestrator: write-once, no unfreeze API.
- Governance / control-plane only — **never** a startup step.
- New strategy after freeze requires a **new strategy id**.

## Apply

```bash
cp src/integrity.rs crates/omega-security/src/integrity.rs
cp patches/main.rs src/main.rs   # or apply the C4b empty-registry hunk only
cargo test -p omega-security
cargo check --workspace
```
