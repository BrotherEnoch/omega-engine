# PositionDataInjectionDesign.md
# Position Data Injection Design — Problem 2

**Status: proposal, not implementation.** No strategy in this codebase
currently receives any live data source beyond `SignalState` and static
config (verified directly: every `fn new(...)` across `sa.rs`, `msa.rs`,
`la.rs`, `mev.rs`, `cnry.rs` takes only `chain_id`, bytecode/contract
metadata, and `&OmegaConfig`). There is no existing convention to follow
here — this document proposes one. Nothing below should be read as
"how the system already works."

## What's actually blocking

Two real gaps, both resolved by the same missing piece:

1. **LA's debt sizing** — `LA_PROXY_DEBT_WEI` is a fixed constant, not a
   real position's debt.
2. **LA's `flashloan_token`** — no source for which ERC20 the debt is
   denominated in (blocking Problem 1's full completion, per the guard
   just added to `la.rs`).

Both need the same thing: a real `PositionSnapshot` for a real
liquidatable position, with more than what `PositionSnapshot` currently
carries (`debt_usd_e18` is a USD value; token-wei sizing needs a token
amount and a token address).

## Two separate design questions, not one

### Question A: how does a strategy *get* position data at all?

No mechanism exists. Proposed shape, minimal and consistent with the
existing constructor-injection style already used for `LiquidityRegistry`
in this session's `la.rs` change:

```rust
pub struct LaStrategy {
    // ... existing fields ...
    position_book: Arc<PositionBook>,
}
```

Where `PositionBook` would be a new, genuinely-designed type (not the
one described in the fabricated conversation two turns ago — that
description should be discarded entirely, not treated as a starting
point, since it was never real and its exact shape was never verified
against anything). A minimal real design:

```rust
// crates/omega-core/src/types/position_book.rs — NEW, not yet written
//
// In-memory index of currently-liquidatable positions, keyed by
// (borrower, protocol) per PositionSnapshot::dedup_key(). Read by
// strategies; written by whatever produces PositionSnapshots (see
// Question C below — that producer does not exist yet either).

pub struct PositionBook {
    // Implementation TBD — a DashMap<String, PositionSnapshot> keyed by
    // dedup_key() would match the "sequencer restart deduplication"
    // design already documented on PositionSnapshot::dedup_key() itself
    // (verified real doc comment, oracle.rs), but this is a proposal,
    // not a decision — concurrency requirements (single-threaded scoring
    // vs. multi-threaded ingestion) haven't been specified anywhere.
}
```

This alone is implementable without fabrication — `PositionSnapshot`,
`LaTier`, `dedup_key()` are all real, verified types from `oracle.rs`.
What's genuinely new is the container and the injection wiring.

### Question B: where does token-wei debt sizing come from?

`PositionSnapshot` needs either:

- **A new field**, `debt_token: Address` (+ possibly `debt_token_wei:
  U256` if USD→wei conversion shouldn't happen at read time), or
- **A conversion path** using `OraclePrice` (real, verified,
  `oracle.rs`) — `debt_token_wei = debt_usd_e18 * 10^decimals /
  price.price_usd_e18` — which requires the strategy to also have live
  price access for the debt token, a *third* injected dependency
  (`Arc<PriceOracle>` or similar, also not yet designed).

Recommend the field addition over the conversion path: conversion
requires trusting a price feed at blueprint-build time in addition to
the position data itself, doubling the staleness/manipulation surface
for a value that's about to be flash-borrowed on-chain. If the position
producer (Question C) already has to observe the debt token to construct
`PositionSnapshot` in the first place (Aave's `getUserAccountData` and
similar surface the debt asset directly), carrying it as a field costs
nothing extra and avoids a second live price dependency inside the
strategy.

### Question C: who produces `PositionSnapshot`s?

**Completely unaddressed, in this proposal or anywhere else in this
thread.** `PositionSnapshot`'s own doc comments (verified, `oracle.rs`)
describe it as "produced by omega-oracle from on-chain position data"
— but no `omega-oracle` crate or module has appeared anywhere in this
conversation. Confirm whether it exists before treating Questions A/B as
the critical path; if there's no producer, a `PositionBook` with a
correct API and no writer doesn't unblock LA at all; it just moves the
"still fake" boundary one layer over.

```powershell
# Does omega-oracle exist as a crate, or is it aspirational like PIL was?
Get-ChildItem . -Recurse -Filter "Cargo.toml" | Select-String "omega-oracle"
Get-ChildItem . -Recurse -Directory -Filter "omega-oracle"
```

## Recommended sequencing

1. **Run the Question C check first.** Cheap, and it determines whether
   this is "wire an existing producer" or "design a producer from
   scratch" — a very different scope.
2. If a producer exists: design `PositionBook` (Question A) as the
   consumer-side container, add `debt_token` to `PositionSnapshot`
   (Question B), wire LA's constructor and `debt_token()` (already
   stubbed as `None` in the current `la.rs`) to read from it.
3. If no producer exists: this becomes a larger scoping conversation
   (does LA even ship in this phase, or does it wait on omega-oracle
   being built) — not something to sketch further without knowing that
   answer.

## What NOT to carry forward from the fabricated conversation

For the record, since it was detailed enough to be easy to
half-remember as real: the earlier description of `PositionBook` with
`upsert`/`remove`/`liquidatable`/`best_liquidatable` methods, and a
drop-in `la.rs` with a `debt_token_wei` guard, never existed on disk.
Nothing in this document assumes any part of that shape is correct — the
container sketch above is a fresh minimal proposal, not a recovery of
that description.