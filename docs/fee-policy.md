# docs/fee-policy.md
# Fee policy — transaction envelope (C6 / KeyManagerTransactionSigner)

**Status:** APPROVED for Arbitrum One (chain id 42161) and Anvil/local forks using 42161  
**Not approved for:** any other chain without a new, explicit sign-off in this document  
**Owner:** Andre Niemand  
**Approved:** 2026-08-25 (see Approval section below)  
**Next review:** after the first 10 successful mainnet transactions, or by 2026-09-24, whichever comes first  

**Chain(s):** Arbitrum One (42161); Anvil/local forks using 42161 for tests  

**Applies to:** outer EIP-1559 fields `max_fee_per_gas` and `max_priority_fee_per_gas`  
produced by `KeyManagerTransactionSigner` in `crates/omega-execution/src/signer.rs`  

**Does not apply to:** on-chain blueprint field `max_base_fee` (separate; gwei→wei is already  
handled in `build_blueprint_calldata`)

---

## Formula (approved)

Inputs from `ExecutionBlueprint` (gwei unless noted):

- `base_fee_at_creation` (gwei)
- `priority_fee_gwei` (gwei)

Outputs (wei):

- `max_priority_fee_per_gas` = `priority_fee_gwei × 1_000_000_000`
- `max_fee_per_gas` = `(base_fee_at_creation + 2 × priority_fee_gwei) × 1_000_000_000`

Rationale: this is the formula already implemented in `signer.rs` and is a common  
EIP-1559 style (`maxFee ≈ base + 2×tip`). The multiplier is on the **tip**, not the base  
fee. An alternate form (`2 × base + tip`) is more aggressive about base-fee spikes; it is  
**not** authorized by this note. Changing the placement of the ×2 requires a new sign-off.

Verification harnesses (e.g. `verification/c6`) may use fixed Anvil fees for local  
checks; those fixed values do **not** replace this formula for production or testnet  
signing through `KeyManagerTransactionSigner`.

---

## Units

- Blueprint fee fields: **gwei**
- RLP / node fields: **wei** (× 1e9)

---

## Caps (fail closed — do not sign)

Checked **before** any key material is used. Exceeding a cap returns  
`ExecutionError::SigningFailed` naming the cap; values are never silently clamped.

- `priority_fee_gwei` ≤ **50**
- `max_fee_per_gas` expressed in gwei  
  (`base_fee_at_creation + 2 × priority_fee_gwei`) ≤ **500**
- `priority_fee_gwei == 0` is **allowed** on Arbitrum One (sequencer inclusion does not  
  require a tip). A zero tip still produces  
  `max_fee_per_gas = base_fee_at_creation × 1e9` and remains subject to the 500 gwei  
  max-fee cap.

---

## Environments

- **Local Anvil / CI:** allowed under this formula (or fixed test fees in a harness)
- **Public testnet (Arbitrum-family, chain id 42161 semantics):** allowed under this note
- **Mainnet / live capital (Arbitrum One 42161):** allowed under this note with the same  
  formula and caps; next review still applies as stated above
- **Any other chain:** not allowed until this document is updated and re-approved for that  
  chain id

---

## Gas limit

- Use `bp.total_l2_gas_budget()` as today; not redefined by this note

---

## Explicitly out of scope (still open elsewhere)

This note does **not** authorize or implement:

- Re-reading base fee at sign/submit time (refresh-at-sign)
- A profit-vs-gas-cost guard beyond existing pre-trade checks
- Per-relay or builder-specific tip overrides
- Non-42161 fee markets

Those remain separate work items and do not reopen the formula or caps above.

---

## Approval

- **Owner:** Andre Niemand  
- **Date:** 2026-08-25  
- **Scope:** Arbitrum One (42161) and Anvil/local forks of 42161 — testnet and mainnet
- **Formula confirmed as-is, or changed?:** as-is
- **Caps confirmed as-is, or changed?:** as-is
- **Verification artifact (commit hash / PR link / other):** commit hash
- **Signature/ack:** Approved by Andre Niemand in this fee-policy note  
  (`docs/fee-policy.md` / ProductionIntegrationPlan.md thread), 2026-08-25  
- **Next review:** after the first 10 successful mainnet transactions, or by 2026-09-25,  
  whichever comes first