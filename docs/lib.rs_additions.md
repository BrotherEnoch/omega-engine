# lib.rs_additions.md
# Required `lib.rs` additions (not full files — neither has been seen)

## `crates/omega-rpc/src/lib.rs`

Add a module declaration and re-export for `chainlink_agg.rs`:

```rust
mod chainlink_agg; // or `pub mod` if AggregatorV3Interface itself needs
                    // to be reachable from outside omega-rpc — as written,
                    // only ChainlinkRound and OmegaRpcClient::fetch_chainlink_round
                    // are used externally (by omega-oracle/chainlink_poll.rs),
                    // so `pub use` below is the only export actually needed.

pub use chainlink_agg::ChainlinkRound;
```

`OmegaRpcClient` itself is presumably already `pub use`d from `client.rs` (unverified — inferred from `main.rs`'s existing `use omega_rpc::{..., OmegaRpcClient, RpcClientConfig};`), so `fetch_chainlink_round` being a method on it needs no separate export.

## `crates/omega-oracle/src/lib.rs`

Add a module declaration for `chainlink_poll.rs`:

```rust
pub mod chainlink_poll;
```

`chainlink_poll.rs` references `crate::chainlink::{arbitrum_feeds, ChainlinkOracle}` — confirmed both are real, already-`pub` items in `chainlink.rs` (verified: `arbitrum_feeds()` and `ChainlinkOracle` both appeared in this session's earlier `pub fn`/struct greps), so no additional visibility changes needed there.

## Why these can't be written as complete files

Neither `lib.rs` has been pasted in this session in full — only re-export lines have been seen for `omega-core`'s `types/mod.rs` (a different file, fully verified) and inferred fragments for these two crates from `use` statements in `main.rs` and doc comments. Writing a "complete" `lib.rs` for either crate would mean guessing at every other module declaration and re-export already present — exactly the fabrication risk this session has repeatedly caught. These two additions are the minimum needed for the new code to be reachable; apply them to the real files by hand.