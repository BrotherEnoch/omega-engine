# docs/P1_P12_Production_Blockers_Status.md — verified status (post control-point + C-series work)

The audit text listed below was **true of an earlier tree**. Against current `main`
(after ExecutionPipeline wiring, KeyManager signer, relay bootstrap, C4 flashloan type
map, C5 eviction, C6/C7 kill-switch feeds), most items are **already closed**.

## Matrix

| ID | Claim | Current reality |
|----|--------|-----------------|
| **P1** | Execution path disconnected; no omega-execution dep | **Closed.** Binary depends on `omega-execution` / `omega-risk` / `omega-security` / `omega-relay`. `score_and_admit` → `ExecutionPipeline::execute` (Stages 0–6). |
| **P2** | No production signer | **Closed.** `KeyManagerTransactionSigner` (env `OMEGA_TX_SIGNING_KEY` / blueprint key). |
| **P3** | No production relay clients | **Closed.** `HttpRelayClient` + `MultiRelayClient` from `OMEGA_RELAY_ENDPOINT_<NAME>` + auth env. |
| **P4** | No secrets/endpoints | **Closed by design.** Endpoints/auth are **env-only** (not TOML). See header in `main.rs`. |
| **P5** | Two RelayConfig types, no translator | **Closed.** Production factory in main translates `OmegaConfig.relay` + env endpoints. |
| **P6** | Empty IntegrityRegistry | **Closed.** `config/deployment_manifest.toml` loaded via `load_deployment_manifest` + `strategy_entries_from_manifest`. Missing file → warn / empty (phase≥1 still fails closed on integrity). |
| **P7** | Flashloan address→name missing | **Closed.** `resolve_flashloan_provider_id` uses `FlashloanProviderType` → `aave`/`balancer`/`uniswap`/`none`. |
| **P8** | Kill switch / CheckContext not production | **Mostly closed.** Live `build_check_context` (oracles, gas, flashloan, competition, exposure). Kill-switch thresholds were `u128::MAX` placeholders → **this package loads `OMEGA_KILL_*` with tight defaults**. |
| **P9** | Stage 7 no caller | **Closed.** Reconciliation task calls `reconcile_inclusions` and `record_outcome`. |
| **P10** | Idempotency no eviction | **Closed.** `run_idempotency_eviction_loop` (60s / 2h). |
| **P11** | Reorg receiver dropped | **Closed.** Receiver held; events call `record_outcome(..., false)`. |
| **P12** | `active_phase = 0` | **Intentional.** Shadow default in `config/default.toml`. Raise phase in TOML/`OMEGA_CONFIG` only when keys, relays, and manifest are ready. Phase 0 still runs scoring/checks; suppresses live relay submit. |

## Remaining ops (not code blockers)

1. Set secrets: `OMEGA_TX_SIGNING_KEY`, `OMEGA_BLUEPRINT_SIGNING_KEY`, `OMEGA_RELAY_ENDPOINT_*`, relay auth tokens, `ARBITRUM_RPC_URL`.
2. Confirm `config/deployment_manifest.toml` bytecode hashes match **your** deployed strategy contracts (sample file uses Hardhat-style addresses — replace for production).
3. Raise `active_phase` when ready for live submit.
4. Tune `OMEGA_KILL_*` for your risk budget.

## This package change (P8 residual only)

```rust
kill_switch_config_from_env()
// OMEGA_KILL_MAX_CUMULATIVE_LOSS_WEI     default 1 ETH
// OMEGA_KILL_MAX_LOSS_PER_WINDOW_WEI     default 0.25 ETH
// OMEGA_KILL_LOSS_WINDOW_SECS            default 3600
// OMEGA_KILL_MAX_CONSECUTIVE_FAILURES    default 5
```

## Apply

```bash
cp patches/main.rs src/main.rs
cargo check -p omega-engine
```
