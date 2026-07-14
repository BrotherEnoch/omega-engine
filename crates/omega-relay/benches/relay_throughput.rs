// crates/omega-relay/benches/relay_throughput.rs
//!
//! Measures cascade submission throughput and single-bundle submission latency
//! under mock relay clients with zero network overhead.
//!
//! CHANGE: `CascadeSubmitter::new` now takes an `InclusionTracker` (see
//! `confirmation.rs`) — added here purely to keep this benchmark compiling; a
//! dummy tracker pointed at a throwaway URL is fine since these benchmarks only
//! measure submission throughput, not confirmation behavior.

use std::collections::HashMap;
use std::sync::Arc;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use tokio::runtime::Runtime;

use omega_relay::{
    backpressure::CascadeSubmitter,
    client::{BundlePayload, MockRelayClient, RelayClient},
    config::{RelayConfig, RelayName},
    confirmation::InclusionTracker,
    metrics::{ExecutionAddress, LaRelayMetrics},
    reputation::submission_order, PositionKey,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn seeded_metrics(addr: &str) -> Arc<LaRelayMetrics> {
    let m = LaRelayMetrics::new(200, ExecutionAddress(addr.into()));
    for i in 0..100 {
        m.record(&RelayName::Flashbots, i < 90);
        m.record(&RelayName::Bloxroute, i < 85);
        m.record(&RelayName::Titan, i < 70);
        m.record(&RelayName::Eden, i < 60);
    }
    m
}

fn four_relay_clients() -> HashMap<String, Arc<dyn RelayClient>> {
    [
        ("flashbots", true),
        ("bloxroute", true),
        ("titan", false),
        ("eden", false),
    ]
    .iter()
    .map(|(name, inc)| {
        (
            name.to_string(),
            Arc::new(MockRelayClient::new(*inc)) as Arc<dyn RelayClient>,
        )
    })
    .collect()
}

fn no_stagger_cfg() -> RelayConfig {
    RelayConfig {
        stagger_ms: 0,
        max_bundles_per_relay_per_second: 10_000,
        ..Default::default()
    }
}

fn bundle(i: usize) -> BundlePayload {
    BundlePayload {
        bundle_hash: format!("0x{i:064x}"),
        txs: vec!["0xdeadbeef".into()],
        block_number: "0x1".into(),
        priority_fee_gwei: 100,
        ..Default::default()
    }
}

// ── Benchmarks ────────────────────────────────────────────────────────────────

fn bench_cascade_throughput(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("cascade_submit");

    for bundle_count in [1usize, 4, 16, 64] {
        group.throughput(Throughput::Elements(bundle_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(bundle_count),
            &bundle_count,
            |b, &count| {
                let clients = Arc::new(four_relay_clients());
                let metrics = seeded_metrics("0xBENCH");
                let cfg = no_stagger_cfg();
                // Dummy tracker — these benchmarks measure submission throughput, not
                // confirmation behavior, so a real RPC endpoint isn't needed here.
                let tracker = InclusionTracker::new("http://localhost:1");
                let submitter =
                    CascadeSubmitter::new(Arc::clone(&clients), Arc::clone(&metrics), &cfg, tracker);

                b.to_async(&rt).iter_batched(
                    || (0..count).map(bundle).collect::<Vec<_>>(),
                    |bundles| submitter.submit_cascade(bundles),
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn bench_submission_order(c: &mut Criterion) {
    let metrics = seeded_metrics("0xORDER");
    c.bench_function("submission_order_4_relays", |b| {
        b.iter(|| submission_order(&metrics));
    });
}

fn bench_dedup_try_submit(c: &mut Criterion) {
    use omega_relay::SequencerRestartHandler;
    let handler = SequencerRestartHandler::new(0);
    let mut counter = 0u64;

    c.bench_function("dedup_try_submit", |b| {
        b.iter(|| {
            counter += 1;
            let pos = PositionKey::from_bytes(&counter.to_le_bytes());
            let _ = handler.try_submit(&pos, counter);
        });
    });
}

fn bench_ranked_relays(c: &mut Criterion) {
    let metrics = seeded_metrics("0xRANK");
    c.bench_function("la_ranked_relays_4", |b| {
        b.iter(|| metrics.la_ranked_relays());
    });
}

// ── Registration ──────────────────────────────────────────────────────────────

criterion_group!(
    relay_benches,
    bench_cascade_throughput,
    bench_submission_order,
    bench_dedup_try_submit,
    bench_ranked_relays,
);

criterion_main!(relay_benches);