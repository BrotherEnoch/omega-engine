// crates/omega-prl/benches/prl_throughput.rs
//!
//! PRL throughput and latency benchmarks â€” Â§22.1
//!
//! Performance targets:
//!   Event ingest latency  <5Âµs
//!   Feature extraction    <10Âµs
//!   Pattern scoring       <25Âµs
//!   ML inference          <50Âµs
//!   End-to-end signal     <80Âµs
//!   Throughput            >2M events/sec

use std::sync::Arc;

use arc_swap::ArcSwap;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use quanta::Clock;

use omega_pattern_recognition::{
    features::extractor::{FeatureExtractor, FeatureVector, FEATURE_DIM},
    features::simd::{dot_product, weighted_score},
    governance::thresholds::ThresholdConfig,
    ingestion::event_bus::{EventBus, EventSource, EventType, PatternEvent},
    ingestion::ring_buffer::LockFreeRingBuffer,
    ml::inference::{OnnxInferenceEngine, MODEL_GAS_WAR},
    patterns::matcher::PatternMatcher,
};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Helpers
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn gas_escalation_event(clock: &Clock) -> PatternEvent {
    let mut payload = [0u8; 256];
    payload[0..8].copy_from_slice(&50u64.to_le_bytes());
    payload[8..16].copy_from_slice(&150u64.to_le_bytes());
    payload[16..18].copy_from_slice(&5u16.to_le_bytes());
    PatternEvent::new(
        clock,
        EventSource::GasEstimation,
        EventType::GasEscalation,
        42161,
        1,
        &payload[..18],
    )
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Event ingest â€” target <5Âµs per event, >2M/sec total
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn bench_event_ingest(c: &mut Criterion) {
    let bus   = EventBus::new(8, 1 << 20);
    let clock = Clock::new();

    let mut group = c.benchmark_group("event_ingest");
    group.throughput(Throughput::Elements(1));

    group.bench_function("single_event_publish", |b| {
        b.iter(|| {
            let ev = PatternEvent::new(
                &clock,
                EventSource::OracleTick,
                EventType::OraclePriceUpdate,
                42161,
                1_000_000,
                &[],
            );
            bus.publish(ev);
        });
    });

    // Drain to prevent overflow across iterations
    let mut buf = Vec::with_capacity(1024);
    bus.drain_shard(0, &mut buf, 1024);

    group.finish();
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Ring buffer raw push/pop
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn bench_ring_buffer(c: &mut Criterion) {
    let ring: LockFreeRingBuffer<u64> = LockFreeRingBuffer::new(1 << 16);
    let mut group = c.benchmark_group("ring_buffer");
    group.throughput(Throughput::Elements(1));

    group.bench_function("push_pop", |b| {
        b.iter(|| {
            let _ = ring.push(42u64);
            let _ = ring.pop();
        });
    });

    group.finish();
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Feature extraction â€” target <10Âµs
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn bench_feature_extraction(c: &mut Criterion) {
    let extractor = FeatureExtractor::new();
    let clock     = Clock::new();
    let ev        = gas_escalation_event(&clock);

    let mut group = c.benchmark_group("feature_extraction");
    group.throughput(Throughput::Elements(1));

    group.bench_function("gas_escalation_extract", |b| {
        b.iter(|| {
            criterion::black_box(extractor.extract(&ev));
        });
    });

    group.finish();
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// SIMD scoring â€” target <25Âµs
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn bench_simd_scoring(c: &mut Criterion) {
    let a       = FeatureVector::zeroed();
    let b       = FeatureVector::zeroed();
    let weights = [0.5f32; FEATURE_DIM];

    let mut group = c.benchmark_group("simd");
    group.throughput(Throughput::Elements(1));

    group.bench_function("dot_product_64f32", |b_crit| {
        b_crit.iter(|| {
            criterion::black_box(dot_product(&a, &b));
        });
    });

    group.bench_function("weighted_score_64f32", |b_crit| {
        b_crit.iter(|| {
            criterion::black_box(weighted_score(&a, &weights));
        });
    });

    group.finish();
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ML inference (heuristic fallback) â€” target <50Âµs
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn bench_ml_inference(c: &mut Criterion) {
    let engine = OnnxInferenceEngine::heuristic_fallback();
    let fv     = FeatureVector::zeroed();

    let mut group = c.benchmark_group("ml_inference");
    group.throughput(Throughput::Elements(1));

    group.bench_function("heuristic_fallback_gas_war", |b| {
        b.iter(|| {
            criterion::black_box(engine.infer(MODEL_GAS_WAR, &fv));
        });
    });

    group.finish();
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Pattern matching end-to-end â€” target <80Âµs
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn bench_pattern_matching(c: &mut Criterion) {
    let thresholds = Arc::new(ArcSwap::from_pointee(ThresholdConfig::default()));
    let matcher    = PatternMatcher::new(thresholds);
    let clock      = Clock::new();
    let extractor  = FeatureExtractor::new();
    let ev         = gas_escalation_event(&clock);

    let mut group = c.benchmark_group("pattern_matching");
    group.throughput(Throughput::Elements(1));

    group.bench_function("extract_and_match", |b| {
        b.iter(|| {
            if let Some(fv) = extractor.extract(&ev) {
                matcher.process(&fv);
            }
        });
    });

    group.finish();
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Registration
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

criterion_group!(
    benches,
    bench_event_ingest,
    bench_ring_buffer,
    bench_feature_extraction,
    bench_simd_scoring,
    bench_ml_inference,
    bench_pattern_matching,
);
criterion_main!(benches);