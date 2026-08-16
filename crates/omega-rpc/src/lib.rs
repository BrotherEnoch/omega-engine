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
// ## Fix (this revision): arb_gas_info
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
mod arb_gas_info;
mod chainlink_agg;
mod net;
mod flashloan_liq;

pub mod client;
pub mod rate_limiter;
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
    AAVE_PROTOCOL_DATA_PROVIDER, AAVE_V3_POOL, BALANCER_V2_VAULT, USDC_NATIVE, WETH,
};