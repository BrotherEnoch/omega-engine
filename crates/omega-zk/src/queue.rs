// crates/omega-zk/src/queue.rs
//
// Proof request queue with pressure FSM (spec config/default.toml [zk]).
//
// Spec pressure thresholds:
//   depth < 128  → Normal     — full throughput.
//   128 ≤ depth < 256 → Throttle  — new non-hot-path requests delayed; skip allowed.
//   256 ≤ depth < 512 → Suspend   — only hot-path (Microtx) proofs accepted; DEGRADED emitted.
//   depth ≥ 512  → Halt       — all submissions rejected; L0 HALT propagated.
//
// Architecture:
//   • `crossbeam_channel::bounded` MPMC channel — zero-allocation push/pop.
//   • Queue capacity = QUEUE_HALT_DEPTH (512) — hard bounded; Halt is the drop policy.
//   • AtomicUsize depth counter maintained alongside the channel for O(1) pressure reads.
//   • QueuePressure FSM read via AtomicU8 — lock-free hot path.
//   • Workers receive ProofRequest via the `Receiver` end.
//   • Blueprint submission path sends ProofRequest via the `Sender` end.
//   • ProofResponse is returned via a per-request `oneshot` channel embedded in ProofRequest.
//
// Thread safety:
//   ProofQueue is Clone (Arc internally) and Send + Sync.
//   Multiple strategy tasks can call `submit()` concurrently.
//   Multiple worker tasks can call `recv()` concurrently (MPMC).

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use tokio::sync::oneshot;

use crate::config::ZkConfig;
use crate::error::ZkError;
use crate::metrics;
use crate::prover::ZkProof;

// ─── Request / response types ─────────────────────────────────────────────────

/// Unique monotone request ID (per process).
static NEXT_REQUEST_ID: AtomicUsize = AtomicUsize::new(1);

fn next_request_id() -> u64 {
    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed) as u64
}

/// A proof generation request placed on the queue by strategy tasks.
pub struct ProofRequest {
    pub id: u64,
    pub blueprint_hash: [u8; 32],
    pub net_profit_wei: u128,
    pub chain_id: u64,
    pub strategy_id: String,
    /// True for Microtx lane (SLA: 1200 ms); false for Normal lane (4000 ms).
    pub is_microtx: bool,
    /// Response channel — the strategy task awaits this after submit().
    pub response_tx: oneshot::Sender<ProofResponse>,
}

impl std::fmt::Debug for ProofRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProofRequest")
            .field("id", &self.id)
            .field("blueprint_hash", &hex::encode(self.blueprint_hash))
            .field("strategy_id", &self.strategy_id)
            .field("is_microtx", &self.is_microtx)
            .finish()
    }
}

/// Response sent back to the strategy task after proof generation.
pub type ProofResponse = Result<ZkProof, ZkError>;

// ─── Queue pressure FSM ───────────────────────────────────────────────────────

/// Queue pressure state (spec thresholds: 128 / 256 / 512).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuePressure {
    Normal = 0,
    Throttle = 1,
    Suspend = 2,
    Halt = 3,
}

impl QueuePressure {
    pub(crate) fn from_depth_cfg(depth: usize, cfg: &ZkConfig) -> Self {
        if depth >= cfg.proof_queue_halt {
            QueuePressure::Halt
        } else if depth >= cfg.proof_queue_suspend {
            QueuePressure::Suspend
        } else if depth >= cfg.proof_queue_throttle {
            QueuePressure::Throttle
        } else {
            QueuePressure::Normal
        }
    }

    fn as_u8(self) -> u8 {
        self as u8
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => QueuePressure::Normal,
            1 => QueuePressure::Throttle,
            2 => QueuePressure::Suspend,
            _ => QueuePressure::Halt,
        }
    }

    pub fn is_halt_worthy(self) -> bool {
        self == QueuePressure::Halt
    }
    pub fn is_degraded_worthy(self) -> bool {
        self == QueuePressure::Suspend
    }
}

impl std::fmt::Display for QueuePressure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueuePressure::Normal => write!(f, "NORMAL"),
            QueuePressure::Throttle => write!(f, "THROTTLE"),
            QueuePressure::Suspend => write!(f, "SUSPEND"),
            QueuePressure::Halt => write!(f, "HALT"),
        }
    }
}

// ─── Proof queue ──────────────────────────────────────────────────────────────

struct ProofQueueInner {
    sender: Sender<ProofRequest>,
    depth: AtomicUsize,
    pressure: AtomicU8,
    cfg: ZkConfig,
}

/// MPMC proof request queue.
///
/// `ProofQueue` is Clone — all clones share the same underlying channel.
#[derive(Clone)]
pub struct ProofQueue {
    inner: Arc<ProofQueueInner>,
    pub receiver: Receiver<ProofRequest>,
}

impl ProofQueue {
    pub fn new(cfg: ZkConfig) -> Self {
        let capacity = cfg.proof_queue_halt;
        let (sender, receiver) = bounded(capacity);
        Self {
            receiver,
            inner: Arc::new(ProofQueueInner {
                sender,
                depth: AtomicUsize::new(0),
                pressure: AtomicU8::new(QueuePressure::Normal.as_u8()),
                cfg,
            }),
        }
    }

    // ── Submission API (called from strategy tasks) ───────────────────────────

    /// Submit a proof request.
    ///
    /// Rules (spec queue pressure FSM):
    ///   Normal   → always accepted.
    ///   Throttle → accepted (caller may add sleep in hot path).
    ///   Suspend  → only accepted if `is_microtx` (hot-path only).
    ///   Halt     → always rejected; caller must propagate HALT.
    ///
    /// Returns a `oneshot::Receiver` that resolves when the proof is complete.
    pub fn submit(
        &self,
        blueprint_hash: [u8; 32],
        net_profit_wei: u128,
        chain_id: u64,
        strategy_id: String,
        is_microtx: bool,
    ) -> Result<oneshot::Receiver<ProofResponse>, ZkError> {
        let pressure = self.pressure();
        let depth = self.depth();

        match pressure {
            QueuePressure::Halt => {
                metrics::REQUESTS_REJECTED
                    .with_label_values(&["queue_full"])
                    .inc();
                return Err(ZkError::QueueFull {
                    depth,
                    halt_threshold: self.inner.cfg.proof_queue_halt,
                });
            }
            QueuePressure::Suspend if !is_microtx => {
                metrics::REQUESTS_REJECTED
                    .with_label_values(&["suspended"])
                    .inc();
                return Err(ZkError::QueueSuspended { depth });
            }
            _ => {}
        }

        let (response_tx, response_rx) = oneshot::channel();
        let id = next_request_id();

        let req = ProofRequest {
            id,
            blueprint_hash,
            net_profit_wei,
            chain_id,
            strategy_id: strategy_id.clone(),
            is_microtx,
            response_tx,
        };

        let lane = if is_microtx { "microtx" } else { "normal" };

        match self.inner.sender.try_send(req) {
            Ok(()) => {
                let new_depth = self.inner.depth.fetch_add(1, Ordering::AcqRel) + 1;
                self.update_pressure(new_depth);
                metrics::REQUESTS_ENQUEUED
                    .with_label_values(&[lane, &strategy_id])
                    .inc();
                metrics::QUEUE_DEPTH.set(new_depth as f64);
                tracing::debug!(
                    id,
                    strategy = strategy_id,
                    is_microtx,
                    depth = new_depth,
                    "proof request enqueued"
                );
                Ok(response_rx)
            }
            Err(TrySendError::Full(_)) => {
                metrics::REQUESTS_REJECTED
                    .with_label_values(&["queue_full"])
                    .inc();
                Err(ZkError::QueueFull {
                    depth,
                    halt_threshold: self.inner.cfg.proof_queue_halt,
                })
            }
            Err(TrySendError::Disconnected(_)) => Err(ZkError::PoolShutdown),
        }
    }

    // ── Worker API (called from worker tasks) ─────────────────────────────────

    /// Acknowledge that a request was dequeued and processed.
    /// Called by worker tasks after completing (or failing) a proof.
    pub fn complete(&self) {
        let new_depth = self
            .inner
            .depth
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |depth| {
                Some(depth.saturating_sub(1))
            })
            .unwrap_or(0)
            .saturating_sub(1);
        self.update_pressure(new_depth);
        metrics::QUEUE_DEPTH.set(new_depth as f64);
    }

    // ── Pressure reads (hot path) ─────────────────────────────────────────────

    /// Current queue pressure — atomic, lock-free.
    #[inline]
    pub fn pressure(&self) -> QueuePressure {
        QueuePressure::from_u8(self.inner.pressure.load(Ordering::Acquire))
    }

    /// Current queue depth — atomic, lock-free.
    #[inline]
    pub fn depth(&self) -> usize {
        self.inner.depth.load(Ordering::Acquire)
    }

    fn update_pressure(&self, depth: usize) {
        let new_pressure = QueuePressure::from_depth_cfg(depth, &self.inner.cfg);
        let old_u8 = self
            .inner
            .pressure
            .swap(new_pressure.as_u8(), Ordering::AcqRel);
        let old_pressure = QueuePressure::from_u8(old_u8);

        if old_pressure != new_pressure {
            tracing::warn!(
                depth,
                from = %old_pressure,
                to   = %new_pressure,
                "ZK queue pressure transition"
            );
            metrics::QUEUE_PRESSURE_STATE.set(new_pressure.as_u8() as f64);
        }
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    use crate::config::ZkConfig;

    fn make_queue() -> ProofQueue {
        ProofQueue::new(ZkConfig::default())
    }

    fn submit(
        q: &ProofQueue,
        is_microtx: bool,
    ) -> Result<oneshot::Receiver<ProofResponse>, ZkError> {
        q.submit([0x01; 32], 1_000, 42161, "LA".into(), is_microtx)
    }

    #[test]
    fn fresh_queue_is_normal_pressure() {
        let q = make_queue();
        assert_eq!(q.pressure(), QueuePressure::Normal);
        assert_eq!(q.depth(), 0);
    }

    #[test]
    fn submit_increases_depth() {
        let q = make_queue();
        let _rx = submit(&q, false).unwrap();
        assert_eq!(q.depth(), 1);
    }

    #[test]
    fn complete_decreases_depth() {
        let q = make_queue();
        let _rx = submit(&q, false).unwrap();
        assert_eq!(q.depth(), 1);
        q.complete();
        assert_eq!(q.depth(), 0);
    }

    #[test]
    fn pressure_transitions_at_thresholds() {
        let cfg = ZkConfig {
            proof_queue_throttle: 4,
            proof_queue_suspend: 6,
            proof_queue_halt: 8,
            ..Default::default()
        };
        let q = ProofQueue::new(cfg);

        for _ in 0..4 {
            submit(&q, false).unwrap();
        }
        assert_eq!(q.pressure(), QueuePressure::Throttle);

        submit(&q, true).unwrap();
        submit(&q, true).unwrap();
        assert_eq!(q.pressure(), QueuePressure::Suspend);

        submit(&q, true).unwrap();
        submit(&q, true).unwrap();
        assert_eq!(q.pressure(), QueuePressure::Halt);
    }

    #[test]
    fn halt_pressure_rejects_all_submissions() {
        let cfg = ZkConfig {
            proof_queue_halt: 2,
            ..Default::default()
        };
        let q = ProofQueue::new(cfg);
        submit(&q, true).unwrap();
        submit(&q, true).unwrap();
        // Third is rejected
        assert!(matches!(submit(&q, true), Err(ZkError::QueueFull { .. })));
        assert!(matches!(submit(&q, false), Err(ZkError::QueueFull { .. })));
    }

    #[test]
    fn suspend_pressure_rejects_normal_but_accepts_microtx() {
        let cfg = ZkConfig {
            proof_queue_throttle: 2,
            proof_queue_suspend: 2,
            proof_queue_halt: 100,
            ..Default::default()
        };
        let q = ProofQueue::new(cfg);
        submit(&q, true).unwrap();
        submit(&q, true).unwrap();
        assert_eq!(q.pressure(), QueuePressure::Suspend);

        // Normal lane rejected
        assert!(matches!(
            submit(&q, false),
            Err(ZkError::QueueSuspended { .. })
        ));
        // Microtx still accepted
        assert!(submit(&q, true).is_ok());
    }

    #[test]
    fn depth_never_underflows_on_extra_complete() {
        let q = make_queue();
        q.complete(); // extra complete on empty queue
        assert_eq!(q.depth(), 0);
    }

    #[test]
    fn pressure_from_depth_boundaries() {
        let cfg = ZkConfig::default();
        assert_eq!(
            QueuePressure::from_depth_cfg(0, &cfg),
            QueuePressure::Normal
        );
        assert_eq!(
            QueuePressure::from_depth_cfg(127, &cfg),
            QueuePressure::Normal
        );
        assert_eq!(
            QueuePressure::from_depth_cfg(128, &cfg),
            QueuePressure::Throttle
        );
        assert_eq!(
            QueuePressure::from_depth_cfg(255, &cfg),
            QueuePressure::Throttle
        );
        assert_eq!(
            QueuePressure::from_depth_cfg(256, &cfg),
            QueuePressure::Suspend
        );
        assert_eq!(
            QueuePressure::from_depth_cfg(511, &cfg),
            QueuePressure::Suspend
        );
        assert_eq!(
            QueuePressure::from_depth_cfg(512, &cfg),
            QueuePressure::Halt
        );
        assert_eq!(
            QueuePressure::from_depth_cfg(9999, &cfg),
            QueuePressure::Halt
        );
    }
}
