ï»¿// crates/omega-rpc/src/lib.rs
//
// omega-rpc â€” WebSocket RPC client and subscription multiplexer.
//
// ## Architectural role (Â§22.1)
//
//   omega-rpc is the ONLY crate in the workspace permitted to use the
//   full alloy transport stack (providers, transports-ws, pubsub).
//   All other crates receive chain data via the oracle layer (omega-oracle)
//   or via typed event channels â€” never via direct RPC handles.
//
//   Dependency graph:
//     omega-rpc â† omega-core, omega-health
//     omega-oracle â† omega-rpc
//
// ## Hard constraints (Â§4, Â§22 hardware spec)
//
//   - Max 8 reads per Microtx blueprint (enforced by callers via gated_read)
//   - Max 1 write per execution cycle (enforced by gated_write)
//   - 500 rps target against a dedicated high-throughput node
//
// ## Module map
//
//   client.rs         â€” OmegaRpcClient: rate-limited WS client with block
//                       subscription, health integration, and reconnect
//
//   rate_limiter.rs   â€” RpcRateLimiter: token-bucket per request kind
//                       (Read 400 rps, Write 50 rps, Subscribe 20 rps)
//
//   subscriptions.rs  â€” Five typed subscription streams:
//                         run_pending_tx_stream       (SA/MSA order flow)
//                         run_lending_protocol_stream (LA events)
//                         run_dex_sync_stream         (MSA Bellman-Ford)
//                         run_fee_oracle_stream       (Â§7 gas model input)
//                         run_mev_share_stream        (Phase 4 MEV-OFA)

pub mod client;
pub mod rate_limiter;
pub mod subscriptions;

// â”€â”€ Re-exports â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub use client::{BlockEvent, OmegaRpcClient, RpcClientConfig};

pub use rate_limiter::{
    BucketConfig,
    RateLimiterSnapshot,
    RpcRateLimiter,
    RpcRequestKind,
};

pub use subscriptions::{
    run_dex_sync_stream,
    run_fee_oracle_stream,
    run_lending_protocol_stream,
    run_mev_share_stream,
    run_pending_tx_stream,
    DexSyncEvent,
    FeeOracleEvent,
    LendingProtocol,
    LendingProtocolEvent,
    MevShareEvent,
    PendingTxEvent,
};