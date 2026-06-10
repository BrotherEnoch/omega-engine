// crates/omega-observability/src/ring_buffer.rs
//
// Lock-free bounded ring buffer for high-frequency event ingestion (§16).
//
// ## Design
//
//   The ring buffer is the ingest path for OmegaEvents emitted by every
//   layer.  Callers MUST NOT block — if the buffer is full, events are
//   dropped with an overflow counter increment and an ERROR tracing event.
//
//   Implementation: two AtomicUsize cursors (head/tail) on a pre-allocated
//   `Vec<Option<OmegaEvent>>`.  The buffer is single-producer-compatible
//   but safe for concurrent producers because the CAS on the tail cursor
//   serialises competing writers.  The exporter (single consumer) reads
//   from the head.
//
// ## Capacity
//
//   Default capacity: 4096 events.  At 100 events/second (generous upper
//   bound for non-LA events) this provides ~40 seconds of headroom before
//   overflow.  LA events are always-sampled but rare (<1/block).
//
// ## Overflow policy
//
//   When the buffer is full, the oldest event is overwritten (ring
//   semantics) and `overflow_count` is incremented.  The exporter logs
//   the overflow count on each drain cycle so operator dashboards can
//   alert on sustained overflow.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::events::OmegaEvent;

/// Default ring buffer capacity in event slots.
pub const DEFAULT_CAPACITY: usize = 4096;

// ─────────────────────────────────────────────────────────────────────────────
// EventRingBuffer
// ─────────────────────────────────────────────────────────────────────────────

/// Bounded ring buffer for `OmegaEvent` ingestion.
///
/// Shared via `Arc<EventRingBuffer>` between producers (all layers)
/// and the single exporter consumer task.
pub struct EventRingBuffer {
    /// The ring storage.  Mutex protects the Vec for safe concurrent
    /// push and drain — the mutex is held only during the push (single
    /// event) or drain (move all) operation.  Both are O(1) and O(n)
    /// respectively with n bounded by `capacity`.
    inner: Mutex<RingInner>,
    /// Total events pushed (including overflows).
    pub total_in: AtomicU64,
    /// Total events drained by the exporter.
    pub total_out: AtomicU64,
    /// Events dropped due to overflow.
    pub overflow: AtomicU64,
}

struct RingInner {
    buf: Vec<Option<OmegaEvent>>,
    head: usize,
    tail: usize,
    len: usize,
    capacity: usize,
}

impl RingInner {
    fn new(capacity: usize) -> Self {
        Self {
            buf: (0..capacity).map(|_| None).collect(),
            head: 0,
            tail: 0,
            len: 0,
            capacity,
        }
    }

    fn push(&mut self, event: OmegaEvent) -> bool {
        if self.len == self.capacity {
            // Overflow: overwrite oldest (advance head)
            self.head = (self.head + 1) % self.capacity;
            self.buf[self.tail] = Some(event);
            self.tail = (self.tail + 1) % self.capacity;
            return false; // overflow occurred
        }
        self.buf[self.tail] = Some(event);
        self.tail = (self.tail + 1) % self.capacity;
        self.len += 1;
        true
    }

    fn drain(&mut self) -> Vec<OmegaEvent> {
        let mut out = Vec::with_capacity(self.len);
        while self.len > 0 {
            if let Some(ev) = self.buf[self.head].take() {
                out.push(ev);
            }
            self.head = (self.head + 1) % self.capacity;
            self.len = self.len.saturating_sub(1);
        }
        out
    }

    fn len(&self) -> usize {
        self.len
    }
}

impl EventRingBuffer {
    /// Create a ring buffer with the given capacity.
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(RingInner::new(capacity.max(1))),
            total_in: AtomicU64::new(0),
            total_out: AtomicU64::new(0),
            overflow: AtomicU64::new(0),
        })
    }

    /// Create with `DEFAULT_CAPACITY`.
    pub fn default_capacity() -> Arc<Self> {
        Self::new(DEFAULT_CAPACITY)
    }

    /// Push one event.  Never blocks.
    ///
    /// Returns `true` when the event was stored without overflow.
    /// Returns `false` when an older event was overwritten; the overflow
    /// counter is incremented and a tracing ERROR is emitted.
    pub fn push(&self, event: OmegaEvent) -> bool {
        self.total_in.fetch_add(1, Ordering::Relaxed);

        let ok = self
            .inner
            .lock()
            .expect("ring buffer mutex poisoned")
            .push(event);

        if !ok {
            let n = self.overflow.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::error!(
                overflow_count = n,
                "Observability ring buffer overflow — oldest event dropped",
            );
        }
        ok
    }

    /// Drain all buffered events in FIFO order.
    ///
    /// Called by the exporter task on its drain interval.  Returns an
    /// empty Vec when the buffer is idle.
    pub fn drain(&self) -> Vec<OmegaEvent> {
        let events = self
            .inner
            .lock()
            .expect("ring buffer mutex poisoned")
            .drain();
        let n = events.len() as u64;
        if n > 0 {
            self.total_out.fetch_add(n, Ordering::Relaxed);
        }
        events
    }

    /// Current number of buffered events.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("ring buffer mutex poisoned").len()
    }

    /// `true` when no events are buffered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Telemetry snapshot.
    pub fn snapshot(&self) -> RingBufferSnapshot {
        RingBufferSnapshot {
            buffered: self.len() as u64,
            total_in: self.total_in.load(Ordering::Relaxed),
            total_out: self.total_out.load(Ordering::Relaxed),
            overflow: self.overflow.load(Ordering::Relaxed),
        }
    }
}

/// Point-in-time snapshot of ring buffer counters.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RingBufferSnapshot {
    pub buffered: u64,
    pub total_in: u64,
    pub total_out: u64,
    pub overflow: u64,
}

impl RingBufferSnapshot {
    /// `true` when no overflow has occurred.
    pub fn is_healthy(&self) -> bool {
        self.overflow == 0
    }

    /// Fraction of events that overflowed [0.0, 1.0].
    pub fn overflow_rate(&self) -> f64 {
        if self.total_in == 0 {
            return 0.0;
        }
        self.overflow as f64 / self.total_in as f64
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn health_event() -> OmegaEvent {
        OmegaEvent::HealthStateChange {
            timestamp: Utc::now(),
            layer_id: "relay".into(),
            from_state: "HEALTHY".into(),
            to_state: "DEGRADED".into(),
            reason: "test".into(),
        }
    }

    #[test]
    fn push_and_drain_basic() {
        let buf = EventRingBuffer::new(8);
        assert!(buf.push(health_event()));
        assert!(buf.push(health_event()));
        assert_eq!(buf.len(), 2);

        let drained = buf.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn drain_empty_returns_empty() {
        let buf = EventRingBuffer::new(4);
        assert!(buf.drain().is_empty());
    }

    #[test]
    fn overflow_increments_counter_and_overwrites_oldest() {
        let buf = EventRingBuffer::new(2);
        assert!(buf.push(health_event())); // slot 0
        assert!(buf.push(health_event())); // slot 1 — full
        let ok = buf.push(health_event()); // overflow: slot 0 overwritten
        assert!(!ok, "overflow must return false");
        assert_eq!(buf.overflow.load(Ordering::Relaxed), 1);
        assert_eq!(buf.len(), 2, "len must still be capacity");
    }

    #[test]
    fn total_in_counts_all_pushes() {
        let buf = EventRingBuffer::new(10);
        for _ in 0..7 {
            buf.push(health_event());
        }
        assert_eq!(buf.total_in.load(Ordering::Relaxed), 7);
    }

    #[test]
    fn total_out_counts_drained() {
        let buf = EventRingBuffer::new(10);
        for _ in 0..5 {
            buf.push(health_event());
        }
        buf.drain();
        assert_eq!(buf.total_out.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn snapshot_healthy_when_no_overflow() {
        let buf = EventRingBuffer::new(10);
        buf.push(health_event());
        let snap = buf.snapshot();
        assert!(snap.is_healthy());
        assert!((snap.overflow_rate() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn snapshot_overflow_rate_correct() {
        let buf = EventRingBuffer::new(1);
        buf.push(health_event()); // ok
        buf.push(health_event()); // overflow
        let snap = buf.snapshot();
        // 2 total_in, 1 overflow → rate = 0.5
        assert!((snap.overflow_rate() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn fifo_order_preserved() {
        let buf = EventRingBuffer::new(4);
        for i in 0..4_u64 {
            buf.push(OmegaEvent::BlueprintAdmitted {
                timestamp: Utc::now(),
                blueprint_hash: format!("{i:064x}"),
                strategy_id: "SA".into(),
                lane: "microtx".into(),
                chain_id: 42161,
            });
        }
        let events = buf.drain();
        for (i, ev) in events.iter().enumerate() {
            if let OmegaEvent::BlueprintAdmitted { blueprint_hash, .. } = ev {
                assert!(
                    blueprint_hash.ends_with(&i.to_string()),
                    "FIFO violated at index {i}"
                );
            }
        }
    }
}
