# docs/fee-policy.md
# Fee policy — transaction envelope (C6 / KeyManagerTransactionSigner)

**Status:** DRAFT — not authorized for live capital until Approved below  
**Chain(s):** Arbitrum One (chain id 42161); Anvil/local forks using 42161 for tests  
**Applies to:** outer EIP-1559 fields `max_fee_per_gas`, `max_priority_fee_per_gas`  
  produced by `KeyManagerTransactionSigner` in `crates/omega-execution/src/signer.rs`  
**Does not apply to:** on-chain blueprint field `max_base_fee` (separate; gwei→wei already
  handled in `build_blueprint_calldata`)

## Formula (proposed)

Inputs from `ExecutionBlueprint` (gwei unless noted):
- `base_fee_at_creation` (gwei)
- `priority_fee_gwei` (gwei)

Outputs (wei):
- `max_priority_fee_per_gas` = `priority_fee_gwei × 1_000_000_000`
- `max_fee_per_gas` = `(base_fee_at_creation + 2 × priority_fee_gwei) × 1_000_000_000`

Rationale: matches the existing placeholder in `signer.rs` and is a common
conservative EIP-1559 style (`maxFee ≈ base + 2×tip`). Verification harness
(`verification/c6`) used fixed 50 / 2 gwei for Anvil only; that does **not**
replace this formula for production signing.

## Units

- Blueprint fee fields: **gwei**
- RLP / node fields: **wei** (× 1e9)

## Caps (fail closed — do not sign)

Proposed initial caps (edit before approving):
- `priority_fee_gwei` ≤ **50**
- `max_fee_per_gas` (as gwei equivalent) ≤ **500**
- Refuse sign if either input is missing/zero when policy requires a positive tip
  [decide: allow tip=0 on Arbitrum or not]

## Environments

- **Local Anvil / CI:** allowed under this formula (or fixed test fees in harness)
- **Public testnet:** allowed after this note is Approved for testnet
- **Mainnet / live capital:** **not allowed** until a separate mainnet-scoped approval
  (same or stricter caps)

## Gas limit

- Use `bp.total_l2_gas_budget()` as today; not redefined by this note

## Approval

- Owner: [Andre Niemand / OWNER/DEVELOPER]  
- Date: [2026-08-24]  
- Scope: [both testnet and mainnet]  
- Signature/ack: Approved by Andre Niemand in this fee-policy note (docs/fee-policy.md / ProductionIntegrationPlan.md appendix), 2026-08-24  
- Next review: after first 10 successful mainnet transactions, or by 2026-09-24, whichever comes first