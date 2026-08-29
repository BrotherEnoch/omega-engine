# docs/C7_wiring_notes.md

## 1. `crates/omega-rpc/Cargo.toml` — add one dependency

`addresses.rs` builds `Address` consts at compile time via `hex_literal::hex!`,
so it doesn't have to hand-roll a `[u8;20]` array or do a fallible runtime
parse for constants that should be infallible by construction:

```toml
[dependencies]
hex-literal = "0.4"
```

Add `hex-literal = "0.4"` to `[workspace.dependencies]` in the root
`Cargo.toml` too if other crates end up wanting the same pattern; not done
here since only `omega-rpc` needs it right now.

## 2. `crates/omega-rpc/src/lib.rs` — export the new module

```rust
mod addresses;
pub use addresses::{
    resolve_liquidity_addresses, validate_deployed_contracts, AddressValidation,
    DeploymentValidationReport, LiquidityProtocol, ResolvedLiquidityAddress,
    ARBITRUM_ONE_CHAIN_ID, ARB_GAS_INFO,
};
// AAVE_V3_POOL, BALANCER_V2_VAULT, WETH already re-exported per main.rs's
// existing `use omega_rpc::{..., AAVE_V3_POOL, BALANCER_V2_VAULT, WETH};` —
// add them here too if they weren't already re-exported from this module's
// predecessor.
```

## 3. `OmegaRpcClient::get_code` — confirm or add

`validate_deployed_contracts` calls `rpc.get_code(address).await`. This
crate already makes `eth_call`-shaped requests (per main.rs's L2e loop
calling `fetch_aave_available` / `fetch_balancer_available`), so a plain
`eth_getCode` wrapper is a small, structurally similar addition if it
doesn't already exist — NOT confirmed against this crate's real client
surface, since that source wasn't available to check against directly.
Signature assumed:

```rust
pub async fn get_code(&self, address: Address) -> Result<Bytes>;
```

## 4. `src/main.rs` — call it once at startup, fail loud

Insert right after `chain_id` is resolved and before the L2d/L2e poll loops
are spawned, so a bad address halts startup instead of degrading silently
for the lifetime of the process:

```rust
let validation = omega_rpc::validate_deployed_contracts(&rpc, chain_id).await;
if !validation.all_ok() {
    for r in &validation.results {
        if !r.has_code || r.error.is_some() {
            tracing::error!(
                label = r.label,
                address = %r.address,
                error = ?r.error,
                "hardcoded contract address failed on-chain validation"
            );
        }
    }
    anyhow::bail!(
        "one or more hardcoded flashloan/oracle addresses failed on-chain \
         validation — see errors above; refusing to start the L2d/L2e poll \
         loops against unverified addresses"
    );
}
tracing::info!(chain_id, "C7: all hardcoded contract addresses validated on-chain");
```

This is a genuine startup gate (fail closed), consistent with how this
codebase already treats a malformed `OMEGA_CHAIN_ID` or a malformed
deployment manifest — both halt `main()` via `?`/`bail!` rather than
degrading.

## 5. `crates/omega-positions` — still not a workspace member

Flagged in that crate's own `Cargo.toml` comment and confirmed against the
root `Cargo.toml` in this session: `"crates/omega-positions"` is genuinely
missing from `[workspace].members`. One-line fix, not bundled into
`addresses.rs` since it's an unrelated gap:

```toml
members = [
    "crates/omega-core",
    ...
    "crates/omega-flashloan",
+   "crates/omega-positions",
    "crates/omega-relay",
    ...
]
```

## What C7 does *not* yet close

- **ABI-level validation** — `validate_deployed_contracts` confirms
  bytecode presence, not "this is actually an Aave V3 Pool implementing
  the expected interface." A follow-up call to a known view function
  (e.g. Aave's `Pool.getReserveData(WETH)` not reverting) would close that
  gap; not implemented here since it needs per-protocol ABI encoding this
  module doesn't own.
- **Uniswap V3** is still deliberately absent from address resolution here,
  same reasoning main.rs already gives: no single canonical pool address
  exists for it the way `AAVE_V3_POOL`/`BALANCER_V2_VAULT` do.
- The three real addresses in `addresses.rs` are transcribed from public
  protocol documentation, not re-derived from an Arbiscan lookup in this
  session — `validate_deployed_contracts` is what actually closes that
  loop at runtime; treat the constants as "needs the startup check to
  confirm," not as independently re-verified here.