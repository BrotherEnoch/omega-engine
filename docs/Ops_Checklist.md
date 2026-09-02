# docs/Ops_Checklist.md — before raising `active_phase`

## 1. Secrets (env only; never commit)

```bash
export OMEGA_TX_SIGNING_KEY=0x...          # tx envelope key
export OMEGA_BLUEPRINT_SIGNING_KEY=0x...   # blueprint auth key (separate)
export ORCHESTRATOR_ADDRESS=0x...
export ARBITRUM_HTTP_RPC_URL=https://...
export ARBITRUM_RPC_URL=https://...        # if distinct from HTTP

# Per configured relay name (phase-gated in config)
export OMEGA_RELAY_ENDPOINT_FLASHBOTS=https://...
export FLASHBOTS_AUTH_KEY=...
# similarly TITAN / BLOXROUTE / EDEN as needed
```

## 2. Deployment manifest

- Replace sample/Hardhat-style entries in `config/deployment_manifest.toml`
- Prefer generating via `omega-manifest-gen` against live `eth_getCode`
- Confirm strategy id strings match what IntegrityRegistry / Stage 2b expect (`SA`, `MSA`, `LA`, `MEV`, …)

## 3. Kill switch

Defaults (from P8 residual package):

| Env | Default |
|-----|---------|
| `OMEGA_KILL_MAX_CUMULATIVE_LOSS_WEI` | 1 ETH |
| `OMEGA_KILL_MAX_LOSS_PER_WINDOW_WEI` | 0.25 ETH |
| `OMEGA_KILL_LOSS_WINDOW_SECS` | 3600 |
| `OMEGA_KILL_MAX_CONSECUTIVE_FAILURES` | 5 |

Tune before live capital.

## 4. Phase

- `config/default.toml`: `active_phase = 0` (shadow)
- Raise only when keys + relays + manifest are real
- Phase ≥ 1 fails closed if zero relays or empty integrity registry

## 5. ZK submit (if used)

```bash
export OMEGA_VAULT_SUBMIT_NONCE=$(cast nonce $SUBMITTER --rpc-url $ARBITRUM_HTTP_RPC_URL)
# also gas / fee env vars per docs/ZK_Gaps_Closed.md
```

## 6. Fee policy

- `docs/fee-policy.md` is APPROVED for Arbitrum One (42161) only
- Do not run other chains without a new sign-off
