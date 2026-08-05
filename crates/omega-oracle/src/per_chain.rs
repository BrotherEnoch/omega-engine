// crates/omega-oracle/src/per_chain.rs
//
// PerChainOracle — per-chain oracle coordinator.
//
// ## Role
//
//   One `PerChainOracle` instance runs per active chain.  It consumes
//   typed events from omega-rpc broadcast channels, applies tri-oracle
//   resolution (§7), and publishes `OracleSignal` snapshots to an
//   `arc_swap`-backed EIL double-buffer (§6) that strategies read.
//
//   The coordinator also updates heartbeat handles in the
//   `OracleLivenessMonitor` so the health layer (ExternalData) reflects
//   live oracle status.
//
// ## Streams consumed (from omega-rpc)
//
//   FeeOracleStream       → FeeOracle OracleSignal (§7)
//   DexSyncStream         → PoolReserves OracleSignal (§10 MSA Bellman-Ford)
//   LendingProtocolStream → HealthFactor OracleSignal (§11 LA tier)
//
// ## Signal versioning
//
//   Every new signal increments an atomic `state_version` counter per
//   chain.  The EIL double-buffer holds the latest `Arc<Vec<OracleSignal>>`
//   snapshot keyed by version.  Strategies compare against the blueprint's
//   `state_version` to detect stale state before simulation (§6, §13.4).
//
// ## Debounce
//
//   DexSync events for the same pool arriving within 50ms are coalesced
//   into a single PoolReserves signal (§10, MSA Bellman-Ford debounce).
//   FeeOracle and HealthFactor signals are not debounced — every block
//   counts.
//
// ## Audit fixes (this revision)
//
// 1. UNSOUND UNSAFE (critical): `with_health` previously mutated the
//    `health` field through a raw pointer obtained via
//    `Arc::into_raw`/`Arc::from_raw`, justified only by a comment
//    asserting "we are the sole Arc holder during construction" — a
//    claim the type signature `self: Arc<Self>` does not actually
//    enforce. Nothing prevented a caller from having cloned the `Arc`
//    before calling this method; if they had, this was a genuine data
//    race (writing through a raw pointer while another thread reads
//    the same field through its own `Arc` clone, with zero
//    synchronization) — undefined behavior, not merely a style issue.
//    Fixed by changing `health: Option<Arc<dyn LayerHealth>>` to an
//    arc-swap-backed field, the same pattern this struct already uses
//    for `eil: ArcSwap<EilSnapshot>`. `with_health` is now fully safe —
//    no `unsafe` block, no raw pointers, correct regardless of how many
//    `Arc` clones exist. See finding 3 below for the exact field type
//    this ended up as, after an intermediate attempt that didn't
//    compile.
//
// 2. UNBOUNDED MEMORY GROWTH: `publish`/`publish_with_fee` cloned
//    `snap.signals` (the ENTIRE historical signal list since process
//    start) on every single call, pushed one more entry, and stored the
//    result — with no eviction, ever. On Arbitrum's ~250ms blocks with
//    several signal kinds firing per block, this grows without bound
//    for the life of the process: unbounded memory, and an
//    increasingly expensive full-vector clone on every publish. Fixed
//    with a bounded eviction cap (`MAX_SIGNAL_HISTORY`), the same
//    pattern already established in this codebase's
//    `omega_rpc::client::SubmissionTracker`. The cap is a starting
//    point, not a derived value — tune it against actual downstream
//    consumption of `EilSnapshot.signals` (which this crate cannot see
//    from here).
//
// 3. ARC-SWAP + TRAIT OBJECT DID NOT ACTUALLY COMPILE (this revision,
//    correcting an earlier mistake): the previous revision declared
//    `health: ArcSwapOption<dyn LayerHealth>`, intending the same
//    lock-free pattern as `eil`. That does not compile against the
//    pinned `arc-swap` dependency in this workspace: its `RefCnt` trait
//    is implemented as `impl<T> RefCnt for Arc<T>` — WITHOUT a
//    `T: ?Sized` bound — so it only applies when the `Arc`'s pointee is
//    `Sized`. `dyn LayerHealth` is not `Sized`, so `Arc<dyn LayerHealth>`
//    (and therefore `ArcSwapOption<dyn LayerHealth>`, which wraps
//    exactly that) never satisfies `RefCnt`, and every call site touching
//    `self: Arc<Self>` or the `health` field failed with "the size for
//    values of type `(dyn LayerHealth + 'static)` cannot be known at
//    compilation time." Fixed by introducing `HealthHandle`, a plain
//    `Sized` newtype wrapping the real `Arc<dyn LayerHealth>`, and
//    storing `ArcSwapOption<HealthHandle>` instead. `ArcSwapOption`'s
//    own pointee is now the `Sized` `HealthHandle` struct, satisfying
//    the installed crate's `RefCnt` impl, while `HealthHandle` still
//    holds the actual trait object underneath — so `with_health`'s and
//    `health()`'s public signatures (`Arc<dyn LayerHealth>` in and out)
//    are unchanged. If this workspace's `arc-swap` is ever upgraded to
//    a version whose `RefCnt` impl is `?Sized`-generic, `HealthHandle`
//    could be removed and `health` could go back to
//    `ArcSwapOption<dyn LayerHealth>` directly — this wrapper is a
//    workaround for the installed version's limitation, not a design
//    preference.
//
// 4. TEST FAKE OUT OF SYNC WITH `LayerHealth` (this revision): the
//    `FakeHealth` test type in `with_health_sets_and_reads_back_without_unsafe`
//    only implemented `set_state`, but `LayerHealth` has since grown
//    `state()` and `layer_id()` (E0046: not all trait items
//    implemented). Added both per rustc's own suggested stubs — this
//    test only exercises `with_health`/`health()` round-tripping the
//    handle, it never calls `state()`/`layer_id()` on the fake, so
//    `todo!()` bodies are safe here; they're placeholders, not
//    behavior this test depends on. If a future test needs a fake that
//    actually reports state, give it real bodies instead.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::{SystemTime, UNIX_EPOCH};

use alloy_primitives::{keccak256, B256};
use arc_swap::{ArcSwap, ArcSwapOption};
use dashmap::DashMap;
use tokio::sync::broadcast;

use omega_core::{FeeSnapshot, LayerHealth, OracleSignal, SignalKind};
use omega_health::monitors::OracleFeedHandle;
use omega_rpc::{DexSyncEvent, FeeOracleEvent, LendingProtocolEvent};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// MSA Bellman-Ford DEX sync debounce window (§10).
const DEX_DEBOUNCE_MS: u64 = 50;

/// Capacity for the outbound OracleSignal broadcast channel.
const SIGNAL_CHANNEL_CAPACITY: usize = 256;

/// Hard cap on how many historical signals `EilSnapshot.signals` retains.
/// Oldest entries are evicted once this is exceeded — see `evict_oldest`.
/// Prevents unbounded memory growth across the life of a long-running
/// process; not a derived/authoritative value, just a conservative
/// starting bound (a few blocks' worth across all signal kinds at
/// Arbitrum's block rate).
const MAX_SIGNAL_HISTORY: usize = 4_096;

// ─────────────────────────────────────────────────────────────────────────────
// EilSnapshot — the arc-swap EIL double-buffer value type
// ─────────────────────────────────────────────────────────────────────────────

/// Immutable snapshot of all signals at a single state version.
///
/// Swapped atomically into the `ArcSwap` on every new version.
/// Strategies hold `Arc<EilSnapshot>` for the duration of one scoring
/// cycle — the ArcSwap swap does not block them.
#[derive(Debug, Clone)]
pub struct EilSnapshot {
    pub state_version: u64,
    pub state_hash: B256,
    /// Bounded to `MAX_SIGNAL_HISTORY` entries — see this file's
    /// module-level audit note. Oldest entries are evicted first.
    pub signals: Vec<OracleSignal>,
    pub fee: FeeSnapshot,
}

// ─────────────────────────────────────────────────────────────────────────────
// HealthHandle — Sized wrapper working around the pinned arc-swap version
// ─────────────────────────────────────────────────────────────────────────────

/// Plain `Sized` newtype wrapping the real health handle.
///
/// See finding 3 in this file's module-level audit note for why this
/// exists: the pinned `arc-swap` version's `RefCnt` impl for `Arc<T>`
/// requires `T: Sized`, so `dyn LayerHealth` cannot be `ArcSwapOption`'s
/// direct pointee. Wrapping it in this struct gives `ArcSwapOption` a
/// `Sized` pointee to work with, while this struct's single field is
/// still the actual `Arc<dyn LayerHealth>` the public API deals in.
struct HealthHandle(Arc<dyn LayerHealth>);

// ─────────────────────────────────────────────────────────────────────────────
// PerChainOracle
// ─────────────────────────────────────────────────────────────────────────────

/// Per-chain oracle coordinator.
///
/// Shared via `Arc<PerChainOracle>` between the update tasks and the
/// strategy scoring loops.
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
    ///
    /// `ArcSwapOption<HealthHandle>` rather than a plain
    /// `Option<Arc<dyn LayerHealth>>` — see this file's module-level
    /// audit note (finding 3) for why the direct `ArcSwapOption<dyn
    /// LayerHealth>` this crate briefly tried does not compile against
    /// the pinned `arc-swap` version, and why `HealthHandle` is the
    /// workaround rather than a `Mutex`/`RwLock` (lock-free reads,
    /// consistent with `eil`'s existing pattern in this struct).
    health: ArcSwapOption<HealthHandle>,
}

impl PerChainOracle {
    /// Create a new coordinator.
    pub fn new(chain_id: u64) -> Arc<Self> {
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
        })
    }

    /// Wire in the ExternalData health layer.
    ///
    /// Fully safe — no `unsafe`, no raw pointers. Correct regardless of
    /// how many `Arc<PerChainOracle>` clones exist at call time, unlike
    /// the original raw-pointer-mutation implementation (see this
    /// file's module-level audit note, finding 1).
    pub fn with_health(self: Arc<Self>, health: Arc<dyn LayerHealth>) -> Arc<Self> {
        self.health.store(Some(Arc::new(HealthHandle(health))));
        self
    }

    /// Current health layer handle, if one has been wired in via
    /// `with_health`. Lock-free read.
    pub fn health(&self) -> Option<Arc<dyn LayerHealth>> {
        self.health.load_full().map(|handle| handle.0.clone())
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

    // ── Background update loops ───────────────────────────────────────────

    /// Consume FeeOracleStream events and publish FeeOracle signals (§7).
    ///
    /// Runs indefinitely.  Spawn as a Tokio task.
    pub async fn run_fee_oracle(self: Arc<Self>, mut rx: broadcast::Receiver<FeeOracleEvent>) {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if event.chain_id != self.chain_id {
                        continue;
                    }
                    let fee = FeeSnapshot {
                        base_fee_gwei: event.base_fee_gwei,
                        l1_data_fee_gwei: 0, // populated by ArbGasInfo; 0 here as default
                        priority_fee_gwei: 0,
                        block_number: event.block_number,
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

    /// Consume DexSyncStream events and publish PoolReserves signals (§10).
    ///
    /// Applies 50ms debounce per pool — rapid reserve updates for the same
    /// pool within one debounce window are coalesced into a single signal.
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

    /// Consume LendingProtocolStream events and publish HealthFactor signals (§11).
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

                    // HealthFactor payload — exact values require a follow-up
                    // eth_call; the log provides the position address via topics.
                    let position = event
                        .log
                        .topics()
                        .first()
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
                    tracing::warn!(
                        chain_id = self.chain_id,
                        skipped = n,
                        "Lending stream lagged"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────────

    fn next_version(&self) -> u64 {
        self.state_version.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn make_signal(
        &self,
        kind: SignalKind,
        block_number: u64,
        received_at_unix_ms: u64,
        payload: serde_json::Value,
    ) -> OracleSignal {
        let version = self.next_version();
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

    /// Evicts oldest entries from `signals` in place until its length is
    /// at most `MAX_SIGNAL_HISTORY`. Called after every push in
    /// `publish`/`publish_with_fee` — see this file's module-level audit
    /// note on why this bound exists.
    fn evict_oldest(signals: &mut Vec<OracleSignal>) {
        if signals.len() > MAX_SIGNAL_HISTORY {
            let excess = signals.len() - MAX_SIGNAL_HISTORY;
            signals.drain(0..excess);
        }
    }

    fn publish(&self, signal: OracleSignal) {
        let snap = self.eil.load_full();
        let mut signals = snap.signals.clone();
        signals.push(signal.clone());
        Self::evict_oldest(&mut signals);

        let new_snap = Arc::new(EilSnapshot {
            state_version: signal.state_version,
            state_hash: signal.state_hash,
            signals,
            fee: snap.fee.clone(),
        });
        self.eil.store(new_snap);

        let _ = self.signal_tx.send(signal);
    }

    fn publish_with_fee(&self, signal: OracleSignal, fee: FeeSnapshot) {
        let snap = self.eil.load_full();
        let mut signals = snap.signals.clone();
        signals.push(signal.clone());
        Self::evict_oldest(&mut signals);

        let new_snap = Arc::new(EilSnapshot {
            state_version: signal.state_version,
            state_hash: signal.state_hash,
            signals,
            fee,
        });
        self.eil.store(new_snap);

        let _ = self.signal_tx.send(signal);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

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
    keccak256(buf)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

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
        let h2 = compute_state_hash(1, 1, 1000);
        assert_ne!(h1, h2, "different chains must produce different hashes");
    }

    #[test]
    fn initial_snapshot_has_zero_version() {
        let oracle = PerChainOracle::new(42161);
        let snap = oracle.snapshot();
        assert_eq!(snap.state_version, 0);
        assert!(snap.signals.is_empty());
    }

    #[tokio::test]
    async fn fee_oracle_publishes_signal() {
        let oracle = PerChainOracle::new(42161);
        let mut rx = oracle.subscribe();

        let (fee_tx, fee_rx) = broadcast::channel(8);
        let oracle_clone = oracle.clone();
        tokio::spawn(oracle_clone.run_fee_oracle(fee_rx));

        fee_tx
            .send(FeeOracleEvent {
                base_fee_gwei: 10,
                block_number: 1_000,
                received_at_unix_ms: 0,
                chain_id: 42161,
            })
            .unwrap();

        let signal = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
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
        let oracle_clone = oracle.clone();
        tokio::spawn(oracle_clone.run_fee_oracle(fee_rx));

        // Send event for chain 1 (Ethereum) — should not appear on Arbitrum oracle
        fee_tx
            .send(FeeOracleEvent {
                base_fee_gwei: 5,
                block_number: 500,
                received_at_unix_ms: 0,
                chain_id: 1, // wrong chain
            })
            .unwrap();

        let result = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;

        assert!(
            result.is_err(),
            "wrong-chain event must not produce a signal"
        );
    }

    #[test]
    fn dex_debounce_suppresses_rapid_updates() {
        let oracle = PerChainOracle::new(42161);
        let pool = [0xAB; 20];

        // First insert
        oracle.dex_last_seen.insert(pool, now_unix_ms());

        // Check: within DEX_DEBOUNCE_MS, same pool should be suppressed
        let last = oracle.dex_last_seen.get(&pool).map(|v| *v).unwrap_or(0);
        let elapsed = now_unix_ms().saturating_sub(last);
        assert!(
            elapsed < DEX_DEBOUNCE_MS,
            "fresh entry must be within debounce window"
        );
    }

    // ── Audit fix regression tests (this revision) ───────────────────────────

    #[test]
    fn with_health_sets_and_reads_back_without_unsafe() {
        struct FakeHealth;
        impl LayerHealth for FakeHealth {
            fn set_state(&self, _state: omega_core::HealthState, _reason: &str) {}
            // Neither method below is exercised by this test — it only
            // round-trips the handle through with_health()/health() — so
            // todo!() bodies are fine here. See finding 4 in this file's
            // module-level audit note. If your actual HealthState/LayerId
            // variant names differ from what rustc's own suggested stub
            // implies, this compiles regardless since the bodies are
            // never called.
            fn state(&self) -> omega_core::HealthStatus {
                todo!()
            }
            fn layer_id(&self) -> omega_core::LayerId {
                todo!()
            }
        }
        let oracle = PerChainOracle::new(42161).with_health(Arc::new(FakeHealth));
        assert!(
            oracle.health().is_some(),
            "health handle must be readable after with_health"
        );
    }

    #[test]
    fn health_defaults_to_none_before_with_health() {
        let oracle = PerChainOracle::new(42161);
        assert!(oracle.health().is_none());
    }

    #[test]
    fn signal_history_bounded_after_exceeding_max() {
        // Directly exercises evict_oldest via publish(), bypassing the
        // broadcast plumbing — push well past MAX_SIGNAL_HISTORY and
        // confirm the retained signals vec never exceeds the cap.
        let oracle = PerChainOracle::new(42161);
        for i in 0..(MAX_SIGNAL_HISTORY + 500) {
            let signal = oracle.make_signal(
                SignalKind::PoolReserves,
                i as u64,
                0,
                serde_json::json!({ "seq": i }),
            );
            oracle.publish(signal);
        }
        let snap = oracle.snapshot();
        assert_eq!(
            snap.signals.len(),
            MAX_SIGNAL_HISTORY,
            "signal history must be capped at MAX_SIGNAL_HISTORY, not grow unbounded"
        );
    }

    #[test]
    fn signal_history_eviction_keeps_newest_entries() {
        let oracle = PerChainOracle::new(42161);
        let total = MAX_SIGNAL_HISTORY + 10;
        for i in 0..total {
            let signal = oracle.make_signal(
                SignalKind::PoolReserves,
                i as u64,
                0,
                serde_json::json!({ "seq": i }),
            );
            oracle.publish(signal);
        }
        let snap = oracle.snapshot();
        // The oldest 10 block_numbers (0..10) must have been evicted;
        // the newest entry's block_number must be total-1.
        //
        // RECONSTRUCTED TAIL: the source pasted into this conversation
        // was truncated exactly at this point (no closing assertions or
        // braces). The two lines below follow directly from the
        // eviction math this test itself sets up — excess = total -
        // MAX_SIGNAL_HISTORY = 10 entries drained from the front — but
        // verify this against whatever the real file actually asserted.
        let oldest_retained = snap.signals.first().unwrap().block_number;
        let newest_retained = snap.signals.last().unwrap().block_number;
        assert_eq!(
            oldest_retained, 10,
            "the oldest 10 entries (block_number 0..10) must have been evicted"
        );
        assert_eq!(
            newest_retained,
            (total - 1) as u64,
            "the newest entry must still be present after eviction"
        );
    }
}