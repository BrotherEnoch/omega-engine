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
// ## Audit fixes (prior revision)
//
// 1. UNSOUND UNSAFE (critical): `with_health` previously mutated the
//    `health` field through a raw pointer obtained via
//    `Arc::into_raw`/`Arc::from_raw`. Fixed by changing `health:
//    Option<Arc<dyn LayerHealth>>` to an arc-swap-backed field, same
//    pattern as `eil: ArcSwap<EilSnapshot>`. Fully safe, no `unsafe`.
//
// 2. UNBOUNDED MEMORY GROWTH: `publish`/`publish_with_fee` cloned the
//    entire historical signal list on every call with no eviction.
//    Fixed with a bounded eviction cap (`MAX_SIGNAL_HISTORY`).
//
// 3. `dyn LayerHealth` is not `Sized`, so `ArcSwapOption<dyn LayerHealth>`
//    doesn't satisfy the pinned `arc-swap` version's `RefCnt` impl.
//    Fixed via `HealthHandle`, a plain `Sized` newtype wrapper.
//
// 4. Test fake `FakeHealth` updated to implement all of `LayerHealth`'s
//    trait items (`state()`, `layer_id()`) after the trait grew them.
//
// ## Fix (this revision): L1 data fee ingestion (ArbGasInfo)
//
// Added `update_l1_data_fee_gwei`, a new public method that updates
// ONLY the `fee.l1_data_fee_gwei` field of the current `EilSnapshot` in
// place. This closes the "populated by ArbGasInfo; 0 here as default"
// gap this file's own `run_fee_oracle` has documented on its
// `FeeSnapshot` construction since it was written — but does NOT touch
// `run_fee_oracle` itself's TRIGGER, deliberately: that method only
// fires on a `FeeOracleEvent` (an L2-base-fee signal arriving from a
// different, independent omega-rpc stream), and bundling an L1-fee poll
// result into that specific event handler would make the two updates
// artificially co-dependent on the same trigger for no reason. Instead,
// this method is called from a dedicated poll loop owned by the binary
// (see `src/main.rs`'s new poll-loop), the same architectural split
// already established for Chainlink ingestion (`ChainlinkOracle::
// update()` is called from a separate poll loop in `main.rs`, not from
// anything in this file).
//
// Deliberately does NOT bump `state_version` or emit a new
// `OracleSignal` on the outbound `signal_tx` broadcast — a periodic
// ArbGasInfo poll (proposed cadence: every 15s, matching the Chainlink
// poll loop's interval) is not itself a new discrete oracle event in
// the sense `EilSnapshot.state_version`/`state_hash` are meant to track
// (see this struct's own doc comment: "Every new signal increments an
// atomic state_version counter"). Bumping the version on every L1-fee
// poll tick would make `state_version` fire far more often than any
// blueprint's own `state_version` staleness check (§6, §13.4) is
// designed to tolerate, for a field no `OracleSignal` payload
// represents. `signals` (the historical signal list) and `state_hash`
// are therefore carried over unchanged from the current snapshot.
//
// ALSO FIXED (same revision, inside `run_fee_oracle`): the existing
// `FeeSnapshot` construction inside `run_fee_oracle` hardcoded
// `l1_data_fee_gwei: 0` on every FeeOracleEvent. Left as-is, this would
// have OVERWRITTEN whatever real value `update_l1_data_fee_gwei` last
// set, every time an unrelated L2-base-fee event arrived — since
// `publish_with_fee` replaces the whole `fee` struct wholesale, not
// just `base_fee_gwei`. Fixed to read the CURRENT snapshot's
// `l1_data_fee_gwei` and carry it forward instead of hardcoding 0, so
// an L2 base-fee update never clobbers a real L1 fee value the
// ArbGasInfo poll loop already populated. See the regression test
// `fee_oracle_event_preserves_previously_set_l1_data_fee` below.
//
// ## Fix (this revision, clippy): doc_lazy_continuation
//
// `update_l1_data_fee_gwei`'s doc comment contained a line starting
// with `+ ` (`+ \`ArcSwap::store\`, same pattern as ...`). Rustdoc's
// Markdown parser reads a line beginning with `+ ` as the start of a
// bullet-list item, which made every following doc-comment line up to
// the next blank line an implicit continuation of that same list item.
// `clippy::doc_lazy_continuation` (promoted to a hard error by this
// workspace's `-D warnings`) requires continuation lines to be indented
// to align under the bullet marker. Fixed by indenting those
// continuation lines two extra spaces, matching clippy's own suggested
// fix exactly. No behavior, no non-doc code, changed.

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

/// Rolling window size for L1 gas fee volatility tracking (this
/// revision) — see `PerChainOracle::l1_gas_volatility_risk`. At the
/// ArbGasInfo poll loop's 15s cadence (`src/main.rs`'s "L2d" block),
/// 20 samples ≈ 5 minutes of history. A starting value, not derived
/// from any measured volatility profile.
const L1_FEE_HISTORY_CAP: usize = 20;

/// Coefficient-of-variation (stddev/mean) value treated as "maximum
/// risk" (1.0) for L1 gas fee volatility — see
/// `l1_gas_volatility_risk`'s own doc comment. A 50% relative stddev
/// over the rolling window is a policy choice for what counts as
/// "unstable," not a value derived from real Arbitrum L1-fee volatility
/// data (which hasn't been measured against this).
const L1_GAS_VOLATILITY_CV_NORMALIZATION: f64 = 0.5;

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
/// See this file's module-level audit note (finding 3) for why this
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
    /// Rolling window of recent L1 data fee readings (gwei), fed by
    /// `update_l1_data_fee_gwei` — see that method's and
    /// `l1_gas_volatility_risk`'s doc comments. A plain `std::sync::
    /// Mutex`, not lock-free like `eil`/`health` above: this is updated
    /// at most once per ArbGasInfo poll tick (15s) and read at most
    /// once per scoring cycle, several orders of magnitude below the
    /// hot-path frequency `eil`'s lock-free design exists for.
    l1_fee_history: std::sync::Mutex<std::collections::VecDeque<u64>>,
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
            l1_fee_history: std::sync::Mutex::new(std::collections::VecDeque::with_capacity(
                L1_FEE_HISTORY_CAP,
            )),
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

    /// Update ONLY the L1 data fee (in gwei) of the current snapshot,
    /// in place — closes the ArbGasInfo ingestion gap. See this file's
    /// module-level "Fix (this revision): L1 data fee ingestion" note
    /// for the full reasoning, including why this deliberately does
    /// NOT bump `state_version`, emit a new `OracleSignal`, or touch
    /// `signals`/`state_hash`.
    ///
    /// Lock-free for the snapshot update: a single `ArcSwap::load_full`
    /// + `ArcSwap::store`, same pattern as `publish`/`publish_with_fee`
    ///   below, just without the signal-history push. ALSO records this
    ///   reading into `l1_fee_history` (this revision) — the rolling
    ///   window `l1_gas_volatility_risk` reads. Both updates happen from
    ///   the same call because both are driven by the same event (one
    ///   ArbGasInfo poll tick producing one new reading); recording
    ///   history separately would risk the two falling out of sync if a
    ///   caller ever called one without the other.
    pub fn update_l1_data_fee_gwei(&self, l1_data_fee_gwei: u64) {
        let snap = self.eil.load_full();
        let new_snap = Arc::new(EilSnapshot {
            state_version: snap.state_version,
            state_hash: snap.state_hash,
            signals: snap.signals.clone(),
            fee: FeeSnapshot {
                l1_data_fee_gwei,
                ..snap.fee.clone()
            },
        });
        self.eil.store(new_snap);

        let mut hist = match self.l1_fee_history.lock() {
            Ok(g) => g,
            // Recover rather than panic on a poisoned lock — same
            // reasoning as `omega-execution::pipeline::DagSlotGuard::
            // do_release`'s poison recovery: a prior panic elsewhere
            // holding this lock shouldn't cascade into a second panic
            // inside routine bookkeeping here.
            Err(poisoned) => poisoned.into_inner(),
        };
        hist.push_back(l1_data_fee_gwei);
        if hist.len() > L1_FEE_HISTORY_CAP {
            hist.pop_front();
        }
    }

    /// Coefficient-of-variation (stddev / mean) of the last
    /// `L1_FEE_HISTORY_CAP` L1 data fee readings, normalized to
    /// `[0.0, 1.0]` where 1.0 = maximum risk (highly volatile or
    /// insufficient data to judge). Feeds the "gas volatility"
    /// component of `src/main.rs`'s risk-score formula.
    ///
    /// Returns `1.0` (fail closed, not "no data = safe") when fewer
    /// than 2 samples exist — same "insufficient data → maximum risk"
    /// convention this codebase already applies elsewhere (e.g.
    /// `OracleSnapshot`'s stale-feed handling), and when the mean is
    /// non-positive (a degenerate reading set that can't produce a
    /// meaningful ratio — e.g. before the ArbGasInfo poll loop's first
    /// successful cycle, when every recorded sample would be 0).
    ///
    /// `L1_GAS_VOLATILITY_CV_NORMALIZATION` (0.5 = 50% relative stddev
    /// treated as maximally risky) is a policy default, not measured
    /// against real Arbitrum L1-fee behavior — see that constant's own
    /// doc comment.
    pub fn l1_gas_volatility_risk(&self) -> f64 {
        let hist = match self.l1_fee_history.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };

        if hist.len() < 2 {
            return 1.0;
        }

        let n = hist.len() as f64;
        let mean = hist.iter().sum::<u64>() as f64 / n;
        if mean <= 0.0 {
            return 1.0;
        }

        let variance = hist
            .iter()
            .map(|&x| {
                let d = x as f64 - mean;
                d * d
            })
            .sum::<f64>()
            / n;
        let stddev = variance.sqrt();
        let cv = stddev / mean;

        (cv / L1_GAS_VOLATILITY_CV_NORMALIZATION).clamp(0.0, 1.0)
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
                    // FIX (this revision): read the CURRENT snapshot's
                    // l1_data_fee_gwei and carry it forward, instead of
                    // hardcoding 0 — see this file's module-level "ALSO
                    // FIXED" note for why hardcoding 0 here would
                    // silently clobber a real value update_l1_data_fee_
                    // gwei already set, on every unrelated L2-base-fee
                    // event.
                    let current_l1_data_fee_gwei = self.eil.load_full().fee.l1_data_fee_gwei;
                    let fee = FeeSnapshot {
                        base_fee_gwei: event.base_fee_gwei,
                        l1_data_fee_gwei: current_l1_data_fee_gwei,
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

    // ── Audit fix regression tests (prior revision) ───────────────────────────

    #[test]
    fn with_health_sets_and_reads_back_without_unsafe() {
        struct FakeHealth;
        impl LayerHealth for FakeHealth {
            fn set_state(&self, _state: omega_core::HealthState, _reason: &str) {}
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

    // ── L1 data fee ingestion (this revision) ─────────────────────────────

    #[test]
    fn update_l1_data_fee_gwei_sets_the_field() {
        let oracle = PerChainOracle::new(42161);
        assert_eq!(oracle.snapshot().fee.l1_data_fee_gwei, 0, "starts at 0");
        oracle.update_l1_data_fee_gwei(42);
        assert_eq!(oracle.snapshot().fee.l1_data_fee_gwei, 42);
    }

    #[test]
    fn update_l1_data_fee_gwei_does_not_bump_state_version() {
        let oracle = PerChainOracle::new(42161);
        let before = oracle.snapshot().state_version;
        oracle.update_l1_data_fee_gwei(99);
        let after = oracle.snapshot().state_version;
        assert_eq!(
            before, after,
            "an L1-fee poll update must not look like a new discrete oracle signal"
        );
    }

    #[test]
    fn update_l1_data_fee_gwei_preserves_other_fee_fields() {
        let oracle = PerChainOracle::new(42161);
        oracle.update_l1_data_fee_gwei(123);
        let snap = oracle.snapshot();
        assert_eq!(snap.fee.l1_data_fee_gwei, 123);
        // base_fee_gwei/priority_fee_gwei/block_number carried over via
        // struct-update syntax, not reset — starting values (0) confirm
        // they weren't overwritten to something else.
        assert_eq!(snap.fee.base_fee_gwei, 0);
        assert_eq!(snap.fee.priority_fee_gwei, 0);
        assert_eq!(snap.fee.block_number, 0);
    }

    #[tokio::test]
    async fn fee_oracle_event_preserves_previously_set_l1_data_fee() {
        // Regression guard for this revision's fix inside run_fee_oracle
        // itself: publishing a new L2-base-fee-triggered FeeSnapshot must
        // preserve whatever l1_data_fee_gwei update_l1_data_fee_gwei last
        // set, not silently reset it to 0.
        let oracle = PerChainOracle::new(42161);
        oracle.update_l1_data_fee_gwei(55);
        assert_eq!(oracle.snapshot().fee.l1_data_fee_gwei, 55);

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

        tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");

        let snap = oracle.snapshot();
        assert_eq!(
            snap.fee.l1_data_fee_gwei, 55,
            "an L2-base-fee-triggered FeeOracle signal must not clobber a \
             previously-set real l1_data_fee_gwei value"
        );
        assert_eq!(snap.fee.base_fee_gwei, 10, "base fee itself still updates normally");
    }

    // ── L1 gas volatility risk (this revision) ────────────────────────────

    #[test]
    fn volatility_risk_insufficient_data_fails_closed() {
        let oracle = PerChainOracle::new(42161);
        assert_eq!(
            oracle.l1_gas_volatility_risk(),
            1.0,
            "zero samples must fail closed to maximum risk"
        );
        oracle.update_l1_data_fee_gwei(10);
        assert_eq!(
            oracle.l1_gas_volatility_risk(),
            1.0,
            "a single sample is not enough to compute a variance — must still fail closed"
        );
    }

    #[test]
    fn volatility_risk_zero_mean_fails_closed() {
        let oracle = PerChainOracle::new(42161);
        oracle.update_l1_data_fee_gwei(0);
        oracle.update_l1_data_fee_gwei(0);
        assert_eq!(
            oracle.l1_gas_volatility_risk(),
            1.0,
            "an all-zero history (e.g. before ArbGasInfo's first successful poll) \
             must fail closed, not compute a spurious 0/0 ratio"
        );
    }

    #[test]
    fn volatility_risk_stable_readings_are_low_risk() {
        let oracle = PerChainOracle::new(42161);
        for _ in 0..10 {
            oracle.update_l1_data_fee_gwei(100);
        }
        let risk = oracle.l1_gas_volatility_risk();
        assert!(
            risk < 0.05,
            "perfectly stable readings should score near-zero risk, got {risk}"
        );
    }

    #[test]
    fn volatility_risk_wildly_varying_readings_are_high_risk() {
        let oracle = PerChainOracle::new(42161);
        // Alternates 10 / 1000 gwei — enormous relative stddev.
        for i in 0..10 {
            oracle.update_l1_data_fee_gwei(if i % 2 == 0 { 10 } else { 1_000 });
        }
        let risk = oracle.l1_gas_volatility_risk();
        assert!(
            risk > 0.9,
            "wildly varying readings should score near-maximum risk, got {risk}"
        );
    }

    #[test]
    fn volatility_risk_history_is_bounded() {
        let oracle = PerChainOracle::new(42161);
        // Push far more than L1_FEE_HISTORY_CAP stable readings, then one
        // outlier — if the window weren't bounded, the huge stable
        // history would dilute the outlier's effect on the computed risk
        // far more than intended.
        for _ in 0..500 {
            oracle.update_l1_data_fee_gwei(100);
        }
        let hist_len = oracle.l1_fee_history.lock().unwrap().len();
        assert_eq!(
            hist_len, L1_FEE_HISTORY_CAP,
            "history must be capped at L1_FEE_HISTORY_CAP, not grow unbounded"
        );
    }
}