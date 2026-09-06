ProductionIntegrationPlan.md
stage7_fix
Files
ProductionIntegrationPlan.md
Production Integration Plan — Shadow Mode → Live Execution
Status: living status document (updated 2026-09-05).
Original twelve-gap inventory is retained for history; each gap is marked CLOSED, PARTIAL, or OPEN against the current codebase.

1. Where things stand (current)
src/main.rs constructs a real ExecutionPipeline and calls ExecutionPipeline::execute from score_and_admit (Gap 8 CLOSED).

KeyManagerTransactionSigner is the production TransactionSigner(Gap 1 CLOSED for software keys; HSM remains an operator choice).

Real relay bootstrap from OMEGA_RELAY_ENDPOINT_* / auth env vars (Gap 2 CLOSED); phase ≥ 1 with zero relays fails closed.

Deployment manifest → IntegrityRegistry (Gap 6 PARTIAL — loader and Stage 2b exist; operator must supply a real manifest file).

Flashloan provider table + L2e liquidity polls (Gap 7 CLOSED for Aave/Balancer/Uniswap V3 on Arbitrum One).

Integrity check wired into the pipeline (Gap 9 CLOSED).

Stage-7 confirmation carries strategy_id, nonce, expected_profit_net_wei on ConfirmationResult (Gap 10 CLOSEDfor metadata + kill-switch + NonceRegistry wiring).

Idempotency eviction loop runs in main (Gap 11 CLOSED).

Kill-switch thresholds from env (Gap 5 PARTIAL — mechanical wiring done; operator must set production numbers).

2. Gap inventory (updated)
Gap 1 — Transaction Signing — CLOSED (software path)
KeyManagerTransactionSigner implements TransactionSigner with EIP-1559 / Arbitrum signing via KeyManager. HSM/KMS backend remains an operator deployment choice, not a missing interface.

Gap 2 — Relay Client Construction — CLOSED
HttpRelayClient is constructed in production from env endpoints/secrets. Missing endpoint/secret → relay skipped; phase ≥ 1 with zero clients refuses startup.

Gap 3 — Deployment Configuration / Secrets — OPEN (operator)
Secrets strategy (file vs env vs vault) is an operator decision. Code reads env vars only; never invents credentials.

Gap 4 — Config Translation Layer — CLOSED
omega-execution::config_translation::translate_relay_config maps OmegaConfig + secrets into omega_relay config.

Gap 5 — Kill Switch Bootstrap — PARTIAL
KillSwitchConfig is loaded from OMEGA_KILL_* env with tight defaults. Operator must review thresholds before mainnet capital.

Gap 6 — IntegrityRegistry Deployment Manifest — PARTIAL
Loader + strategy_entries_from_manifest + Stage 2b fail-closed behavior exist. A real 5-entry manifest (CNRY/SA/MSA/LA/MEV) must be supplied for full strategy registration.

Gap 7 — Flashloan Provider Resolution — CLOSED (Arbitrum One)
Address table + L2e polls for Aave V3, Balancer V2, Uniswap V3 WETH/USDC. Other chains need explicit sign-off.

Gap 8 — main.rs Integration — CLOSED
ExecutionPipeline is constructed and invoked from score_and_admit.

Gap 9 — Bytecode Integrity Check in Pipeline — CLOSED
Integrity registry is a pipeline dependency; unknown / wrong hash / frozen strategies fail closed.

Gap 10 — Confirmation Reconciliation Wiring (Stage 7) — CLOSED
BundlePayload carries local metadata (strategy_id, nonce, expected_profit_net_wei, skip_serializing so relays never see them). InclusionTracker copies them into PendingBundle → ConfirmationResult. main reconciliation loop:

kill switch: per-strategy scope (fallback "global" only if empty);

profit: expected net on inclusion; optional miss-as-loss via OMEGA_KS_COUNT_MISSED_PROFIT=1;

NonceRegistry::record_processed(strategy, chain, nonce) on inclusion.

True on-chain realized P&L still requires a Vault/receipt oracle (residual).

Gap 11 — Idempotency Cache Eviction — CLOSED
run_idempotency_eviction_loop spawned from main.

Gap 12 — LaReorgRiskEvent Receiver — OPEN / residual
Search for a dedicated consumer remains incomplete. Reorg guard and confirmation grace window exist; a first-class event drain may still be needed depending on operator policy.

3. Residual financial-risk items (not the original twelve)
Item

Status

Multi-strategy L13 registration (SA/MSA/MEV)

Depends on manifest entries (Gap 6)

PositionRegistry live writer for LA

Requires operator watchlist / scanner

Durable exposure / nonce across restart

Exposure snapshot optional; nonce still process-local without on-chain sync

On-chain realized P&L into kill switch

Expected profit used as proxy until Vault P&L oracle exists

Uniswap V3 full strategy path beyond liquidity poll

Liquidity poll exists; strategy-level route pricing may still be incomplete

4. Operator checklist before raising active_phase
Set secrets / relay endpoints / signing keys (Gap 3).

Set OMEGA_KILL_* and OMEGA_MAX_ACCOUNT_EXPOSURE_WEI (Gap 5).

Install real deployment manifest for IntegrityRegistry (Gap 6).

Confirm Arbitrum contract addresses still match (Gap 7).

Optionally set OMEGA_KS_COUNT_MISSED_PROFIT and OMEGA_EXPOSURE_SNAPSHOT_PATH.

Run cargo test --workspace and ops checklist before phase promotion.

5. Historical note
The original document described a state where score_and_admit stopped at tracing::info! and omega-execution was unwired. That is no longer true. Do not treat sections that claimed “zero production call sites” for relays/signers as current without re-verifying against src/main.rs.