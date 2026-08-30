# docs/C5_Relay_Production_Bootstrap.md

## Status

**Implemented on main** in `src/main.rs`. This package documents the flow and
adds **C5b**: hard fail at startup when `active_phase >= 1` and zero
`HttpRelayClient`s were constructed.

## Requirements → code

| Requirement | Location |
|-------------|----------|
| Translate `OmegaConfig.relay` | `omega_execution::config_translation::translate_relay_config` + `RelayBootstrapInputs { confirmation_rpc_url }` |
| Construct real `HttpRelayClient`s | Loop over phase-gated `phase_1_relays` / `phase_2plus_relays`; endpoint `OMEGA_RELAY_ENDPOINT_<NAME>`; auth per `RelayAuth` conventions |
| Construct `MultiRelayClient` | `MultiRelayClient::new(relay_clients, metrics, blacklist, &relay_cfg, startup_block=0)` |
| Wire reorg receiver | Drain `reorg_event_rx` (log today); **feed** via `rpc.subscribe_blocks()` → `feed_block_event_to_reorg_guard` → `on_new_block(number, hash)` |
| Reconciliation lifecycle | Task on oracle subscribe: `relay.reconcile_inclusions(current_block)` |

## Bootstrap sequence

```
ARBITRUM_HTTP_RPC_URL
  → translate_relay_config(config.relay, confirmation_rpc_url)
  → warn unmapped fields
  → for each candidate RelayName (phase-gated):
        OMEGA_RELAY_ENDPOINT_<NAME>
        + FLASHBOTS_AUTH_KEY | TITAN_AUTH_KEY | BLOXROUTE_AUTH_KEY | EDEN_AUTH_KEY
        → HttpRelayClient::new
  → if empty && phase >= 1 → BAIL (C5b)
  → MultiRelayClient::new
  → spawn reorg event drain
  → spawn block-hash feed (subscribe_blocks)
  → spawn inclusion reconciliation (oracle block stream)
```

## Env vars

| Var | Role |
|-----|------|
| `ARBITRUM_HTTP_RPC_URL` | Confirmation RPC (required) |
| `OMEGA_RELAY_ENDPOINT_FLASHBOTS` (etc.) | Bundle endpoint per relay — never hardcoded |
| `FLASHBOTS_AUTH_KEY` / `TITAN_AUTH_KEY` | flashbots-style auth |
| `BLOXROUTE_AUTH_KEY` / `EDEN_AUTH_KEY` | bearer token |
| `OMEGA_EXECUTION_ADDRESS` | Metrics label only (optional) |

`RelayName::Other` is always skipped (no verified auth convention).

## Fail closed

| Condition | Behavior |
|-----------|----------|
| Missing endpoint/auth for a candidate | Skip that relay, warn |
| Zero relays, phase 0 | Warn; continue (shadow) |
| Zero relays, phase ≥ 1 | **Startup `bail!` (C5b)** |
| Unmapped TOML fields | Warn; translation still succeeds |

## Still open

- Reorg event **rescoring** consumer (drain logs only)
- `startup_block` still 0 (no sync head read off `rpc`)
- `ExecutionAddress` is a metrics label, not a signer identity

## Apply

```bash
cp patches/main.rs src/main.rs   # or apply the zero-relay hunk only
cp src/relay_factory.rs crates/omega-execution/src/relay_factory.rs
cargo check --workspace
```
