// crates/omega-prl/benches/prl_throughput.rs
//!
//! PRL throughput and latency benchmarks — §22.1
//!
//! Performance targets:
//!   Event ingest latency  <5µs
//!   Feature extraction    <10µs
//!   Pattern scoring       <25µs
//!   ML inference          <50µs
//!   End-to-end signal     <80µs
//!   Throughput            >2M events/sec

use std::sync::Arc;

use arc_swap::ArcSwap;
use criterion::{criterion_group, criterion_main, Criterion, Throughput};

use omega_pattern_recognition::{
    features::extractor::{FeatureExtractor, FeatureVector, FEATURE_DIM},
    features::simd::{dot_product, weighted_score},
    governance::thresholds::ThresholdConfig,
    ingestion::event_bus::{EventBus, PatternEvent},
    ingestion::ring_buffer::{Consumer, LockFreeRingBuffer, Producer},
    ml::inference::{OnnxInferenceEngine, MODEL_GAS_WAR},
    patterns::matcher::PatternMatcher,
};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Build a synthetic gas-escalation event with a non-zero payload so the
/// extractor has real data to work with.
fn gas_escalation_event() -> PatternEvent {
    let mut ev = PatternEvent::zeroed();
    // Write base_fee=50, tip=150, tx_count=5 into the canonical payload layout.
    ev.payload[0..8].copy_from_slice(&50u64.to_le_bytes());
    ev.payload[8..16].copy_from_slice(&150u64.to_le_bytes());
    ev.payload[16..18].copy_from_slice(&5u16.to_le_bytes());
    ev
}

// ─────────────────────────────────────────────────────────────────────────────
// Event ingest — target <5µs per event, >2M/sec total
// ─────────────────────────────────────────────────────────────────────────────

fn bench_event_ingest(c: &mut Criterion) {
    let bus = EventBus::new(8, 1 << 20);
    let mut drain_buf = Vec::with_capacity(1024);

    let mut group = c.benchmark_group("event_ingest");
    group.throughput(Throughput::Elements(1));

    group.bench_function("single_event_publish", |b| {
        b.iter(|| {
            let ev = PatternEvent::zeroed();
            bus.publish(ev);
            // Drain inline so the ring never fills across iterations.
            bus.drain_shard(0, &mut drain_buf, 1024);
            drain_buf.clear();
        });
    });

    group.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// Ring buffer raw push/pop — target >5 Gelem/s
// ─────────────────────────────────────────────────────────────────────────────

fn bench_ring_buffer(c: &mut Criterion) {
    // `LockFreeRingBuffer` is the unsplit handle; push/pop live on the
    // `Producer` and `Consumer` halves returned by `.split()`.
    let (tx, rx): (Producer<u64>, Consumer<u64>) = LockFreeRingBuffer::new(1 << 16).split();

    let mut group = c.benchmark_group("ring_buffer");
    group.throughput(Throughput::Elements(1));

    group.bench_function("push_pop", |b| {
        b.iter(|| {
            // Push a value then immediately pop it so the buffer never fills.
            // black_box both sides to prevent the optimiser eliding either op.
            let _ = criterion::black_box(tx.push(criterion::black_box(42u64)));
            criterion::black_box(rx.pop())
        });
    });

    group.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// Feature extraction — target <10µs
// ─────────────────────────────────────────────────────────────────────────────

fn bench_feature_extraction(c: &mut Criterion) {
    let extractor = FeatureExtractor::new();
    let ev = gas_escalation_event();

    let mut group = c.benchmark_group("feature_extraction");
    group.throughput(Throughput::Elements(1));

    group.bench_function("gas_escalation_extract", |b| {
        b.iter(|| {
            criterion::black_box(extractor.extract(&ev));
        });
    });

    group.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// SIMD scoring — target <25µs
// ─────────────────────────────────────────────────────────────────────────────

fn bench_simd_scoring(c: &mut Criterion) {
    let a = FeatureVector::zeroed();
    let b_vec = FeatureVector::zeroed();
    let weights = [0.5f32; FEATURE_DIM];

    let mut group = c.benchmark_group("simd");
    group.throughput(Throughput::Elements(1));

    group.bench_function("dot_product_64f32", |b| {
        b.iter(|| {
            criterion::black_box(dot_product(&a, &b_vec));
        });
    });

    group.bench_function("weighted_score_64f32", |b| {
        b.iter(|| {
            criterion::black_box(weighted_score(&a, &weights));
        });
    });

    group.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// ML inference (heuristic fallback) — target <50µs
// ─────────────────────────────────────────────────────────────────────────────

fn bench_ml_inference(c: &mut Criterion) {
    let engine = OnnxInferenceEngine::heuristic_fallback();
    let fv = FeatureVector::zeroed();

    let mut group = c.benchmark_group("ml_inference");
    group.throughput(Throughput::Elements(1));

    group.bench_function("heuristic_fallback_gas_war", |b| {
        b.iter(|| {
            criterion::black_box(engine.infer(MODEL_GAS_WAR, &fv));
        });
    });

    group.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// Pattern matching end-to-end — target <80µs
// ─────────────────────────────────────────────────────────────────────────────

fn bench_pattern_matching(c: &mut Criterion) {
    let thresholds = Arc::new(ArcSwap::from_pointee(ThresholdConfig::default()));
    let matcher = PatternMatcher::new(thresholds);
    let extractor = FeatureExtractor::new();
    let ev = gas_escalation_event();

    let mut group = c.benchmark_group("pattern_matching");
    group.throughput(Throughput::Elements(1));

    group.bench_function("extract_and_match", |b| {
        b.iter(|| {
            if let Some(fv) = extractor.extract(&ev) {
                matcher.process(&fv);
                criterion::black_box(());
            }
        });
    });

    group.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// Registration
// ─────────────────────────────────────────────────────────────────────────────

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