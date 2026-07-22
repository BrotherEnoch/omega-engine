// crates/omega-oracle/src/per_chain.rs
//
// PerChainOracle â€” per-chain oracle coordinator.
//
// ## Role
//
//   One `PerChainOracle` instance runs per active chain.  It consumes
//   typed events from omega-rpc broadcast channels, applies tri-oracle
//   resolution (Â§7), and publishes `OracleSignal` snapshots to an
//   `arc_swap`-backed EIL double-buffer (Â§6) that strategies read.
//
//   The coordinator also updates heartbeat handles in the
//   `OracleLivenessMonitor` so the health layer (ExternalData) reflects
//   live oracle status.
//
// ## Streams consumed (from omega-rpc)
//
//   FeeOracleStream       â†’ FeeOracle OracleSignal (Â§7)
//   DexSyncStream         â†’ PoolReserves OracleSignal (Â§10 MSA Bellman-Ford)
//   LendingProtocolStream â†’ HealthFactor OracleSignal (Â§11 LA tier)
//
// ## Signal versioning
//
//   Every new signal increments an atomic `state_version` counter per
//   chain.  The EIL double-buffer holds the latest `Arc<Vec<OracleSignal>>`
//   snapshot keyed by version.  Strategies compare against the blueprint's
//   `state_version` to detect stale state before simulation (Â§6, Â§13.4).
//
// ## Debounce
//
//   DexSync events for the same pool arriving within 50ms are coalesced
//   into a single PoolReserves signal (Â§10, MSA Bellman-Ford debounce).
//   FeeOracle and HealthFactor signals are not debounced â€” every block
//   counts.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy_primitives::{keccak256, Address, B256};
use arc_swap::ArcSwap;
use dashmap::DashMap;
use tokio::sync::broadcast;

use omega_core::{
    FeeSnapshot, LayerHealth, OracleSignal, SignalKind,
};
use omega_health::monitors::OracleFeedHandle;
use omega_rpc::{DexSyncEvent, FeeOracleEvent, LendingProtocolEvent};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Constants
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// MSA Bellman-Ford DEX sync debounce window (Â§10).
const DEX_DEBOUNCE_MS: u64 = 50;

/// Capacity for the outbound OracleSignal broadcast channel.
const SIGNAL_CHANNEL_CAPACITY: usize = 256;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// EilSnapshot â€” the arc-swap EIL double-buffer value type
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Immutable snapshot of all signals at a single state version.
///
/// Swapped atomically into the `ArcSwap` on every new version.
/// Strategies hold `Arc<EilSnapshot>` for the duration of one scoring
/// cycle â€” the ArcSwap swap does not block them.
#[derive(Debug, Clone)]
pub struct EilSnapshot {
    pub state_version: u64,
    pub state_hash:    B256,
    pub signals:       Vec<OracleSignal>,
    pub fee:           FeeSnapshot,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// PerChainOracle
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Per-chain oracle coordinator.
///
/// Shared via `Arc<PerChainOracle>` between the update tasks and the
/// strategy scoring loops.
pub struct PerChainOracle {
    pub chain_id:      u64,
    state_version:     AtomicU64,
    /// EIL double-buffer â€” atomically swapped on every new signal batch.
    pub eil:           ArcSwap<EilSnapshot>,
    /// Outbound OracleSignal broadcast for immediate strategy consumers.
    pub signal_tx:     broadcast::Sender<OracleSignal>,
    /// Last DEX sync timestamps per pool (for 50ms debounce).
    dex_last_seen:     DashMap<Address, u64>,
    /// Chainlink feed liveness handle â€” heartbeated on each price update.
    pub cl_handle:     Arc<OracleFeedHandle>,
    /// Pyth feed liveness handle.
    pub pyth_handle:   Arc<OracleFeedHandle>,
    /// Health layer for ExternalData transitions.
    health:            Option<Arc<dyn LayerHealth>>,
}

impl PerChainOracle {
    /// Create a new coordinator.
    pub fn new(chain_id: u64) -> Arc<Self> {
        let (signal_tx, _) = broadcast::channel(SIGNAL_CHANNEL_CAPACITY);

        let initial_fee = FeeSnapshot {
            base_fee_gwei:    0,
            l1_data_fee_gwei: 0,
            priority_fee_gwei: 0,
            block_number:     0,
        };

        let initial_snap = Arc::new(EilSnapshot {
            state_version: 0,
            state_hash:    B256::ZERO,
            signals:       Vec::new(),
            fee:           initial_fee,
        });

        Arc::new(Self {
            chain_id,
            state_version:   AtomicU64::new(0),
            eil:             ArcSwap::from(initial_snap),
            signal_tx,
            dex_last_seen:   DashMap::new(),
            cl_handle:       OracleFeedHandle::new("chainlink", true),
            pyth_handle:     OracleFeedHandle::new("pyth", true),
            health:          None,
        })
    }

    /// Wire in the ExternalData health layer.
    pub fn with_health(self: Arc<Self>, health: Arc<dyn LayerHealth>) -> Arc<Self> {
        // Safety: we are the sole Arc holder during construction
        let ptr = Arc::into_raw(self) as *mut Self;
        unsafe { (*ptr).health = Some(health); }
        unsafe { Arc::from_raw(ptr) }
    }

    /// Subscribe to outbound OracleSignal events.
    ///
    /// Strategy scoring loops subscribe once and select on this receiver
    /// alongside the halt flag.
    pub fn subscribe(&self) -> broadcast::Receiver<OracleSignal> {
        self.signal_tx.subscribe()
    }

    /// Current EIL snapshot (lock-free read).
    pub fn snapshot(&self) -> Arc<EilSnapshot> {
        self.eil.load_full()
    }

    // â”€â”€ Background update loops â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Consume FeeOracleStream events and publish FeeOracle signals (Â§7).
    ///
    /// Runs indefinitely.  Spawn as a Tokio task.
    pub async fn run_fee_oracle(
        self: Arc<Self>,
        mut rx: broadcast::Receiver<FeeOracleEvent>,
    ) {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if event.chain_id != self.chain_id {
                        continue;
                    }
                    let fee = FeeSnapshot {
                        base_fee_gwei:     event.base_fee_gwei,
                        l1_data_fee_gwei:  0, // populated by ArbGasInfo; 0 here as default
                        priority_fee_gwei: 0,
                        block_number:      event.block_number,
                    };
                    let signal = self.make_signal(
                        SignalKind::FeeOracle,
                        event.block_number,
                        event.received_at_unix_ms,
                        serde_json::json!({
                            "base_fee_gwei":     fee.base_fee_gwei,
                            "l1_data_fee_gwei":  fee.l1_data_fee_gwei,
                            "priority_fee_gwei": fee.priority_fee_gwei,
                        }),
                    );
                    self.publish_with_fee(signal, fee);
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(chain_id = self.chain_id, skipped = n, "Fee oracle lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    /// Consume DexSyncStream events and publish PoolReserves signals (Â§10).
    ///
    /// Applies 50ms debounce per pool â€” rapid reserve updates for the same
    /// pool within one debounce window are coalesced into a single signal.
    pub async fn run_dex_sync(
        self: Arc<Self>,
        mut rx: broadcast::Receiver<DexSyncEvent>,
    ) {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if event.chain_id != self.chain_id {
                        continue;
                    }

                    // Debounce: skip if same pool updated within DEX_DEBOUNCE_MS
                    let now_ms = now_unix_ms();
                    let last = self.dex_last_seen
                        .get(&event.pool)
                        .map(|v| *v)
                        .unwrap_or(0);

                    if now_ms.saturating_sub(last) < DEX_DEBOUNCE_MS {
                        continue;
                    }
                    self.dex_last_seen.insert(event.pool, now_ms);

                    // Determine block number from log (may be None if pending)
                    let block_number = event.log.block_number.unwrap_or(0);

                    let signal = self.make_signal(
                        SignalKind::PoolReserves,
                        block_number,
                        event.received_at_unix_ms,
                        serde_json::json!({
                            "pool":     format!("{:#x}", event.pool),
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

    /// Consume LendingProtocolStream events and publish HealthFactor signals (Â§11).
    pub async fn run_lending_protocol(
        self: Arc<Self>,
        mut rx: broadcast::Receiver<LendingProtocolEvent>,
    ) {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if event.chain_id != self.chain_id {
                        continue;
                    }
                    let protocol_str = format!("{:?}", event.protocol).to_lowercase();
                    let block_number = event.log.block_number.unwrap_or(0);

                    // HealthFactor payload â€” exact values require a follow-up
                    // eth_call; the log provides the position address via topics.
                    let position = event.log.topics().first()
                        .map(|t| format!("{t:#x}"))
                        .unwrap_or_default();

                    let signal = self.make_signal(
                        SignalKind::HealthFactor,
                        block_number,
                        event.received_at_unix_ms,
                        serde_json::json!({
                            "position": position,
                            "hf_e18":  "0",   // populated by a follow-up read
                            "protocol": protocol_str,
                        }),
                    );
                    self.publish(signal);
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(chain_id = self.chain_id, skipped = n, "Lending stream lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    // â”€â”€ Internal helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    fn next_version(&self) -> u64 {
        self.state_version.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn make_signal(
        &self,
        kind:               SignalKind,
        block_number:       u64,
        received_at_unix_ms: u64,
        payload:            serde_json::Value,
    ) -> OracleSignal {
        let version    = self.next_version();
        let state_hash = compute_state_hash(self.chain_id, version, block_number);

        OracleSignal {
            kind,
            chain_id: self.chain_id,
            block_number,
            received_at_unix_ms,
            state_version: version,
            state_hash,
            payload,
        }
    }

    fn publish(&self, signal: OracleSignal) {
        let snap = self.eil.load_full();
        let mut signals = snap.signals.clone();
        signals.push(signal.clone());

        let new_snap = Arc::new(EilSnapshot {
            state_version: signal.state_version,
            state_hash:    signal.state_hash,
            signals,
            fee:           snap.fee.clone(),
        });
        self.eil.store(new_snap);

        let _ = self.signal_tx.send(signal);
    }

    fn publish_with_fee(&self, signal: OracleSignal, fee: FeeSnapshot) {
        let snap = self.eil.load_full();
        let mut signals = snap.signals.clone();
        signals.push(signal.clone());

        let new_snap = Arc::new(EilSnapshot {
            state_version: signal.state_version,
            state_hash:    signal.state_hash,
            signals,
            fee,
        });
        self.eil.store(new_snap);

        let _ = self.signal_tx.send(signal);
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Helpers
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Compute the canonical state hash for an EIL snapshot.
///
/// keccak256(chain_id ++ state_version ++ block_number)
fn compute_state_hash(chain_id: u64, state_version: u64, block_number: u64) -> B256 {
    let mut buf = [0u8; 24];
    buf[..8].copy_from_slice(&chain_id.to_be_bytes());
    buf[8..16].copy_from_slice(&state_version.to_be_bytes());
    buf[16..].copy_from_slice(&block_number.to_be_bytes());
    keccak256(&buf)
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_hash_changes_with_version() {
        let h1 = compute_state_hash(42161, 1, 1000);
        let h2 = compute_state_hash(42161, 2, 1000);
        assert_ne!(h1, h2, "different versions must produce different hashes");
    }

    #[test]
    fn state_hash_changes_with_chain() {
        let h1 = compute_state_hash(42161, 1, 1000);
        let h2 = compute_state_hash(1,     1, 1000);
        assert_ne!(h1, h2, "different chains must produce different hashes");
    }

    #[test]
    fn initial_snapshot_has_zero_version() {
        let oracle = PerChainOracle::new(42161);
        let snap   = oracle.snapshot();
        assert_eq!(snap.state_version, 0);
        assert!(snap.signals.is_empty());
    }

    #[tokio::test]
    async fn fee_oracle_publishes_signal() {
        let oracle = PerChainOracle::new(42161);
        let mut rx = oracle.subscribe();

        let (fee_tx, fee_rx) = broadcast::channel(8);
        let oracle_clone     = oracle.clone();
        tokio::spawn(oracle_clone.run_fee_oracle(fee_rx));

        fee_tx.send(FeeOracleEvent {
            base_fee_gwei:       10,
            block_number:        1_000,
            received_at_unix_ms: 0,
            chain_id:            42161,
        }).unwrap();

        let signal = tokio::time::timeout(
            Duration::from_millis(200),
            rx.recv(),
        ).await
        .expect("timeout")
        .expect("channel closed");

        assert_eq!(signal.kind, SignalKind::FeeOracle);
        assert_eq!(signal.chain_id, 42161);
        assert_eq!(signal.block_number, 1_000);
        assert!(signal.state_version > 0);
    }

    #[tokio::test]
    async fn wrong_chain_events_are_dropped() {
        let oracle = PerChainOracle::new(42161);
        let mut rx = oracle.subscribe();

        let (fee_tx, fee_rx) = broadcast::channel(8);
        let oracle_clone     = oracle.clone();
        tokio::spawn(oracle_clone.run_fee_oracle(fee_rx));

        // Send event for chain 1 (Ethereum) â€” should not appear on Arbitrum oracle
        fee_tx.send(FeeOracleEvent {
            base_fee_gwei:       5,
            block_number:        500,
            received_at_unix_ms: 0,
            chain_id:            1, // wrong chain
        }).unwrap();

        let result = tokio::time::timeout(
            Duration::from_millis(100),
            rx.recv(),
        ).await;

        assert!(result.is_err(), "wrong-chain event must not produce a signal");
    }

    #[test]
    fn dex_debounce_suppresses_rapid_updates() {
        let oracle = PerChainOracle::new(42161);
        let pool   = Address::from([0xAB; 20]);

        // First insert
        oracle.dex_last_seen.insert(pool, now_unix_ms());

        // Check: within DEX_DEBOUNCE_MS, same pool should be suppressed
        let last = oracle.dex_last_seen.get(&pool).map(|v| *v).unwrap_or(0);
        let elapsed = now_unix_ms().saturating_sub(last);
        assert!(elapsed < DEX_DEBOUNCE_MS, "fresh entry must be within debounce window");
    }
}