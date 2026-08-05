# sa_mev_patch_hunks.md
# sa.rs / mev.rs — patch hunks, not full files

Unlike `msa.rs` and `la.rs`, I have never seen the complete source for
`sa.rs` or `mev.rs` — only the `ExecutionBlueprint { ... }` construction
block for each, verified across earlier turns in this thread. Writing
full replacement files for these two would mean fabricating everything
around that block (imports, constants, scoring logic, tests) — exactly
what this thread has been careful not to do. These are patch hunks,
meant to be applied by hand at the verified insertion point in each real
file.

---

## `crates/omega-strategies/src/sa.rs`

Verified insertion point (from earlier in this thread): inside
`build_blueprint`'s `ExecutionBlueprint { ... }` literal, immediately
after `signal_id,` and before `flashloan_provider:`.

```rust
            signal_id,
            // TODO(capital-path): flashloan_provider == Address::ZERO is documented on
            // ExecutionBlueprint as "no flashloan — capital sourced from PIL (§7)", and
            // omega-execution maps zero → Ok("none") in resolve_flashloan_provider_id.
            // There is no Orchestrator branch and no strategy→PIL inventory path that
            // makes this executable on-chain (execute() reverts ZeroAddress on
            // flashloanToken == address(0); PilTreasury has deposit/redeem only, no
            // strategy loan/allocate). Either wire omega_flashloan::select_provider
            // (treat SA as incomplete flashloan strategy — Option B default) or
            // implement a real no-flashloan path as a product feature. Do not encode
            // or submit until one of those exists.
            flashloan_provider:      Address::ZERO,
            flashloan_amount:        U256::ZERO,
            flashloan_available:     U256::MAX,
            calldata,
```

(Padded-colon alignment preserved to match SA's verified style.)

---

## `crates/omega-strategies/src/mev.rs`

Verified insertion point: same position, inside `build_blueprint`'s
`ExecutionBlueprint { ... }` literal.

```rust
            signal_id,
            // TODO(capital-path): Strategy comment claims "MEV does not use flashloans."
            // Zero provider/amount matches that claim and resolve_flashloan_provider_id's
            // Ok("none") path. There is still no on-chain no-flashloan / PIL-inventory
            // execution path if this strategy ever needs borrowable capital. Do not
            // populate a non-zero flashloan_token without either select_provider wiring
            // or an explicit product decision that MEV remains self/externally funded
            // outside the Orchestrator flashloan flow.
            flashloan_provider: Address::ZERO, // MEV does not use flashloans
            flashloan_amount: U256::ZERO,
            flashloan_available: U256::ZERO,
            calldata,
```

(Compact, unpadded style preserved to match MEV's verified layout.)

---

## What would upgrade these to complete, verified files

Paste the full `sa.rs` and/or `mev.rs` (same way `msa.rs`/`la.rs` came
through) and I'll write complete files the same way — checking every
line against real source, not just the one block already confirmed,
the same standard applied to `msa.rs` and `la.rs` above.