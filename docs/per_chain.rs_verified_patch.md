# per_chain.rs_verified_patch.md
# `per_chain.rs` — verified patch (three sections only)

**Not a full-file rewrite.** Only these three sections of `per_chain.rs`
have been pasted directly in this session: the `PerChainOracle` struct,
its `new()`, and `run_dex_sync`'s full body. Everything else in that file
(`with_health`, `health`, `subscribe`, `snapshot`, `make_signal`,
`publish`, `run_fee_oracle`, `run_lending_protocol`) has never been seen
and is left untouched — apply this patch by hand against the real file.

---

## 1. Struct — add a `twap` field

```rust
pub struct PerChainOracle {
    pub chain_id: u64,
    state_version: AtomicU64,
    /// EIL double-buffer — atomically swapped on every new signal batch.
    pub eil: ArcSwap<EilSnapshot>,
    /// Outbound OracleSignal broadcast for immediate strategy consumers.
    pub signal_tx: broadcast::Sender<OracleSignal>,
    /// Last DEX sync timestamps per pool (for 50ms debounce).
    dex_last_seen: DashMap<[u8; 20], u64>,
    /// Chainlink feed liveness handle — heartbeated on each price update.
    pub cl_handle: Arc<OracleFeedHandle>,
    /// Pyth feed liveness handle.
    pub pyth_handle: Arc<OracleFeedHandle>,
    /// Health layer for ExternalData transitions.
    health: ArcSwapOption<dyn LayerHealth>,
    // NEW — shared TWAP price cache, populated by run_dex_sync from real
    // Uniswap V2 Sync events (see that method's updated body below). A
    // shared Arc rather than an owned TwapOracle so callers outside this
    // struct (main.rs's scoring loop, via build_oracle_snapshot) read the
    // exact same cache instance run_dex_sync writes to.
    pub twap: Arc<crate::TwapOracle>,
}
```

## 2. Constructor — accept a shared `Arc<TwapOracle>`

This is a real, breaking signature change — every existing call site
(including any tests calling `PerChainOracle::new(42161)`) needs updating
to pass the new argument.

```rust
pub fn new(chain_id: u64, twap: Arc<crate::TwapOracle>) -> Arc<Self> {
    let (signal_tx, _) = broadcast::channel(SIGNAL_CHANNEL_CAPACITY);
    let initial_fee = FeeSnapshot {
        base_fee_gwei: 0,
        l1_data_fee_gwei: 0,
        priority_fee_gwei: 0,
        block_number: 0,
    };
    let initial_snap = Arc::new(EilSnapshot {
        state_version: 0,
        state_hash: B256::ZERO,
        signals: Vec::new(),
        fee: initial_fee,
    });
    Arc::new(Self {
        chain_id,
        state_version: AtomicU64::new(0),
        eil: ArcSwap::from(initial_snap),
        signal_tx,
        dex_last_seen: DashMap::new(),
        cl_handle: OracleFeedHandle::new("chainlink", true),
        pyth_handle: OracleFeedHandle::new("pyth", true),
        health: ArcSwapOption::empty(),
        twap,
    })
}
```

## 3. `run_dex_sync` — decode real V2 reserves, feed `self.twap`

```rust
/// Consume DexSyncStream events and publish PoolReserves signals (§10).
///
/// Applies 50ms debounce per pool — rapid reserve updates for the same
/// pool within one debounce window are coalesced into a single signal.
///
/// FIX (this revision): previously published a hardcoded
/// `"reserve0": "0", "reserve1": "0"` regardless of the real event data
/// (confirmed against the real prior body) and never fed `TwapOracle` at
/// all. Now decodes the real Uniswap V2 `Sync(uint112,uint112)` reserves
/// from `event.log.data().data` (verified accessor — see this session's
/// docs.rs lookups for `alloy_rpc_types::Log::data()` and
/// `alloy_primitives::LogData`'s public `data: Bytes` field) and, for
/// known pools, updates `self.twap` with a real computed price.
///
/// HONESTY NOTE: the pool table (`twap::arbitrum_pools()`) is documented
/// as Uniswap V3 pools, but this stream filters on the V2 `Sync` topic —
/// see `twap.rs`'s new `lookup_arbitrum_pool`/`decode_v2_sync_reserves`
/// doc comments. This computes a V2 reserve-ratio price as the TWAP
/// cache's input, not a real V3 sqrtPriceX96 TWAP.
///
/// `block_time` uses wall-clock time, not the log's own timestamp — the
/// real `Log` type (verified this session) has a `block_timestamp:
/// Option<u64>` field that would be more correct if populated by the
/// RPC provider; using it instead of `SystemTime::now()` is a follow-up,
/// not done here to keep this patch scoped to the reserve-decode fix.
pub async fn run_dex_sync(self: Arc<Self>, mut rx: broadcast::Receiver<DexSyncEvent>) {
    loop {
        match rx.recv().await {
            Ok(event) => {
                if event.chain_id != self.chain_id {
                    continue;
                }

                // Debounce: skip if same pool updated within DEX_DEBOUNCE_MS
                let now_ms = now_unix_ms();
                let pool_key = <[u8; 20]>::try_from(event.pool.as_slice())
                    .expect("event pool address must be 20 bytes");

                let last = self.dex_last_seen.get(&pool_key).map(|v| *v).unwrap_or(0);

                if now_ms.saturating_sub(last) < DEX_DEBOUNCE_MS {
                    continue;
                }
                self.dex_last_seen.insert(pool_key, now_ms);

                // Determine block number from log (may be None if pending)
                let block_number = event.log.block_number.unwrap_or(0);
                let pool_hex = format!("{:#x}", event.pool);

                // NEW: real V2 reserve decode + TWAP cache update, for
                // known pools only.
                if let Some((symbol, t0_num, d0, d1)) =
                    crate::twap::lookup_arbitrum_pool(&pool_hex)
                {
                    // event.log.data() -> &LogData (shortcut for
                    // log.inner.data, verified via docs.rs); LogData.data
                    // is the raw Bytes payload (also verified, public
                    // field, not a method).
                    if let Some((reserve0, reserve1)) =
                        crate::twap::decode_v2_sync_reserves(event.log.data().data.as_ref())
                    {
                        if let Some(price) = crate::twap::price_from_v2_reserves(
                            reserve0, reserve1, d0, d1, t0_num,
                        ) {
                            let block_time = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            self.twap.update(symbol, price, block_time, block_number);
                        }
                    }
                }

                let signal = self.make_signal(
                    SignalKind::PoolReserves,
                    block_number,
                    event.received_at_unix_ms,
                    serde_json::json!({
                        "pool": pool_hex,
                        // Still not decoded into the signal payload itself —
                        // the TWAP cache (self.twap.update above) is the
                        // path build_oracle_snapshot actually reads from.
                        // Populating this JSON with real reserves too is a
                        // separate, smaller follow-up if any consumer reads
                        // signal payloads directly instead of the cache.
                        "reserve0": "0",
                        "reserve1": "0",
                    }),
                );
                self.publish(signal);
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(chain_id = self.chain_id, skipped = n, "DEX sync lagged");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}
```

## What this does not fix (unchanged from the design discussion)

- Chainlink/Pyth still have zero ingestion — only the TWAP leg gets real
  data from this patch.
- Real Uniswap V3 sqrtPriceX96 — still V2 reserve math, honestly labeled.
- `block_time` — wall clock, not chain time; `Log.block_timestamp` would
  be more correct but isn't wired here.
- The malformed-LINK-pool-address test failure mentioned in the earlier
  design pass — unrelated to this patch, still outstanding.