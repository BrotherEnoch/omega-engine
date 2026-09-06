# docs/Production_Blockers.md — residual status (updated 2026-09-06)

**Repo:** BrotherEnoch/omega-engine  
**Branch baseline:** `main` + audit session fixes  

---

## Closed (do not re-open as production blockers)

| ID / area | Status |
|-----------|--------|
| P1–P12 | **Closed** in code |
| C1 kill switch, C2 pre-trade checks, C4 provider id, C5 eviction loop, C6 reorg drain, C7 inclusion→success, C8 DAG | **Closed** (residuals only where noted) |
| Signer, relay bootstrap, integrity manifest load | **Closed** |
| LA `select_provider` + asset-scoped registry | **Closed** |
| **MSA / SA capital path (Option B)** | **Closed** — `select_provider` + non-zero WETH token/amount; Orchestrator-safe |
| **rollout_tier** | **Closed** — feeds composite `risk_score` |
| **LA debt_amount_wei price path** | **Closed (fail-closed)** — `TokenPriceLookup` injected from main (Chainlink/Pyth); returns `None` if price missing/stale |

---

## Bucket A — Ops only (hard live gates, not code bugs)

Must be done before live submit; phase 0 deliberately suppresses relay submit.

1. Secrets: `OMEGA_TX_SIGNING_KEY`, `OMEGA_BLUEPRINT_SIGNING_KEY`, `OMEGA_RELAY_ENDPOINT_*`, relay auth, RPC URLs, `ORCHESTRATOR_ADDRESS`
2. `config/deployment_manifest.toml` hashes/addresses match **your** deployed strategies
3. Raise `active_phase` only after (1)+(2)
4. Tune `OMEGA_KILL_*`
5. `OMEGA_VAULT_SUBMIT_NONCE` from `eth_getTransactionCount` for ZK submit path
6. Ensure Chainlink (and optionally Pyth) feeds are live so LA can size debt in wei

See `Ops_Checklist.md`.

---

## Bucket B — Documented residuals (quality / hardening)

| Residual | Gap |
|----------|-----|
| C3 live codehash | Off-chain compares manifest only; on-chain Orchestrator does live codehash |
| C5 multi-instance | Idempotency cache process-local |
| C7 economic P&L | Stage-7 uses expected profit as proxy, not on-chain realized wei |
| C6 LA rescore | Reorg drain exists; position rescore open |
| PositionRegistry writer | No continuous liquidatable-position scanner yet — LA scores 0 until populated |
| Pyth ingestion | Pyth cache may be unfed depending on ops wiring; Chainlink poll is primary |
| MEV capital path | Intentional zero flashloan (self/externally funded product decision) |
| CNRY zero flashloan | Canary path; not a live capital strategy |

These do **not** open a fail-open path for live capital under phase ≥ 1 gates.

---

## Bucket C — Hard strategy blockers

**None remaining in code for SA/MSA/LA capital fields.**

LA still requires:
1. PositionRegistry population (ops/scanner), and  
2. Fresh Chainlink/Pyth price for the debt token  

Both fail closed (no blueprint) when absent.

---

## Corrected one-liner

**P1–P12 and Option B MSA/SA are closed. LA debt sizing is wired via TokenPriceLookup and fails closed without price/positions. Live go-live still requires ops config (Bucket A) and a position writer for LA.**
