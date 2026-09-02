# README.md
# Omega-Engine production residual fixes (sandbox deliverable)

Copied here so you can download and apply locally against
`https://github.com/BrotherEnoch/omega-engine` `main`.

## Contents

| File | Purpose |
|------|---------|
| `PRODUCTION_BLOCKERS_STATUS_FIXED.md` | Corrected residual status (ops / residuals / hard blockers) |
| `OPS_CHECKLIST.md` | Operator-only gates before raising `active_phase` |
| `OPTION_B_MSA_SA_CAPITAL_PATH.md` | Design decision lock + apply steps for MSA/SA |
| `patches/msa_option_b.patch.md` | Concrete Rust edits for `crates/omega-strategies/src/msa.rs` |
| `patches/sa_option_b.patch.md` | Concrete Rust edits for `crates/omega-strategies/src/sa.rs` |
| `patches/main_rs_wiring_notes.md` | How to thread `LiquidityRegistry` into MSA/SA registration |
| `*.orig` | Baseline sources downloaded from `main` at generation time |

## What this does **not** claim

- These are **not** auto-merged commits. Apply and `cargo test` / `cargo check` yourself.
- MSA/SA sizing uses **WETH** + existing notional constants (`MSA_*_NOTIONAL_WEI`, `SA_SPREAD_WEI`) as the borrow amount — same order of magnitude the strategies already score against. Replace with route-derived sizing when you have a real quote path.
- Orchestrator still requires `flashloanToken != address(0)` and token match with Vault `profit_token` (WETH on Arbitrum deployments that follow the existing design).
- C3 live codehash, C7 realized P&L, Pyth ingestion, etc. are **not** implemented in this package (see status doc).

## Quick apply order

1. Read `PRODUCTION_BLOCKERS_STATUS_FIXED.md` and `OPS_CHECKLIST.md`.
2. Apply Option B patches to `msa.rs` / `sa.rs` (and update tests’ `::new` call sites).
3. Wire constructors from `main.rs` (and any registry builder) with `Arc::clone(&liquidity_registry)`.
4. `cargo test -p omega-strategies`
5. `cargo check --workspace`
6. Ops: secrets, real manifest, then raise `active_phase`.
