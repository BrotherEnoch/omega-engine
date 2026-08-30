# docs/C10_UniswapV3_FlashloanLiquidityWiring.md

**Status:** Baseline already on `main`; this package completes residual gaps  
**Scope:** L2e poll + `omega-rpc::flashloan_liq` + CheckContext WETH sanity signal

## Goals

1. Poll Uniswap V3 available liquidity for tracked assets (WETH, USDC_NATIVE)
2. Write provider rows into asset-scoped `LiquidityRegistry` (so `select_provider` can use Uni)
3. Fail closed on wrong pool / wrong asset / total read failure
4. Surface Uniswap in the pre-trade CheckContext MAX (was registry-only)

## Baseline already on main (C10)

| Piece | Location |
|-------|----------|
| Canonical pool constant | `UNISWAP_V3_WETH_USDC_POOL` = `0xC6962004f452bE9203591991D15f6b388e09E8D0` (WETH/USDC_NATIVE 0.05%, not USDC.e) |
| Read path | `OmegaRpcClient::fetch_uniswap_v3_pool_balance(pool, asset)` → `ERC20(asset).balanceOf(pool)` |
| Startup validation | C7 `validate_deployed_contracts` includes the pool (6 addresses) |
| L2e registry writes | Per tick, for each of `[WETH, USDC_NATIVE]`, update Aave + Balancer + UniswapV3 |
| `select_provider` | No change needed — already provider/asset-generic as of C9 |

### Wrong-pool trap (documented, not inventable in code alone)

Arbitrum has **two** “USDC/WETH 0.05%” Uniswap V3 pools:

- **Canonical (this codebase):** paired with Circle **USDC_NATIVE** (`0xaf88…`) — deep liquidity  
- **Wrong one:** paired with bridged **USDC.e** (`0xFF97…`) — thin; `balanceOf` still succeeds  

Bytecode presence does **not** distinguish them. The constant’s verification trail in `flashloan_liq.rs` is the control; live `balanceOf` is the ongoing signal.

## Gaps closed by this package (C10b)

### 1. CheckContext WETH MAX includes Uniswap V3

Previously L2e wrote Uni into `LiquidityRegistry` for both assets, but the
`FlashloanLiquidityState` watch channel (feeds check 10 / `MissLiquidity`) took
MAX over **Aave and Balancer only**.

**Now:** for WETH, MAX over Aave, Balancer, and Uniswap V3. Protocol id label is
whichever won (`"aave"` / `"balancer"` / `"uniswap_v3"`).

USDC still does not drive the single-scalar CheckContext channel (C9 design:
paired with `ORACLE_SNAPSHOT_TOKEN` = WETH). LA sizing continues to use
`select_provider` + registry directly.

### 2. Fail closed when all three WETH reads fail

If Aave, Balancer, **and** Uniswap all error, the watch channel is **not**
updated (keep previous value). Publishing a synthetic zero would look like a
successful empty measurement.

### 3. Asset allowlist on the canonical pool

`fetch_uniswap_v3_pool_balance(UNISWAP_V3_WETH_USDC_POOL, asset)` rejects any
`asset` other than `WETH` or `USDC_NATIVE` **before** the eth_call. Prevents a
silent wrong-token `balanceOf` against the canonical pool address.

## Apply

```bash
# From repo root after unpacking this package:
cp patches/flashloan_liq.rs crates/omega-rpc/src/flashloan_liq.rs
# main.rs is large — prefer applying the L2e candidate-selection hunk by hand
# or: cp patches/main.rs src/main.rs  (only if your tree matches the fetched baseline)

cargo check -p omega-rpc
cargo check --bin <your-binary>   # or workspace build
cargo test -p omega-rpc
```

## Files

| Path | Role |
|------|------|
| `docs/C10_UniswapV3_LiquidityWiring.md` | This note |
| `patches/flashloan_liq.rs` | Full module with C10b allowlist |
| `patches/main.rs` | Full entrypoint with Uni in WETH MAX + changelog |

## Out of scope

- Making `CheckContext.flashloan` multi-asset (USDC)
- On-chain `token0()`/`token1()` confirmation of pool composition at startup
- Uniswap flashloan **encoding** / callback path beyond liquidity availability
- Changing premiums in `omega-flashloan` (Uni remains 30 bps last resort)
