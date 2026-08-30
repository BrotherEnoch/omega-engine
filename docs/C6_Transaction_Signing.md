# C6_Transaction_Signing.md

**Prerequisite:** authoritative OmegaOrchestrator ABI (now available in-repo as
`contracts/src/OmegaOrchestrator.sol`).

## Status

C6 is **implemented on main** in `crates/omega-execution/src/signer.rs` and wired
from `src/main.rs`. This package documents the integration and adds residual
**fail-closed** pre-flight checks.

## Requirements → where they live

| Requirement | Implementation |
|-------------|----------------|
| Complete calldata encoder | `build_blueprint_calldata` via `abi_encode_params()` (flat tuple matching `abi.decode` on Orchestrator); `encode_execute_call` for outer `execute(bytes,bytes)` |
| Blueprint → calldata mapping | Field order/types transcribed from Orchestrator; flashloan enum ordinals golden-tested; gwei→wei for `maxBaseFee` |
| Integrate KeyManager | `KeyManagerTransactionSigner`: **tx envelope** key (`OMEGA_TX_SIGNING_KEY`) + **blueprint auth** key via `BlueprintSigner` (`OMEGA_BLUEPRINT_SIGNING_KEY`) — deliberately separate |
| Test signing | solc golden vector, round-trip decode, end-to-end `sign_transaction`, strategy lookup failures, zero orchestrator panic |
| Fail closed | Unconfigured strategy; no active key; fee-cap violations; **this package:** zero `chain_id`, zero gas budget, all-zero strategyId mapping |

## Pipeline (signing)

```
ExecutionBlueprint
  → validate_blueprint_for_signing(chain_id)     // C6 pre-flight
  → build_blueprint_calldata (ABI flat tuple)
  → bp_hash = keccak(orchestrator ‖ chain_id ‖ blueprintCalldata)
  → BlueprintSigner.sign_raw_hash(bp_hash)       // authorization sig
  → encode_execute_call(blueprintCalldata, sig)
  → EIP-1559 RLP (unsigned) → keccak → secp256k1 (tx key)
  → SignedTransaction { raw_tx_hex }
```

`to` is always the configured OmegaOrchestrator address (never zero — panics at construction).

## Fail-closed matrix

| Condition | Behavior |
|-----------|----------|
| `orchestrator == Address::ZERO` | Panic at `new` |
| Unknown strategy_id | `SigningFailed` |
| Mapped strategyId all-zero | `SigningFailed` (C6b) |
| `chain_id == 0` | `SigningFailed` (C6b) |
| `total_l2_gas_budget() == 0` | `SigningFailed` (C6b) |
| No active tx key | `SigningFailed` |
| Priority/base fee above policy cap | `SigningFailed` (before RLP) |
| `UnconfiguredSigner` | Always `NoTransactionSigner` |

## main.rs env

- `ORCHESTRATOR_ADDRESS`
- `OMEGA_TX_SIGNING_KEY`
- `OMEGA_BLUEPRINT_SIGNING_KEY`
- `strategy_onchain_ids()` from StrategyIds.sol constants

## Apply

```bash
cp src/signer.rs crates/omega-execution/src/signer.rs
cargo test -p omega-execution
cargo check --workspace
```
