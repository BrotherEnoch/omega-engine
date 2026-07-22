ï»¿// crates/omega-observability/src/lib.rs
//
// omega-observability â€” always-on telemetry layer (spec Â§16).
//
// ## Design constraint (Â§22.1)
//
//   omega-observability has deliberately minimal dependencies.  It is a
//   leaf node in the Â§22.1 dependency graph â€” it has NO dependents.  If
//   it fails or becomes slow, nothing else fails with it.
//
//   Inversion of the normal dependency direction: every other crate
//   imports omega-observability to emit events; omega-observability
//   imports nothing from the engine crates.
//
// ## Sampling contract (Â§16)
//
//   LA events are always-sampled (100%).  All other events use a
//   configurable `Sampler`.  The `OmegaEvent::is_always_sampled()` gate
//   is checked in `EventRingBuffer::push` so the always-sampled guarantee
//   is enforced at the ring-buffer layer, not at each call site.
//
// ## Event flow
//
//   ```text
//   any layer                        observability task
//   â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€    â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
//   OmegaEvent::emit_*(...)          OmegaExporter::run(buffer, config)
//     â†’ sampler.should_emit()           loop:
//     â†’ buffer.push(event)                events = buffer.drain()
//                                         for e in events: write_jsonl(e)
//   ```
//
// ## Module map
//
//   events.rs      â€” `OmegaEvent`: tagged union of all observable events.
//                    Includes `timestamp()`, `is_always_sampled()`, and
//                    `elk_index()` for ELK hot/warm routing (Â§16).
//
//   ring_buffer.rs â€” `EventRingBuffer`: lock-free ring buffer for
//                    high-frequency ingestion without back-pressure.
//                    Overflow increments a counter (never blocks callers).
//
//   sampler.rs     â€” `Sampler`: enforces 100% for LA events and a
//                    configurable rate for all other events.
//
//   exporter.rs    â€” `OmegaExporter`: async drain task that reads from
//                    the ring buffer and writes NDJSON to the structured
//                    log stream.  `ExporterConfig` controls flush interval
//                    and output path.  `ExporterStats` exposes lag metrics
//                    for the health monitor.

pub mod events;
pub mod exporter;
pub mod ring_buffer;
pub mod sampler;

// â”€â”€ Re-exports â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub use events::OmegaEvent;

pub use ring_buffer::{
    DEFAULT_CAPACITY,
    EventRingBuffer,
    RingBufferSnapshot,
};

pub use sampler::Sampler;

pub use exporter::{ExporterConfig, ExporterStats, OmegaExporter};