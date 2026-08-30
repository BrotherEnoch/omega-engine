# docs/C9_MultiAsset_LiquidityRegistry.md (WETH + USDC)

**Status:** Baseline already on `main`; this package adds fail-closed guards on zero addresses.

## Problem

`LiquidityRegistry` was keyed by `(chain_id, provider, contract)` with **no asset**.
Aave Pool and Balancer Vault are each one contract for every token. Polling USDC
into the same key as WETH **silently overwrote** the other asset’s liquidity —
not a panic, not a staleness warning, a wrong number.

## Fix (already on main)

| Surface | Change |
|---------|--------|
| `ProviderKey` | `(chain_id, provider, **asset**, contract)` |
| `LiquidityRegistry::update` / `snapshot` / `available_contracts` | take explicit `asset` |
| `select_provider` | `select_provider(registry, chain_id, asset, amount_wei)` |
| `FlashloanError::NoneAvailable` | includes `asset: Address` |
| L2e poll (`main.rs`) | iterates `[WETH, USDC_NATIVE]`, writes per (provider, asset) |
| `LaStrategy::build_blueprint` | passes real `flashloan_token` through as `asset` |

`FlashloanLiquidityState` / CheckContext watch channel stays **WETH-only** by design
(paired with `ORACLE_SNAPSHOT_TOKEN`). LA sizing uses the registry directly.

Uniswap V3 registry rows are C10 (same pool holds both token balances).

## This package (C9b fail-closed)

1. **`update`**: refuse `asset == Address::ZERO` or `contract == Address::ZERO` — leave cache unchanged and warn.
2. **`select_provider`**: refuse zero `asset` with `NoneAvailable { best_available_wei: 0, … }`.
3. **Tests**: zero-asset update ignored, zero-contract update ignored, select rejects zero asset.

Existing tests already cover cross-asset isolation (WETH write does not satisfy USDC select; both assets coexist under one shared Aave contract).

## Apply

```bash
cp src/lib.rs   crates/omega-flashloan/src/lib.rs
cp src/tests.rs crates/omega-flashloan/src/tests.rs
cargo test -p omega-flashloan
cargo check --workspace
```

No `main.rs` changes required for C9b (L2e multi-asset wiring is already upstream).
