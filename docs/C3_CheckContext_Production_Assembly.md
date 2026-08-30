# docs/C3_CheckContext_Production_Assembly.md

## Field → source map

| Field | Check | Source (this package) | Previously |
|-------|-------|----------------------|------------|
| `expected_chain_id` | 1 | `OMEGA_CHAIN_ID` / config | same |
| `current_block` | 2 | `SignalState.block_number` (oracle fee stream) | same |
| `current_l1_gas_price_gwei` | 5/6 | L2d ArbGasInfo → `PerChainOracle` → signal | same |
| `current_l2_base_fee_gwei` | 5 | fee oracle stream → signal | same |
| `l1_adaptive_buffer` | 5 | **`L1GasEma`** fed by L2d poll | `l1_adaptive_buffer(&[])` always min |
| `oracle` | 7/8/16 | Chainlink / Pyth / TWAP caches | same (Pyth still unfed upstream) |
| `flashloan` | 10 | L2e WETH MAX (Aave/Balancer/Uni V3) | same |
| `competition_probability` | 11 | **`omega_risk::competition`** on WETH tier | pinned `1.0` |
| `max_competition_probability` | 11 | **`OMEGA_MAX_COMPETITION_PROBABILITY`** (default **0.95**) | pinned `0.0` (always fail) |
| `strategy_max_gas` | 3 | `StrategyTrait::gas_budget()` | same |
| `max_slippage_bps` | 9 | `max_slippage_bps_for(strategy)` | same |
| `rollout_tier` | S19 | **`OMEGA_ROLLOUT_TIER`** (default 1.0) | pinned `0.0` |
| `strategy_bytecode_hash` | 4 | `IntegrityRegistry::snapshot` | same |
| `risk_score` / `max_risk_score` | 12 | formula: gas vol + oracle age + competition + liquidity | competition was fake |
| `current_account_exposure_wei` | 14 | `AccountExposureTracker` | same |
| `max_account_exposure_wei` | 14 | **`OMEGA_MAX_ACCOUNT_EXPOSURE_WEI`** (default 1 ETH) | hard-coded 1 ETH only |
| `latest_blueprint_nonce` | 15 | `NonceRegistry` | same |

## Remaining unavailable / limited

| Item | Notes |
|------|--------|
| **Pyth feed** | Cache exists; no ingestion path yet → ages/prices may be stale zero |
| **LA-specific competition** | Uses neutral HF 1.05 + size 0; real HF/size needs lending position scanner |
| **MEV-Share competition signal** | Stream runs; not yet mapped into competition model |
| **`rollout_tier`** | Assembled from env; **no pre-trade check reads it** yet |
| **Flashloan CheckContext asset** | Still WETH-only single scalar (registry is multi-asset for LA) |
| **`L1GasEma` cold start** | Empty history → buffer = `L1_BUFFER_MIN` until first successful ArbGasInfo poll |

## Env

| Variable | Default |
|----------|---------|
| `OMEGA_MAX_ACCOUNT_EXPOSURE_WEI` | `1e18` (1 ETH) |
| `OMEGA_MAX_COMPETITION_PROBABILITY` | `0.95` |
| `OMEGA_ROLLOUT_TIER` | `1.0` |

## Tests

`check_context_assembly_tests`:
- `build_check_context_traces_live_fields`
- `competition_for_weth_is_not_pinned_at_one`
- `empty_flashloan_maps_to_high_liquidity_risk_component`

## Apply

```bash
cp patches/main.rs src/main.rs
cargo test --bin omega-engine check_context_assembly
cargo check --workspace
```
