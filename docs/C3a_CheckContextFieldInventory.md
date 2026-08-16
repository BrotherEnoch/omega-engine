# C3a_CheckContextFieldInventory.md
# C3a — CheckContext Field Inventory (updated after direct source review)

Status of each of `omega_risk::context::CheckContext`'s 17 fields, after
reading `config.rs`, `competition.rs`, `gas_model.rs`, `whitelist.rs`,
`replay.rs`, `per_chain.rs`, and the `omega-flashloan` crate directly.
This supersedes the original C3a inventory's "likely live, unread" rows
with confirmed findings — several turned out real and are now wired;
one turned out to be a placeholder hardcoded inside the oracle itself,
not the ambiguous case it first looked like.

| Field | Status | Evidence |
|---|---|---|
| `expected_chain_id`, `current_block`, `current_l2_base_fee_gwei`, `oracle` | **Live** | Wired since C2. |
| `strategy_max_gas` | **Live (new)** | `StrategyTrait::gas_budget()` — each strategy's own real constant (confirmed in `sa.rs`/`msa.rs`/`mev.rs`). Wired via `score_and_admit`, which already holds the strategy object. |
| `max_slippage_bps` | **Live (new)** | `context.rs`'s real `MAX_SLIPPAGE_BPS_SA`/`MSA`/`LA`/`MEV` constants, selected via `strategy.strategy_id()`. **Finding, not resolved by this wiring**: `sa.rs`'s own `SA_SLIPPAGE_BPS` (50) exceeds `MAX_SLIPPAGE_BPS_SA` (30) — a real, latent inconsistency between two independently-set constants. `msa.rs` (40 vs. cap 50) and `mev.rs` (30 vs. cap 30) don't have this problem. Needs a product/spec decision on which value is wrong; not something main.rs can resolve. |
| `l1_adaptive_buffer` | **Live (correctly empty)** | Calls the real `omega_risk::gas_model::l1_adaptive_buffer(&[])` directly — returns the real `L1_BUFFER_MIN` (1.30) constant for empty history, rather than an invented 0.0. Not read by any check in `checks.rs`, so this is about correctness, not gating. |
| `latest_blueprint_nonce` | **Live (partial)** | Real `omega_security::replay::NonceRegistry`, constructed empty in `main()` (C1-style: real object, no ingestion yet). **Caveat**: nothing calls `.advance()` after confirmation, so it stays frozen at 0 — check 15 only rejects each strategy's very first blueprint (nonce 0), not later ones. Safe today only because check 4 (below) still blocks everything first. |
| `current_l1_gas_price_gwei` | **Confirmed NOT live** (was "ambiguous") | `per_chain.rs`'s `run_fee_oracle` hardcodes `l1_data_fee_gwei: 0, // populated by ArbGasInfo` — the apparent source (`signal.l1_data_fee_gwei`) is itself an unimplemented placeholder inside the oracle, not real data. Remains fail-closed (`u64::MAX`). |
| `flashloan` (`available`, `protocol_id`) | **Confirmed blocked, not just missing** | `omega_flashloan::LiquidityRegistry` is real, live, and tested — but blocked on two independent gaps: (1) nothing feeds it (no ingestion wired from oracle streams), (2) no address→provider mapping exists to resolve which `FlashloanProvider` to query for a blueprint's raw `Address`. Moot today regardless — every strategy read (`sa`/`msa`/`mev`) sets `flashloan_provider: Address::ZERO`. Remains fail-closed. |
| `competition_probability`, `max_competition_probability` | **Confirmed blocked** | `omega_risk::competition` is real and matches `context.rs`'s own doc comment. But its inputs (asset tier, health factor, liquidation size) aren't available from `SignalState`, and the underlying health-factor ingestion (`per_chain.rs`'s `run_lending_protocol`) itself hardcodes `"hf_e18": "0"` — same placeholder-inside-the-oracle pattern as the L1 gas price finding. `max_competition_probability` confirmed absent from `config.rs`. Remains fail-closed. |
| `strategy_bytecode_hash` (CheckContext-level) | **Confirmed blocked on a real API gap** | `omega_risk::whitelist::BytecodeWhitelist` is real, but only exposes `is_approved(id, candidate) -> bool` — a membership test, not an accessor for the stored expected hash that check 4's direct value comparison needs. Would require a new method on `BytecodeWhitelist` itself (outside main.rs's scope). Remains fail-closed `[0u8; 32]` — this is now confirmed as the deterministic backstop every other real-but-partial field above relies on. |
| `max_account_exposure_wei` | **Confirmed NOT the field I thought** | `config.rs`'s `VaultConfig.per_transfer_cap_wei`/`daily_cap_wei` are real, but cap *profit released from the Vault*, not *capital at risk* — a different concept. No genuine exposure-cap source found. Remains fail-closed. |
| `current_account_exposure_wei` | **Missing** | No live tracker found anywhere. C3b (new subsystem). |
| `risk_score`, `max_risk_score` | **Missing** | No composite scoring function found. C3b. |
| `rollout_tier` | **Missing** | No config field, no consumer in `checks.rs`. |

## Summary

- **7 fields real and live**: the original 4 (C2) plus `strategy_max_gas`, `max_slippage_bps`, `l1_adaptive_buffer` (new this pass).
- **1 field real but partial, safe today**: `latest_blueprint_nonce`.
- **1 real finding requiring a product decision, not a wiring fix**: SA's slippage constant exceeds its own policy cap.
- **6 fields confirmed blocked or missing**, each with a specific, sourced reason rather than a general "nothing found": `current_l1_gas_price_gwei`, `flashloan`, `competition_probability`/`max_competition_probability`, `strategy_bytecode_hash`, `max_account_exposure_wei`/`current_account_exposure_wei`, `risk_score`/`max_risk_score`, `rollout_tier`.
- **Deterministic fail-closed backstop unchanged**: check 4 (`strategy_bytecode_hash`) still blocks every real blueprint before any of the newly-wired fields' behavior would matter, since it comes earlier in fast-fail order than checks 9/15.

## Recurring pattern worth naming

Three separate fields this pass (`current_l1_gas_price_gwei`, `flashloan`, `competition_probability`) turned out to have a *real, tested computation function* sitting on top of an *unimplemented data source* — `run_fee_oracle`'s `l1_data_fee_gwei: 0`, no liquidity ingestion, `run_lending_protocol`'s `"hf_e18": "0"`. The function existing is not the same as the field being wireable. Worth checking for this same shape before assuming any future "likely live" lead is actually usable.