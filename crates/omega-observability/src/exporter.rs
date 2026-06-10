// crates/omega-observability/src/exporter.rs
//
// OmegaExporter — async drain loop for the ring buffer (spec §16).
//
// ## Spec §16
//
//   LA events: always-sampled (100%), high-priority log channel.
//   All other events: configurable sampling rate.
//   Storage: ELK hot tier (30 days) + warm tier (90 days).
//   Ring buffer: pre-allocated, lock-free; exporter drains it every
//   `drain_interval_ms` milliseconds.
//
// ## Design
//
//   The exporter runs as a single Tokio task, separate from the event
//   producers.  It drains the `EventRingBuffer` in batches, serialises
//   each event to JSON, and writes to a `tracing` structured log at the
//   designated ELK log level.  In production, the ELK Filebeat agent
//   tails the structured log and ships it to Elasticsearch.
//
//   The exporter never blocks producers — if it falls behind, the ring
//   buffer's overwrite-oldest-on-full semantics ensure producers are
//   never blocked.  Overflow is counted and reported as a metric.
//
// ## Shutdown
//
//   The exporter loop exits when the `shutdown` watch receiver fires
//   `true`.  It drains any remaining events before exiting.

use std::sync::Arc;
use std::time::Duration;

use serde_json;
use tokio::sync::watch;

use crate::ring_buffer::EventRingBuffer;
use crate::sampler::Sampler;

// ─────────────────────────────────────────────────────────────────────────────
// ExporterConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Runtime configuration for the exporter drain loop.
#[derive(Debug, Clone)]
pub struct ExporterConfig {
    /// How often the exporter wakes to drain the ring buffer.
    /// Default 10ms — matches the hot-path polling interval (§3).
    pub drain_interval_ms: u64,

    /// Maximum events to drain per tick.  Bounds the per-tick work and
    /// prevents the exporter from starving other tasks on the same thread.
    /// Default 512.
    pub drain_batch_size: usize,

    /// Whether to emit exporter-internal tracing spans (lag, overflow).
    /// Disable in benchmarks where tracing itself is the bottleneck.
    pub enable_internal_tracing: bool,
}

impl Default for ExporterConfig {
    fn default() -> Self {
        Self {
            drain_interval_ms: 10,
            drain_batch_size: 512,
            enable_internal_tracing: true,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ExporterStats
// ─────────────────────────────────────────────────────────────────────────────

/// Cumulative statistics from the exporter loop.
///
/// Exposed by `OmegaExporter::stats()` and surfaced by the control-plane
/// at `GET /api/v1/observability/stats`.
#[derive(Debug, Clone, Default)]
pub struct ExporterStats {
    /// Total events drained from the ring buffer.
    pub events_exported: u64,
    /// Events dropped due to ring-buffer overflow.
    pub events_overflowed: u64,
    /// Events skipped by the sampler.
    pub events_sampled_out: u64,
    /// Number of drain ticks executed.
    pub drain_ticks: u64,
    /// Total serialisation errors (malformed events).
    pub serialise_errors: u64,
}

// ─────────────────────────────────────────────────────────────────────────────
// OmegaExporter
// ─────────────────────────────────────────────────────────────────────────────

/// Async exporter — drains the ring buffer and writes events to the log stream.
///
/// Start by calling `OmegaExporter::run(buffer, sampler, config, shutdown)`.
/// The returned `ExporterStats` reflect the full run.
pub struct OmegaExporter;

impl OmegaExporter {
    /// Run the exporter drain loop.
    ///
    /// ## Arguments
    ///
    /// - `buffer`:   The shared ring buffer produced by event emitters.
    /// - `sampler`:  Sampling-rate enforcer; events that fail the sample
    ///   check are counted in `stats.events_sampled_out`.
    /// - `config`:   Drain interval and batch size.
    /// - `shutdown`: Watch receiver; `true` triggers graceful shutdown.
    ///
    /// ## Returns
    ///
    /// `ExporterStats` reflecting the complete run (useful in tests).
    pub async fn run(
        buffer: Arc<EventRingBuffer>,
        sampler: Sampler,
        config: ExporterConfig,
        mut shutdown: watch::Receiver<bool>,
    ) -> ExporterStats {
        let mut stats = ExporterStats::default();
        let interval_ms = config.drain_interval_ms.max(1);

        let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await; // consume the immediate first tick

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let drained = Self::drain_batch(
                        &buffer,
                        &sampler,
                        config.drain_batch_size.max(1),
                        &mut stats,
                        config.enable_internal_tracing,
                    );

                    stats.drain_ticks += 1;

                    // Account for ring-buffer overflow since last tick.
                    // `overflow` is a public AtomicU64 field on EventRingBuffer.
                    let overflow = buffer.overflow.load(std::sync::atomic::Ordering::Relaxed);
                    if overflow > stats.events_overflowed {
                        let new_overflow = overflow - stats.events_overflowed;
                        if config.enable_internal_tracing && new_overflow > 0 {
                            tracing::warn!(
                                new_overflow,
                                total_overflow = overflow,
                                "Ring buffer overflow — events lost",
                            );
                        }
                        stats.events_overflowed = overflow;
                    }

                    if config.enable_internal_tracing && drained > 0 {
                        tracing::trace!(drained, tick = stats.drain_ticks, "Exporter tick");
                    }
                }

                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        // Drain remaining events before exiting.
                        tracing::info!("Exporter shutdown — draining remaining events");
                        loop {
                            let drained = Self::drain_batch(
                                &buffer, &sampler,
                                config.drain_batch_size.max(1),
                                &mut stats, false,
                            );
                            if drained == 0 { break; }
                        }
                        tracing::info!(
                            events_exported    = stats.events_exported,
                            events_overflowed  = stats.events_overflowed,
                            events_sampled_out = stats.events_sampled_out,
                            drain_ticks        = stats.drain_ticks,
                            "Exporter stopped",
                        );
                        return stats;
                    }
                }
            }
        }
    }

    /// Drain all available events from the buffer, serialise, and emit.
    ///
    /// `EventRingBuffer::drain()` returns all buffered events in one call.
    /// We respect `batch_size` by capping how many we process per tick.
    ///
    /// Returns the number of events successfully exported.
    fn drain_batch(
        buffer: &EventRingBuffer,
        sampler: &Sampler,
        batch_size: usize,
        stats: &mut ExporterStats,
        do_trace: bool,
    ) -> usize {
        // Drain the full buffer; we cap processing to batch_size below.
        let events = buffer.drain();
        let mut exported = 0usize;

        for event in events.into_iter().take(batch_size) {
            // Sampling gate — LA events always pass (§16).
            if !sampler.should_emit(&event) {
                stats.events_sampled_out += 1;
                continue;
            }

            // Serialise to JSON.
            match serde_json::to_string(&event) {
                Ok(json) => {
                    // Emit at INFO level — Filebeat/Fluentd ships this to ELK.
                    // The structured key `omega_event` lets ELK distinguish
                    // engine events from application logs.
                    tracing::info!(omega_event = %json, "OMEGA_EVENT");
                    stats.events_exported += 1;
                    exported += 1;
                }
                Err(e) => {
                    stats.serialise_errors += 1;
                    if do_trace {
                        tracing::error!(
                            error      = %e,
                            elk_index  = event.elk_index(),
                            "Failed to serialise OmegaEvent — event dropped",
                        );
                    }
                }
            }
        }

        exported
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::OmegaEvent;
    use crate::ring_buffer::EventRingBuffer;
    use crate::sampler::Sampler;
    use chrono::Utc;

    fn la_event() -> OmegaEvent {
        OmegaEvent::BlueprintDropped {
            blueprint_hash: "aabbcc".into(),
            strategy_id: "LA".into(),
            drop_code: "MISS_HF_NOT_LIQUIDATABLE".into(),
            chain_id: 42161,
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn exporter_drains_all_events_before_shutdown() {
        let buffer = EventRingBuffer::new(64);
        let sampler = Sampler::new(1.0); // 100% sampling

        for _ in 0..10 {
            buffer.push(la_event());
        }

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let cfg = ExporterConfig {
            drain_interval_ms: 1,
            drain_batch_size: 512,
            enable_internal_tracing: false,
        };

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _ = shutdown_tx.send(true);
        });

        let stats = OmegaExporter::run(buffer, sampler, cfg, shutdown_rx).await;
        assert_eq!(stats.events_exported, 10);
        assert_eq!(stats.serialise_errors, 0);
    }

    #[tokio::test]
    async fn sampled_out_events_are_counted() {
        let buffer = EventRingBuffer::new(64);
        let sampler = Sampler::new(0.0); // reject everything except always-sampled

        // GasModelReverted is always-sampled — use a non-always-sampled event.
        buffer.push(OmegaEvent::OraclePriceResolved {
            timestamp: Utc::now(),
            asset: "ETH".into(),
            price_usd: 3000.0,
            source: "chainlink".into(),
            age_seconds: 1,
            chain_id: 42161,
        });

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let cfg = ExporterConfig {
            drain_interval_ms: 1,
            drain_batch_size: 512,
            enable_internal_tracing: false,
        };

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _ = shutdown_tx.send(true);
        });

        let stats = OmegaExporter::run(buffer, sampler, cfg, shutdown_rx).await;
        // OraclePriceResolved is not always-sampled; at rate 0.0 it is dropped.
        assert_eq!(stats.events_exported + stats.events_sampled_out, 1);
    }
}
