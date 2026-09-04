// crates/omega-rpc/src/lib.rs
//
// omega-rpc — WebSocket RPC client and subscription multiplexer.
//
// ## Architectural role (§22.1)
//
//   omega-rpc is the ONLY crate in the workspace permitted to use the
//   full alloy transport stack (providers, transports-ws, pubsub).
//   All other crates receive chain data via the oracle layer (omega-oracle)
//   or via typed event channels — never via direct RPC handles.
//
//   Dependency graph:
//     omega-rpc ← omega-core, omega-health
//     omega-oracle ← omega-rpc
//
// ## Hard constraints (§4, §22 hardware spec)
//
//   - Max 8 reads per Microtx blueprint (enforced by callers via gated_read)
//   - Max 1 write per execution cycle (enforced by gated_write /
//     submit_raw_transaction)
//   - 500 rps target against a dedicated high-throughput node
//
// ## Audit pass summary (prior revision)
//
// Applied a boundary-safety audit (the guiding question: can this crate
// ever let the engine act on incorrect, stale, incomplete, or
// duplicated external information?). Findings fixed in that pass:
//   - Chain ID was never verified against any connected endpoint —
//     added `net::verify_chain_id`, called at every connection
//     establishment (client.rs, subscriptions.rs).
//   - No duplicate-transaction-submission protection existed at all —
//     added `OmegaRpcClient::submit_raw_transaction`, backed by a
//     bounded, TTL'd dedup cache keyed on transaction hash.
//   - The "single WebSocket connection" architecture described in the
//     original docs didn't actually exist: one-shot read calls opened a
//     fresh connection per call. Fixed with a shared, lazily-
//     established, reconnect-on-failure connection in `OmegaRpcClient`.
//   - No reorg/staleness signal existed on incoming blocks — added
//     `BlockEvent::is_reorg_or_stale`.
//   - SSE parsing in `run_mev_share_stream` processed each HTTP chunk
//     independently, silently dropping/corrupting any event that
//     straddled a chunk boundary — fixed with a persistent, bounded
//     byte buffer.
//   - A truncating `as u64` cast on gas-fee values could silently wrap
//     an absurd/malformed RPC-reported value into a small, dangerously
//     LOW number — fixed with `net::wei_to_gwei_saturating`.
//   - Three of four hardcoded lending-protocol contract addresses were
//     unverified placeholders — the stream now refuses to start rather
//     than silently watching wrong/nonexistent contracts. See
//     `subscriptions::arbitrum_addrs` for what needs to be supplied
//     before that stream can be enabled in production.
//   - Raw WS URLs (which commonly embed an API key) were logged in the
//     clear — fixed with `net::redact_ws_url`.
//   - `connect_with_retry` retried forever regardless of cause — fixed
//     to stop immediately on a fatal (configuration) error via
//     `RpcClientError::is_fatal`, a BREAKING signature change (see
//     client.rs).
//   - `RateLimiterSnapshot::rpc_headroom()` hardcoded an assumed
//     capacity of 400, which silently diverges from reality for any
//     client configured with a non-default `rps_limit` — fixed to
//     track and report the actual configured capacity.
//
// ## Fix (prior revision): fetch_chainlink_round / chainlink_agg
//
// Added `chainlink_agg` — the AggregatorV3Interface `sol!` binding and
// `OmegaRpcClient::fetch_chainlink_round` (client.rs), giving
// omega-oracle's new Chainlink poll loop a real eth_call path instead of
// a cache with nothing feeding it. Deliberately contains no reference to
// any omega-oracle type (e.g. `ChainlinkOracle`) — this crate has no
// dependency on omega-oracle and must not gain one, since omega-oracle
// already depends on omega-rpc; a reverse reference here would create a
// cycle. The "fetch then update the cache" wiring lives in
// omega-oracle's `chainlink_poll.rs` instead, which already has this
// crate as a dependency.
//
// ## Fix (prior revision): arb_gas_info
//
// Added `arb_gas_info` — the ArbGasInfo precompile `sol!` binding and
// `OmegaRpcClient::fetch_l1_base_fee_estimate_gwei` (see that module's
// own doc comment for the full reasoning). Same non-dependency
// discipline as chainlink_agg: no reference to any omega-oracle type
// here; the "fetch then update PerChainOracle's FeeSnapshot" wiring
// lives in `src/main.rs`'s new poll loop, mirroring the Chainlink split
// exactly. Declared as a private module (`mod`, not `pub mod`) — same
// as `chainlink_agg` above — since `OmegaRpcClient::
// fetch_l1_base_fee_estimate_gwei` is reachable via the already-`pub`
// `OmegaRpcClient` re-export below regardless of this module's own
// visibility; no new `pub use` is needed since this module adds a
// method to an existing public type rather than a new standalone type.
//
// ## Fix (prior revision): flashloan_liq — C7 provider registry / address
// resolution / snapshot generation / validation against actual deployed
// contracts
//
// `flashloan_liq.rs` now contains the real implementation behind what
// was previously just a set of re-exported constants:
//   - `AAVE_V3_POOL`, `AAVE_PROTOCOL_DATA_PROVIDER`, `BALANCER_V2_VAULT`,
//     `WETH`, `USDC_NATIVE` — real Arbitrum One addresses, each
//     individually re-verified against a live source THIS session (see
//     that file's own header) after an earlier, never-committed draft
//     of this same module transcribed `BALANCER_V2_VAULT` wrong
//     (truncated the trailing bytes) — caught by that verification
//     pass, not by luck.
//   - `resolve_liquidity_addresses` — the query-target-vs-recorded-label
//     split `main.rs`'s `OMEGA_AAVE_V3_POOL_TAG_OVERRIDE` /
//     `OMEGA_BALANCER_V2_VAULT_TAG_OVERRIDE` env vars already document,
//     now with one real function backing it instead of the split logic
//     living only in main.rs's own env-parsing code.
//   - `OmegaRpcClient::fetch_aave_available` /
//     `OmegaRpcClient::fetch_balancer_available` — real eth_call reads
//     (previously these were referenced by main.rs's L2e poll loop but
//     not shown to exist anywhere in this crate). Same non-dependency
//     discipline as chainlink_agg/arb_gas_info: these return a plain
//     `u128`, not a `LiquiditySnapshot` or any other
//     `omega_flashloan`-owned type — that crate's `LiquidityRegistry`
//     wraps the returned number into its own type on the `main.rs` side,
//     where the dependency on omega_flashloan already exists.
//   - `validate_deployed_contracts` — a real `eth_getCode` check (via
//     the new `OmegaRpcClient::get_code`, client.rs) against every
//     address above, run once at startup per `main.rs`'s own call site.
//     Confirms bytecode presence, not full ABI conformance — see that
//     function's own doc comment for the exact, deliberately-limited
//     scope of what it checks.
//
// ## Fix (this revision): flashloan_liq — C10 Uniswap V3 pool coverage
//
// `flashloan_liq.rs` gained `UNISWAP_V3_WETH_USDC_POOL` (a real, verified pool
// address — see that constant's own doc comment for the two-different-pools trap its
// verification caught) and `OmegaRpcClient::fetch_uniswap_v3_pool_balance`, extending
// C7's `validate_deployed_contracts` from 5 to 6 checked addresses in the process.
//
// `UNISWAP_V3_WETH_USDC_POOL` is added to this file's `pub use flashloan_liq::{...}`
// list below. THIS IS THE PART THAT WAS MISSED THE FIRST TIME THIS CONSTANT WAS
// ADDED: `flashloan_liq.rs` defining a new `pub const` does not, by itself, make it
// reachable as `omega_rpc::UNISWAP_V3_WETH_USDC_POOL` — this crate's re-exports are a
// hand-maintained list, not a glob (`pub use flashloan_liq::*;` would auto-include new
// items; this crate deliberately doesn't do that, presumably so the crate's public API
// surface is an explicit, reviewable list rather than "whatever flashloan_liq.rs
// happens to make pub"). A real `cargo build` caught the omission
// (`E0432: unresolved import 'omega_rpc::UNISWAP_V3_WETH_USDC_POOL'`) across four
// consecutive tool invocations (`build`, `check`, `test`, `clippy`) before this fix
// landed — anyone adding a new symbol to `flashloan_liq.rs` in the future needs to
// remember this file has its own separate list to update.
mod arb_gas_info;
mod chainlink_agg;
mod net;
mod flashloan_liq;

pub mod client;
pub mod rate_limiter;
pub mod reconciliation;
pub mod subscriptions;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use chainlink_agg::ChainlinkRound;

pub use client::{BlockEvent, OmegaRpcClient, RpcClientConfig};

pub use net::RpcClientError;

pub use rate_limiter::{BucketConfig, RateLimiterSnapshot, RpcRateLimiter, RpcRequestKind};

pub use subscriptions::{
    run_dex_sync_stream, run_fee_oracle_stream, run_lending_protocol_stream, run_mev_share_stream,
    run_pending_tx_stream, DexSyncEvent, FeeOracleEvent, LendingProtocol, LendingProtocolEvent,
    MevShareEvent, PendingTxEvent,
};

pub use flashloan_liq::{
    resolve_liquidity_addresses, validate_deployed_contracts, AddressValidation,
    DeploymentValidationReport, LiquidityProtocol, ResolvedLiquidityAddress,
    AAVE_PROTOCOL_DATA_PROVIDER, AAVE_V3_POOL, ARBITRUM_ONE_CHAIN_ID, BALANCER_V2_VAULT,
    UNISWAP_V3_WETH_USDC_POOL, USDC_NATIVE, WETH,
};

pub use reconciliation::{
    reconciler_for_providers, AtomicBalanceReconciler, ReconciliationConfig, ReconciliationError,
};