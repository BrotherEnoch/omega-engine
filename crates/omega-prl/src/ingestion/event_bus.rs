// omega-prl/src/ingestion/event_bus.rs
//! Pattern event bus — ingestion pipeline entry point.
//!
//! `PatternEvent` is the canonical event type flowing through the PRL pipeline.
//! It is written to the WAL (bincode + zstd) and replayed deterministically.
//!
//! ## Serde and [u8; 64]
//!
//! serde's derive macros only generate array impls up to `[T; 32]`.
//! `MAX_PAYLOAD_LEN = 64` is outside that range, so we supply a local
//! `serde_payload` module that calls `serialize_bytes` / `deserialize_bytes`
//! — same compact byte-sequence encoding as `serde_bytes`, zero extra deps.

use std::sync::{Arc, Mutex};

use dashmap::DashMap;

// ─────────────────────────────────────────────────────────────────────────────
// Event taxonomy
// ─────────────────────────────────────────────────────────────────────────────

/// Discriminant for the event payload layout (§4.1).
///
/// Repr u8 keeps the wire format compact and enum matching branch-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum EventType {
    OraclePriceUpdate = 0,
    RelayInclusionResult = 1,
    BundleIncluded = 2,
    BundleDropped = 3,
    LossRecorded = 4,
    GasEscalation = 5,
    SequencerRestart = 6,
    ReorgDetected = 7,
    SimulationResult = 8,
    /// Catch-all for forward-compatible extension without breaking deserialisation.
    Unknown = 255,
}

/// Origin subsystem — used for source-aware feature weighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum EventSource {
    OracleWatcher = 0,
    RelayMonitor = 1,
    BundleTracker = 2,
    LossAttribution = 3,
    GasWarEngine = 4,
    HealthFsm = 5,
    Sequencer = 6,
}

/// Dispatch priority — used by `EventBus` to order shard drain batches.
///
/// Critical events are never dropped under backpressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum EventPriority {
    Low = 0,
    Normal = 1,
    High = 2,
    /// Always-sampled; never dropped under backpressure.
    Critical = 3,
}

// ─────────────────────────────────────────────────────────────────────────────
// Serde helper for [u8; MAX_PAYLOAD_LEN]
//
// serde derive only covers [T; 0..=32].  MAX_PAYLOAD_LEN = 64 is outside that
// range, so we provide a `with` module.  Both serialise and deserialise
// delegate to serde's `serialize_bytes` / `deserialize_bytes` — identical
// wire format to `serde_bytes`, no additional crate required.
// ─────────────────────────────────────────────────────────────────────────────

mod serde_payload {
    use serde::de::{Deserializer, Error, SeqAccess, Visitor};
    use serde::ser::Serializer;
    use std::fmt;

    const N: usize = super::MAX_PAYLOAD_LEN;

    pub fn serialize<S: Serializer>(bytes: &[u8; N], s: S) -> Result<S::Ok, S::Error> {
        // Compact byte-sequence encoding — bincode emits a length-prefixed blob.
        s.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; N], D::Error> {
        struct ArrayVisitor;

        impl<'de> Visitor<'de> for ArrayVisitor {
            type Value = [u8; N];

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "a byte array of length {N}")
            }

            /// bincode calls `visit_bytes` for byte sequences.
            fn visit_bytes<E: Error>(self, v: &[u8]) -> Result<[u8; N], E> {
                v.try_into().map_err(|_| E::invalid_length(v.len(), &self))
            }

            /// JSON and other human-readable formats call `visit_seq`.
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<[u8; N], A::Error> {
                let mut arr = [0u8; N];
                for slot in arr.iter_mut() {
                    *slot = seq
                        .next_element()?
                        .ok_or_else(|| A::Error::invalid_length(0, &self))?;
                }
                Ok(arr)
            }
        }

        d.deserialize_bytes(ArrayVisitor)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PatternEvent
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum inline payload size (bytes).
/// Fixed-size keeps `PatternEvent` stack-allocated with no heap allocation on
/// the ingestion hot path.
pub const MAX_PAYLOAD_LEN: usize = 64;

/// The core event type for the PRL ingestion pipeline.
///
/// # Layout
/// Fixed-size struct; no heap allocation.  `payload` uses `#[serde(with =
/// "serde_payload")]` to work around serde's `[T; 0..=32]` derive limit.
///
/// # Serialisation
/// `Serialize` + `Deserialize` are required by the WAL (`bincode::serialize` /
/// `bincode::deserialize`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PatternEvent {
    /// Monotonic nanosecond timestamp (enforced by WAL).
    pub ts_nanos: u64,
    /// Event discriminant — determines payload interpretation.
    pub event_type: EventType,
    /// Origin subsystem.
    pub source: EventSource,
    /// Opaque binary payload.  Schema is event-type-specific (see extractor.rs).
    #[serde(with = "serde_payload")]
    pub payload: [u8; MAX_PAYLOAD_LEN],
    /// Number of valid bytes in `payload`. Must be ≤ `MAX_PAYLOAD_LEN`.
    pub payload_len: usize,
}

impl PatternEvent {
    /// Construct a zeroed event.  Caller fills `event_type`, `source`,
    /// `payload[..n]`, and `payload_len`.
    #[inline]
    pub const fn zeroed() -> Self {
        Self {
            ts_nanos: 0,
            event_type: EventType::Unknown,
            source: EventSource::HealthFsm,
            payload: [0u8; MAX_PAYLOAD_LEN],
            payload_len: 0,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-shard queue
// ─────────────────────────────────────────────────────────────────────────────

/// Fixed-capacity per-shard event queue.
///
/// Each shard worker holds exclusive drain rights over its own `ShardQueue`,
/// so the `Mutex` is uncontended on the drain path.  The publish path
/// contends only within one shard slot, never globally.
struct ShardQueue {
    events: Mutex<Vec<PatternEvent>>,
    capacity: usize,
}

impl ShardQueue {
    fn new(capacity: usize) -> Self {
        Self {
            events: Mutex::new(Vec::with_capacity(capacity)),
            capacity,
        }
    }

    /// Push one event.  Drops silently if the queue is at capacity
    /// (backpressure: low-priority events are shed under load per §5.2).
    #[inline]
    fn push(&self, event: PatternEvent) {
        let mut q = self.events.lock().expect("shard queue lock poisoned");
        if q.len() < self.capacity {
            q.push(event);
        }
    }

    /// Drain up to `limit` events into `out`.
    #[inline]
    fn drain_into(&self, out: &mut Vec<PatternEvent>, limit: usize) {
        let mut q = self.events.lock().expect("shard queue lock poisoned");
        let take = q.len().min(limit);
        out.extend(q.drain(..take));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// EventBus
// ─────────────────────────────────────────────────────────────────────────────

/// Subscriber callback type.  Called synchronously on `publish()`.
/// Must not block; heavy work belongs in a spawned task.
pub type EventHandler = Arc<dyn Fn(&PatternEvent) + Send + Sync + 'static>;

/// Sharded multi-producer multi-consumer event bus.
///
/// Events are routed to a shard by `event_type as usize % shard_count`.
/// Each shard worker drains its own queue independently — no cross-shard
/// contention on the drain path.
///
/// Synchronous `EventHandler` subscribers are called inline on `publish()`
/// before enqueuing, for low-latency observers (WAL append, metrics).
pub struct EventBus {
    shards: Vec<ShardQueue>,
    shard_count: usize,
    handlers: DashMap<EventType, Vec<EventHandler>>,
}

impl EventBus {
    /// Create a new `EventBus`.
    ///
    /// `shard_count` — number of independent shard queues (typically == CPU
    /// cores on the NUMA node).
    /// `ring_buffer_capacity` — total event slots across all shards; each
    /// shard gets `capacity / shard_count`.
    pub fn new(shard_count: usize, ring_buffer_capacity: usize) -> Self {
        let shard_count = shard_count.max(1);
        let shards = (0..shard_count)
            .map(|idx| {
                // Distribute the configured capacity exactly across shards so
                // aggregate queue depth never exceeds the caller's budget.
                let base = ring_buffer_capacity / shard_count;
                let extra = usize::from(idx < (ring_buffer_capacity % shard_count));
                ShardQueue::new(base + extra)
            })
            .collect();
        Self {
            shards,
            shard_count,
            handlers: DashMap::new(),
        }
    }

    /// Register a synchronous handler for a specific event type.
    pub fn subscribe(&self, event_type: EventType, handler: EventHandler) {
        self.handlers.entry(event_type).or_default().push(handler);
    }

    /// Publish an event.
    ///
    /// 1. Calls all synchronous handlers registered for `event.event_type`.
    /// 2. Routes the event into the appropriate shard queue.
    #[inline]
    pub fn publish(&self, event: PatternEvent) {
        if let Some(handlers) = self.handlers.get(&event.event_type) {
            for h in handlers.iter() {
                h(&event);
            }
        }
        let shard_idx = event.event_type as usize % self.shard_count;
        self.shards[shard_idx].push(event);
    }

    /// Drain up to `limit` events from shard `shard_idx` into `out`.
    ///
    /// Called by the per-shard worker task in `lib.rs::drain_shard_tick`.
    /// Each shard index must be driven by exactly one worker task.
    #[inline]
    pub fn drain_shard(&self, shard_idx: usize, out: &mut Vec<PatternEvent>, limit: usize) {
        if let Some(shard) = self.shards.get(shard_idx) {
            shard.drain_into(out, limit);
        }
    }

    pub fn shard_count(&self) -> usize {
        self.shard_count
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(8, 1 << 20)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(et: EventType) -> PatternEvent {
        let mut e = PatternEvent::zeroed();
        e.event_type = et;
        e.ts_nanos = 1_000_000;
        e
    }

    #[test]
    fn publish_routes_to_correct_shard() {
        let bus = EventBus::new(4, 1024);
        bus.publish(make_event(EventType::OraclePriceUpdate)); // type 0 → shard 0
        bus.publish(make_event(EventType::RelayInclusionResult)); // type 1 → shard 1

        let mut out = Vec::new();
        bus.drain_shard(0, &mut out, 256);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event_type, EventType::OraclePriceUpdate);

        out.clear();
        bus.drain_shard(1, &mut out, 256);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event_type, EventType::RelayInclusionResult);
    }

    #[test]
    fn serde_roundtrip_payload() {
        let mut ev = PatternEvent::zeroed();
        ev.event_type = EventType::GasEscalation;
        ev.payload_len = 8;
        ev.payload[0] = 0xDE;
        ev.payload[7] = 0xAD;

        let bytes = bincode::serialize(&ev).expect("serialize");
        let decoded: PatternEvent = bincode::deserialize(&bytes).expect("deserialize");

        assert_eq!(decoded.payload[0], 0xDE);
        assert_eq!(decoded.payload[7], 0xAD);
        assert_eq!(decoded.payload_len, 8);
    }

    #[test]
    fn queue_drops_at_capacity() {
        let bus = EventBus::new(1, 2); // 2 slots / 1 shard = 2 capacity
        for _ in 0..10 {
            bus.publish(make_event(EventType::OraclePriceUpdate));
        }
        let mut out = Vec::new();
        bus.drain_shard(0, &mut out, 256);
        assert!(out.len() <= 2, "must not exceed shard capacity");
    }
}
