# ExecutionBlueprint.md
# `ExecutionBlueprint` field addition — patch for `blueprint.rs`

**Confidence basis, stated plainly:** the struct *declaration* in
`blueprint.rs` has never been pasted in this thread. But its field
*list and order* can now be read with high confidence from four
independently-written, fully-verified real struct literals
(`sa.rs`, `msa.rs`, `la.rs`, `mev.rs`) that all construct
`ExecutionBlueprint { ... }` with the identical field sequence:

```
blueprint_hash, chain_id, strategy_id, lane, simulator, signal_state_hash,
state_version, signal_id, flashloan_provider, flashloan_amount,
flashloan_available, calldata, strategy_bytecode_hash,
l2_exec_gas_estimate, l1_data_gas_estimate, extraction_gas,
expected_profit_net, dynamic_min_profit, l2_buffer_factor,
l1_data_buffer_factor, slippage_bps, base_fee_at_creation,
l1_data_fee_at_creation, priority_fee_gwei, price_impact_bps,
ofa_compliant, expiry_block, nonce, confirmation_depth,
client_order_id, idempotency_key, relay_targets, zk_proof_commitment
```

Four independent authors matching field-for-field, order-for-order is
strong corroborating evidence — categorically different from the single
prose description that produced the fabricated base-fee guard earlier in
this thread. Still: **paste the actual `pub struct ExecutionBlueprint`
block to confirm types/derives before merging this.** The list above is
inferred from usage, not read from a declaration.

## Patch

Insert three new fields immediately after `flashloan_available`, before
`calldata` — this groups all flashloan-related fields together, matching
the existing convention of `flashloan_provider`/`flashloan_amount`/
`flashloan_available` being adjacent:

```rust
    pub flashloan_provider: Address,
    pub flashloan_amount: U256,
    pub flashloan_available: U256,
    // NEW — see crates/omega-strategies/src/flashloan_select.rs for the
    // off-chain -> on-chain mapping. Populated by build_blueprint via
    // omega_flashloan::select_provider. Chain-independent default when
    // unset: FlashloanProviderType::Balancer / Address::ZERO — matches
    // the Orchestrator's own ZeroAddress() revert path, so an unset
    // blueprint fails the same way it already does today, not a new
    // failure mode.
    pub flashloan_provider_type: crate::types::flashloan_provider::FlashloanProviderType,
    pub provider_contract: Address,
    pub flashloan_token: Address,
    pub calldata: Bytes,
```

## Hash / idempotency inclusion

Per the precedent already applied and verified two turns ago (the real
`compute_hash`/`compute_idempotency_key` diff you pasted), include all
three new fields in both — same reasoning: provider/pool/token selection
changes what executes on-chain, so it's trade identity, not a pure risk
parameter like `max_base_fee_gwei` was.