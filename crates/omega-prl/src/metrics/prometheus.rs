// omega-prl/src/metrics/prometheus.rs
//! PRL Prometheus metrics — §19.1
//!
//! Metric names match spec §19.1 exactly.

use prometheus::{
    Gauge, Histogram, HistogramOpts, IntCounter, IntCounterVec, IntGauge, Opts, Registry,
};
use tracing::{error, warn};

use crate::metrics::events::ObservabilityEvent;

/// All PRL Prometheus metrics (§19.1).
pub struct PrlMetrics {
    pub events_ingested: IntCounter,
    pub pattern_matches: IntCounterVec,
    pub replay_divergence_total: IntCounter,
    pub ml_fallback_total: IntCounter,
    pub relay_anomaly_total: IntCounter,
    pub inference_latency_us: Histogram,
    pub confidence_avg: Gauge,
    pub queue_depth: IntGauge,
    registry: Registry,
}

impl PrlMetrics {
    pub fn new() -> anyhow::Result<Self> {
        let registry = Registry::new_custom(Some("omega".into()), None)?;

        let events_ingested = IntCounter::with_opts(Opts::new(
            "prl_events_ingested_total",
            "Total events ingested by the PRL",
        ))?;
        registry.register(Box::new(events_ingested.clone()))?;

        let pattern_matches = IntCounterVec::new(
            Opts::new("prl_pattern_matches_total", "Pattern matches by domain"),
            &["domain"],
        )?;
        registry.register(Box::new(pattern_matches.clone()))?;

        let inference_latency_us = Histogram::with_opts(
            HistogramOpts::new(
                "prl_inference_latency_us",
                "ML inference latency in microseconds",
            )
            .buckets(vec![
                1.0, 5.0, 10.0, 20.0, 30.0, 40.0, 50.0, 75.0, 100.0, 200.0,
            ]),
        )?;
        registry.register(Box::new(inference_latency_us.clone()))?;

        let confidence_avg = Gauge::with_opts(Opts::new(
            "prl_confidence_avg",
            "Average confidence across active patterns",
        ))?;
        registry.register(Box::new(confidence_avg.clone()))?;

        let queue_depth = IntGauge::with_opts(Opts::new(
            "prl_queue_depth",
            "Current event queue depth across all shards",
        ))?;
        registry.register(Box::new(queue_depth.clone()))?;

        let replay_divergence_total = IntCounter::with_opts(Opts::new(
            "prl_replay_divergence_total",
            "Total replay divergences detected",
        ))?;
        registry.register(Box::new(replay_divergence_total.clone()))?;

        let ml_fallback_total = IntCounter::with_opts(Opts::new(
            "prl_ml_fallback_total",
            "Total ML-to-heuristic fallbacks",
        ))?;
        registry.register(Box::new(ml_fallback_total.clone()))?;

        let relay_anomaly_total = IntCounter::with_opts(Opts::new(
            "prl_relay_anomaly_total",
            "Total relay anomaly detections",
        ))?;
        registry.register(Box::new(relay_anomaly_total.clone()))?;

        Ok(Self {
            events_ingested,
            pattern_matches,
            replay_divergence_total,
            ml_fallback_total,
            relay_anomaly_total,
            inference_latency_us,
            confidence_avg,
            queue_depth,
            registry,
        })
    }

    pub fn gather_text(&self) -> String {
        use prometheus::Encoder;
        let encoder = prometheus::TextEncoder::new();
        let mf = self.registry.gather();
        let mut buf = Vec::new();
        encoder.encode(&mf, &mut buf).unwrap_or_default();
        String::from_utf8_lossy(&buf).into_owned()
    }

    pub fn set_queue_depth(&self, depth: i64) {
        self.queue_depth.set(depth);
    }
    pub fn set_confidence_avg(&self, avg: f64) {
        self.confidence_avg.set(avg);
    }
    pub fn inc_pattern_match(&self, domain: &str) {
        self.pattern_matches.with_label_values(&[domain]).inc();
    }

    /// Emit an always-sampled observability event and update counters (§19.2).
    pub fn emit_always_sampled(
        &self,
        event: ObservabilityEvent,
        relay_id: Option<u32>,
        description: &str,
    ) {
        match event.priority() {
            "CRITICAL" => error!(
                event = event.name(),
                priority = "CRITICAL",
                relay_id = relay_id.unwrap_or(0),
                description,
                "PRL always-sampled event"
            ),
            _ => warn!(
                event = event.name(),
                priority = "HIGH",
                description,
                "PRL always-sampled event"
            ),
        }
        match event {
            ObservabilityEvent::RelayLeakSuspected => self.relay_anomaly_total.inc(),
            ObservabilityEvent::PatternModelReverted => self.inc_pattern_match("model_revert"),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialises_without_error() {
        let m = PrlMetrics::new().unwrap();
        m.events_ingested.inc();
        assert_eq!(m.events_ingested.get(), 1);
    }

    #[test]
    fn exposition_contains_metric_name() {
        let m = PrlMetrics::new().unwrap();
        m.events_ingested.inc();
        assert!(m.gather_text().contains("prl_events_ingested_total"));
    }

    #[test]
    fn relay_anomaly_increments_on_leak_event() {
        let m = PrlMetrics::new().unwrap();
        m.emit_always_sampled(ObservabilityEvent::RelayLeakSuspected, Some(1), "test");
        assert_eq!(m.relay_anomaly_total.get(), 1);
    }
}
