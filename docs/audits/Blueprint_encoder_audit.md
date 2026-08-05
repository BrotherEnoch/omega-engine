# omega-engine\docs\audits\Blueprint_encoder_audit.md
# Audit: `ExecutionBlueprint` (omega-core) vs `OmegaOrchestrator.execute()` decoder

**Scope:** `crates/omega-core/src/types/blueprint.rs` cross-checked against
`OmegaOrchestrator.sol::execute()`'s `abi.decode` tuple.

**Verdict up front:** the Rust-side `max_base_fee_gwei` handling is correct and
well-tested. But this file is **not** the artifact that determines whether
signed blueprints will decode correctly on-chain — it defines the blueprint's
*domain type* and its *own* hash/idempotency commitments, not the
ABI-encoding step that turns an `ExecutionBlueprint` into `blueprintCalldata`
bytes for `execute()`. That encoder is not in this file. Until it's produced,
the highest-priority item from the previous message (field-order /
sentinel-convention match) is **still open**, just narrowed.

---

## 1. What this file actually proves

### 1.1 `max_base_fee_gwei` → `maxBaseFee` unit conversion — ✅ correct

```rust
pub fn max_base_fee_wei(&self) -> U256 {
    U256::from(self.max_base_fee_gwei) * U256::from(1_000_000_000u64)
}
```

This is the right conversion (gwei → wei, ×1e9) and is directly unit-tested:

```rust
#[test]
fn max_base_fee_wei_converts_gwei_to_wei() {
    let mut bp = sample_blueprint();
    bp.max_base_fee_gwei = 42;
    assert_eq!(bp.max_base_fee_wei(), U256::from(42_000_000_000u64));
}
```

The doc comment on `max_base_fee_gwei` explicitly calls out the failure mode
this guards against — encoding the raw gwei value instead of calling
`max_base_fee_wei()` would set an on-chain ceiling ~1e9× too low, causing
`BaseFeeTooHigh` to revert on every single call. Good: the risk is documented
at the field, not just assumed obvious.

**Action item unaffected by this file:** whatever code actually calls
`abi.encode(...)` to build `blueprintCalldata` must call `max_base_fee_wei()`,
not read `max_base_fee_gwei` directly. This file makes that correct choice
available and impossible to get right by accident — it still has to be used
correctly at the call site, which lives elsewhere.

### 1.2 No "disable the guard" sentinel exists in this file — note, not a bug

The Solidity comment says pass `type(uint256).max` to opt out of the base-fee
guard. Nothing in `ExecutionBlueprint` or `derive_max_base_fee_gwei()`
constructs that sentinel — every blueprint gets a concrete, buffer-derived
cap:

```rust
pub fn derive_max_base_fee_gwei(base_fee_at_creation: u64, buffer_factor: f64) -> u64 {
    (base_fee_at_creation as f64 * buffer_factor).ceil() as u64
}
```

This resolves half of the previously-flagged risk: there is no `0 = disabled`
convention anywhere in this struct, so the earlier concern ("what if an old
encoder still treats 0 as disabled") doesn't apply *to this file*. But it
also means the opt-out path (`type(uint256).max`) has **no producer** in
omega-core at all. If any strategy or chain is expected to run with the guard
disabled (e.g. a non-EIP-1559 chain, per the Solidity comment), that has to
be constructed somewhere outside this file, or it simply never happens today
— worth confirming which.

### 1.3 Hash/idempotency treatment of `max_base_fee_gwei` — ✅ correct and tested

- Included in `compute_hash()` (construction-time identity field) — mutating
  it fails `verify_hash()`, confirmed by
  `verify_hash_fails_after_mutating_max_base_fee_gwei`.
- Excluded from `compute_idempotency_key()` (risk parameter, not trade
  identity) — confirmed by
  `idempotency_key_unaffected_by_max_base_fee_gwei_mutation`.

This mirrors the treatment of `dynamic_min_profit` and the buffer factors,
which is the right call: re-deriving the same trade with a different fee
ceiling shouldn't produce a new idempotency key, but it should produce a
different content hash for LA/ZK purposes.

### 1.4 Rounding direction — ✅ correct

`derive_max_base_fee_gwei` rounds up (`.ceil()`), matching the reasoning
already applied to `total_l2_gas_budget()`. This is the right direction for a
*ceiling* — truncating down would silently shrink the safety margin the
buffer factor was supposed to provide. Tested:

```rust
assert_eq!(ExecutionBlueprint::derive_max_base_fee_gwei(3, 1.10), 4);
```

---

## 2. What this file does *not* prove — the actual open item

`blueprint.rs` defines the struct, its own content hash, and its own
idempotency key. **It contains no function that ABI-encodes an
`ExecutionBlueprint` into the 10-field `blueprintCalldata` tuple**
`execute()` expects:

```solidity
(uint64, uint64, bytes32, FlashloanProviderType, address, address, bytes, uint256, uint256, uint256)
//expiry_block, nonce, strategyId, providerType, flashloanToken, providerContract, strategyCalldata, flashloanAmount, minNetProfit, maxBaseFee
```

That encoder — wherever it lives (likely `omega-rpc` or a submission/relay
crate, per this file's own module comment: *"omega-strategies — not visible
from omega-core"*) — is the thing that actually determines whether signed
blueprints decode correctly on-chain. Until it's reviewed, none of the
following can be closed, only narrowed by what's shown here:

| Check | Status from this file | Still needs |
|---|---|---|
| `maxBaseFee` unit conversion | ✅ correct helper exists (`max_base_fee_wei`) | Confirm the encoder calls it, not the raw gwei field |
| `maxBaseFee` disable sentinel (`type(uint256).max`) | No 0-sentinel risk in this file | Confirm whether/where the opt-out path is ever constructed |
| Field order (10-tuple) | Not present in this file | The actual `abi.encode` call site |
| `providerType` enum ordinal (`Balancer=0/AaveV3=1/UniswapV3=2`) | Not present in this file — `StrategyId` here is a *different* enum (SA/CNRY/MSA/LA/MEV) with its own priority ordering, don't conflate the two | The actual encoder, plus confirmation it doesn't hand-roll the ordinal separately from Solidity's enum |
| Domain-separation wrapper (`keccak256(abi.encode(address(this), chainId, blueprintCalldata))`) | Not present in this file | Wherever signing happens |
| `expiry_block` semantics | ✅ matches on-chain `>` check exactly — see §3 below | — |

---

## 3. One genuine cross-file consistency finding: `expiry_block` boundary

This is worth flagging even though it's not the top-priority item, because
it's a real, verified discrepancy between two boundary conventions that both
claim to be authoritative:

- **Solidity, `OmegaOrchestrator.execute()`:**
  ```solidity
  if (block.number > expiry_block) revert BlueprintExpired(...);
  ```
  i.e. a blueprint is valid **through and including** `expiry_block`; it only
  reverts when `block.number > expiry_block`.

- **Rust, `ExecutionBlueprint::is_expired()`:**
  ```rust
  pub fn is_expired(&self, current_block: u64) -> bool {
      current_block >= self.expiry_block
  }
  ```
  i.e. a blueprint is treated as **already expired at** `expiry_block` itself.

The doc comment says this intentionally matches `omega_risk::check_expiry`
(check 2 of 13, off-chain pre-trade gate) — not the Solidity boundary. That's
fine as an internal consistency choice (off-chain risk gate and this
convenience method now agree with each other), but it means the **off-chain
system is strictly more conservative than the contract**: it will refuse to
submit a blueprint at exactly `current_block == expiry_block`, a block where
the Orchestrator would still have accepted it (`block.number > expiry_block`
is false when they're equal).

This is not a security bug — a stricter off-chain gate can only reject valid
opportunities early, never let an actually-expired blueprint through. But
it's worth being deliberate about: if `expiry_block` is being set with a tight
margin, this off-by-one is silently costing you the very last eligible block
of every blueprint's window. Confirm this is intentional slack rather than an
unnoticed mismatch.

---

## 4. Recommended next artifact

To actually close the encoder-verification item, request the file that
performs the `abi.encode` call for `blueprintCalldata` — grep for
`abi.encode` equivalents (e.g. `ethers`/`alloy` `sol!` macro usage,
`DynSolValue::Tuple`, or a hand-rolled encoder) in `omega-rpc` or wherever
signing happens. That file, not `blueprint.rs`, is where field order,
`providerType` ordinal mapping, and the domain-separation wrapper are
actually decided.