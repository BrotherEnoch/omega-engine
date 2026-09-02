# docs/Option_B_MSA_SA_Capital_Path.md (recommended)

## Decision lock

- **Do not** add Orchestrator `FlashloanProviderType.None` unless product explicitly requires self-funded execution (Option A).
- **Do** wire MSA and SA through `omega_flashloan::select_provider`, same as LA.
- Token for Arbitrum deployments that match Vault `profit_token`: **WETH** (`0x82aF49447D8a07e3bd95BD0d56f35241523fBab1`).
- Borrow size (interim): reuse existing strategy notionals already used in scoring:
  - **SA:** `SA_SPREAD_WEI` (0.2 ETH)
  - **MSA:** `route.gross_profit` from `route_profile` (0.22–0.38 ETH scale) as `flashloan_amount`

These sizes are **placeholders for capital path correctness**, not a claim of optimal sizing. Replace when a real quote/inventory path exists.

## Fail closed

- If `select_provider` fails → return `Err` from `build_blueprint` (do not emit zero token).
- Never submit `flashloan_token == Address::ZERO`.

## Files to touch

1. `crates/omega-strategies/src/msa.rs` — struct + `new` + `build_blueprint` + tests
2. `crates/omega-strategies/src/sa.rs` — same
3. `src/main.rs` — pass `Arc::clone(&liquidity_registry)` wherever MSA/SA are constructed (and test helpers)
4. Any other `MsaStrategy::new` / `SaStrategy::new` call sites (tests in-crate)

`omega-strategies` already depends on `omega-flashloan` for LA.

## Apply

See `patches/msa_option_b.patch.md`, `patches/sa_option_b.patch.md`, `patches/main_rs_wiring_notes.md`.

```bash
cargo test -p omega-strategies
cargo check --workspace
```
