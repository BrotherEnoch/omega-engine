# C8_FlashloanLiquidity_Wiring_+_C4-A_Tooling.md
# C8 (flashloan liquidity wiring) + C4-A tooling (manifest generator)

Both were written against the files shown in this conversation only — there is
no access to the workspace root `Cargo.toml`, `Cargo.lock`, or
`crates/omega-security/src/integrity.rs` source. Everything below is either
directly consistent with patterns already proven elsewhere in this codebase,
or explicitly flagged as an assumption that needs a real `cargo check` /
manual verification. Nothing here should be merged without that pass.

## 1. `src/main.rs` — C8

**What changed:** `build_check_context`'s `flashloan_available_value` was
hardcoded `0` since C2 (see the removed comment block). It's now sourced from
a new `FlashloanLiquidityState` populated by a dedicated "L2e" poll loop
(mirrors L2d's ArbGasInfo poll exactly: 15s interval, keep-previous-value on
error, `tokio::sync::watch` channel), reading real Aave V3 / Balancer V2
liquidity for WETH via `omega-rpc`'s already-existing `flashloan_liq` module
(`fetch_aave_available` / `fetch_balancer_available`).

**Threading:** `flashloan_liq_rx: watch::Receiver<FlashloanLiquidityState>` is
passed `run_scoring_loop` → `score_and_admit` → `build_check_context`, same
pattern already established for `gas_volatility_risk` (C6) and
`exposure_tracker` (C7). All three functions already carry
`#[allow(clippy::too_many_arguments)]` from prior revisions.

**Known, deliberate limitation (documented in-code on
`FlashloanLiquidityState`):** the value is the MAX of the two providers, for
a single hardcoded asset (`omega_rpc::WETH`). It is a pre-trade **sanity**
signal for check 10, not a guarantee that whichever provider
`omega_flashloan::select_provider` actually picks for a given blueprint has
that much liquidity — that would need `LiquidityRegistry` fed from real data,
which is unchanged by this patch (still blocked, per the existing "C7
(separate task): flashloan integration status" doc comment).

**Side effect worth flagging to whoever reviews this:** because
`liquidity_risk` in the risk-score formula is no longer permanently pinned at
`1.0`, `RISK_SCORE_MAX_THRESHOLD` (0.45) no longer has the same
unconditional-fail-closed guarantee C6/C7 documented (the old floor
arithmetic assumed both `competition_risk` and `liquidity_risk` were pinned
at 1.0). I did **not** re-tune the threshold — see the new comment on that
constant. This is a real risk-policy question, not something to silently
paper over.

**To verify before merging:**
- [ ] `alloy::providers::Provider` really does have `get_code_at` (used only
      in the manifest tool, not this file — see part 2) — not relevant here.
- [ ] `tokio::sync::watch::Receiver<T>` is `Clone` regardless of whether `T:
      Clone` in the pinned tokio version (it is, as of tokio 1.x — `T: Clone`
      is only needed for `.borrow().clone()`, which `FlashloanLiquidityState`
      derives).
- [ ] Run `cargo check` — this is a large, hand-applied patch across several
      function signatures; a parameter-order mistake is the most likely class
      of error.

## 2. `crates/omega-manifest-gen/` — C4-A tooling

A new standalone binary crate, **not** a `bin` inside `omega-security`,
specifically because generating a manifest needs a real `eth_getCode` read,
and `omega-rpc` is documented (§22.1, in that crate's own `lib.rs`) as the
**only** crate permitted to touch the full alloy transport stack. Putting
this tool inside `omega-security` would mean either violating that rule
directly, or `omega-security` gaining a dependency on `omega-rpc` — a bigger
architectural change than a manifest-generation CLI warrants. A separate
crate depending on both `omega-rpc` (for the chain read) and, in spirit, on
`omega-security`'s manifest *format* (see below) is the smaller change.

**Important: this tool does NOT import `omega_security::DeploymentManifest`.**
I don't have `integrity.rs`'s real source, so I don't know
`StrategyDeployment`'s real field names/types with confidence — only
inferred from doc-comment prose in `src/main.rs`. Rather than guess and
possibly produce a tool that silently writes the wrong shape while still
compiling, I built a **local mirror struct** (`ManifestEntry`/`ManifestFile`)
and serialize that. This means:

- The tool will compile and run **without needing to see
  `omega-security`'s internals at all** — lower risk of a surprising build
  break from an unrelated crate.
- It does **not** guarantee the output matches the real
  `DeploymentManifest` shape. That has to be checked by hand (or by adding a
  `#[test]` inside `omega-security` itself that round-trips a
  `gen-manifest`-produced file through the real loader — I'd suggest adding
  exactly that test once the real struct is visible, as the actual
  verification step).

**Before this tool is trusted for anything production-facing:**
- [ ] Open `crates/omega-security/src/integrity.rs`, confirm
      `DeploymentManifest { strategies: Vec<StrategyDeployment> }` and each
      field name/type/`#[serde(rename)]` on `StrategyDeployment` against
      `ManifestEntry` in `gen-manifest`'s `main.rs`.
- [ ] Confirm the exact strings `StrategyId::to_string()` produces for SA /
      MSA / LA / MEV against `KNOWN_STRATEGY_IDS` in the same file — a
      mismatched case means `resolve_strategy_bytecode_hash` silently misses
      and falls back to `[0u8; 32]`, which is functionally identical to
      having no manifest at all but much harder to notice, since the file
      exists and "looks" registered.
- [ ] Confirm `Provider::get_code_at`'s real signature against the pinned
      alloy version (`cargo doc -p alloy --open`) — flagged inline in
      `fetch_bytecode_hash`'s doc comment; every other alloy-facing call in
      this codebase's shown files (`chainlink_agg.rs`, `arb_gas_info.rs`) was
      confirmed this way against real compiler output, and this one wasn't.
- [ ] Add `omega-manifest-gen` to the workspace root `Cargo.toml`'s
      `members` list, and reconcile the dependency versions in its
      `Cargo.toml` (flagged inline) against whatever the workspace already
      resolves.

**Usage (once wired in):**

```
# Manual mode
gen-manifest --manual strategies.toml --output config/deployment_manifest.toml

# forge broadcast mode
gen-manifest \
  --forge-broadcast broadcast/Deploy.s.sol/42161/run-latest.json \
  --strategy-map strategy-map.json \
  --output config/deployment_manifest.toml
```

Both modes refuse to run if the output file already exists (`--force` to
override) and refuse to register any address with empty `eth_getCode` —
consistent with the rest of this codebase's "never fabricate, flag and stop
instead" posture.