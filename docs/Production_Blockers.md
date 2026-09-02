# docs/Production_Blockers.md — corrected residual status

**Repo:** BrotherEnoch/omega-engine  
**Branch baseline:** `main` (post P1–P12 / C-series docs)  
**Intent of this doc:** Replace the vague “what remains” line with a precise matrix.

---

## Closed (do not re-open as production blockers)

| ID / area | Status |
|-----------|--------|
| P1–P12 (see `docs/P1_P12_Production_Blockers_Status.md`) | **Closed** in code |
| C1 kill switch, C2 pre-trade checks, C4 provider id, C5 eviction loop, C6 reorg drain, C7 inclusion→success, C8 DAG | **Closed** (residuals only where noted) |
| Signer (`KeyManagerTransactionSigner`), relay bootstrap, integrity manifest load | **Closed** |
| LA `select_provider` + asset-scoped registry | **Closed** (amount pricing still gated) |

---

## Bucket A — Ops only (hard live gates, not code bugs)

Must be done before live submit; phase 0 deliberately suppresses relay submit.

1. Secrets: `OMEGA_TX_SIGNING_KEY`, `OMEGA_BLUEPRINT_SIGNING_KEY`, `OMEGA_RELAY_ENDPOINT_*`, relay auth, RPC URLs, `ORCHESTRATOR_ADDRESS`
2. `config/deployment_manifest.toml` hashes/addresses match **your** deployed strategies
3. Raise `active_phase` only after (1)+(2)
4. Tune `OMEGA_KILL_*`
5. `OMEGA_VAULT_SUBMIT_NONCE` from `eth_getTransactionCount` for ZK submit path

See `OPS_CHECKLIST.md`.

---

## Bucket B — Documented residuals (quality / hardening)

| Residual | Gap |
|----------|-----|
| C3 live codehash | Off-chain compares manifest only; no live `eth_getCode` |
| C5 multi-instance | Idempotency cache process-local |
| C7 economic P&L | Inclusion success only; realized wei not yet into kill switch |
| C6 LA rescore | Reorg drain exists; position rescore open |
| CheckContext | Pyth unfed; LA competition neutral; MEV-Share not mapped; `rollout_tier` unused |
| LA debt sizing | Needs live token price for `debt_amount_wei` |

These do **not** by themselves block phase-0 shadow. They matter for production confidence.

---

## Bucket C — Hard strategy blockers (still open)

### MSA / SA capital path

- `msa.rs` / `sa.rs` still set `flashloan_provider`, `flashloan_amount`, `flashloan_token` to zero.
- `OmegaOrchestrator.execute` reverts on `flashloanToken == address(0)`.
- Shared mapper `flashloan_select::to_blueprint_provider_type` exists; **not used by MSA/SA**.
- Recommended fix: **Option B** (wire `select_provider` like LA). See `OPTION_B_MSA_SA_CAPITAL_PATH.md` and `patches/`.

### LA amount

- Real token + provider selection exist; blueprint refused until debt can be sized in wei.

---

## Corrected one-liner

**P1–P12 code gaps are closed. Live go-live still requires ops config (Bucket A). MSA/SA remain non-executable on-chain until the capital-path (Option B or a real no-flashloan product path) is implemented. C3/C5/C6/C7 and CheckContext items are residuals, not the same class of hard gate as zero flashloan fields.**
