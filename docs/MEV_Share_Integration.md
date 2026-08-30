# docs/MEV_Share_Integration.md

## Problem

`run_mev_share_stream` published to a broadcast channel whose only receiver was
`_mev_rx` (immediately dropped). Events were parsed as opaque JSON and never
reached risk / CheckContext.

## Solution

1. **Typed parse** (`MevShareEvent::from_payload`) — extracts `hash`, `has_txs`, `has_logs`
2. **Live consumer** — holds `mev_rx` for process lifetime; records competition-indicating events
3. **`MevShareActivityTracker`** — 30s sliding window of event timestamps
4. **`competition_with_mev_share`** — bumps tier-based probability (+0.02/event, max +0.25)
5. **CheckContext check 11** — uses blended probability at `build_check_context` time

## Flow

```
Flashbots SSE → run_mev_share_stream → broadcast
                    → consumer → MevShareActivityTracker
score_and_admit → competition_probability_for_primary_asset(events_in_window)
                    → CheckContext.competition_probability
```

## Heuristic

`indicates_competition()` is true if the event has a hash, non-empty `txs`, or non-empty `logs`.

## Apply

```bash
cp src/subscriptions.rs crates/omega-rpc/src/subscriptions.rs
cp src/competition.rs   crates/omega-risk/src/competition.rs
cp src/lib.rs           crates/omega-risk/src/lib.rs
cp patches/main.rs      src/main.rs

cargo test -p omega-rpc -- mev_share
cargo test -p omega-risk -- mev_share
cargo check --workspace
```
