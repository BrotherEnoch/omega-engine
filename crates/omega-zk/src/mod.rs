// crates/omega-zk/src/tests/mod.rs
//
// Integration test suite for omega-zk.
//
// Tests here exercise cross-module wiring:
//   1. Config spec constants match exactly.
//   2. Commitment → verify_commitment roundtrip.
//   3. Queue pressure FSM all state transitions.
//   4. Queue: suspend rejects normal, accepts microtx.
//   5. Queue: halt rejects all.
//   6. Concurrent queue submissions from multiple tasks.
//   7. Prover produces valid proof bytes.
//   8. ProofWorkerPool: end-to-end prove via queue.
//   9. Checkpoint: record → restart → recover → re-queue.
//  10. QueuePressure::is_halt_worthy / is_degraded_worthy.
//  11. ZkError::is_halt_worthy / is_degraded_worthy categorisation.
//  12. Metrics: register_all() smoke test.
//  13. ZkConfig::sla_ms dispatches correctly.
//  14. ProofCommitment display and equality.
//  15. Worker pool drains queue to zero after completing all proofs.

use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

use crate::checkpoint::ProofCheckpointManager;
use crate::commitment::{compute_proof_commitment, verify_commitment};
use crate::config::{
    ZkConfig, MICROTX_SLA_MS, NORMAL_SLA_MS,
    QUEUE_HALT_DEPTH, QUEUE_SUSPEND_DEPTH, QUEUE_THROTTLE_DEPTH,
};
use crate::error::ZkError;
use crate::metrics;
use crate::prover::T1SoftwareProver;
use crate::queue::{ProofQueue, QueuePressure};
use crate::worker::ProofWorkerPool;

// ─── Test helpers ─────────────────────────────────────────────────────────────

fn hash(b: u8) -> [u8; 32] { [b; 32] }

fn fast_cfg() -> ZkConfig {
    let mut cfg = ZkConfig::default();
    cfg.microtx_sla_ms  = 30_000;
    cfg.normal_sla_ms   = 30_000;
    cfg.worker_count    = 2;
    cfg.checkpoint_dir  = std::env::temp_dir()
        .join(format!("omega_zk_test_{}", rand::random::<u32>()))
        .to_str().unwrap().to_string();
    cfg
}

fn small_queue_cfg(throttle: usize, suspend: usize, halt: usize) -> ZkConfig {
    let mut cfg = ZkConfig::default();
    cfg.proof_queue_throttle = throttle;
    cfg.proof_queue_suspend  = suspend;
    cfg.proof_queue_halt     = halt;
    cfg
}

// ─── 1. Config constants ──────────────────────────────────────────────────────

#[test]
fn config_constants_match_spec() {
    assert_eq!(MICROTX_SLA_MS,       1200,  "microtx SLA must be 1200ms");
    assert_eq!(NORMAL_SLA_MS,        4000,  "normal SLA must be 4000ms");
    assert_eq!(QUEUE_THROTTLE_DEPTH, 128,   "throttle at 128");
    assert_eq!(QUEUE_SUSPEND_DEPTH,  256,   "suspend at 256");
    assert_eq!(QUEUE_HALT_DEPTH,     512,   "halt at 512");
}

// ─── 2. Commitment roundtrip ──────────────────────────────────────────────────

#[test]
fn commitment_verify_roundtrip() {
    let c = compute_proof_commitment(&hash(0x10), 500_000, 42161, "LA");
    assert!(verify_commitment(&c, &hash(0x10), 500_000, 42161, "LA"));
    assert!(!verify_commitment(&c, &hash(0x10), 500_001, 42161, "LA"));
    assert!(!verify_commitment(&c, &hash(0x11), 500_000, 42161, "LA"));
    assert!(!verify_commitment(&c, &hash(0x10), 500_000, 1,     "LA"));
    assert!(!verify_commitment(&c, &hash(0x10), 500_000, 42161, "SA"));
}

#[test]
fn commitment_equality_by_value() {
    let a = compute_proof_commitment(&hash(0x20), 100, 42161, "SA");
    let b = compute_proof_commitment(&hash(0x20), 100, 42161, "SA");
    assert_eq!(a, b);
}

// ─── 3. Queue pressure FSM transitions ───────────────────────────────────────

#[test]
fn queue_pressure_all_thresholds() {
    assert_eq!(QueuePressure::from_depth(0),   QueuePressure::Normal);
    assert_eq!(QueuePressure::from_depth(127),  QueuePressure::Normal);
    assert_eq!(QueuePressure::from_depth(128),  QueuePressure::Throttle);
    assert_eq!(QueuePressure::from_depth(255),  QueuePressure::Throttle);
    assert_eq!(QueuePressure::from_depth(256),  QueuePressure::Suspend);
    assert_eq!(QueuePressure::from_depth(511),  QueuePressure::Suspend);
    assert_eq!(QueuePressure::from_depth(512),  QueuePressure::Halt);
    assert_eq!(QueuePressure::from_depth(9999), QueuePressure::Halt);
}

// ─── 4. Queue: suspend accepts microtx, rejects normal ───────────────────────

#[test]
fn queue_suspend_rejects_normal_accepts_microtx() {
    let cfg = small_queue_cfg(1, 1, 100);
    let q   = ProofQueue::new(cfg);

    // Fill to suspend threshold.
    q.submit(hash(0x01), 100, 42161, "LA".into(), true).unwrap();
    assert_eq!(q.pressure(), QueuePressure::Suspend);

    // Normal lane rejected.
    assert!(matches!(
        q.submit(hash(0x02), 100, 42161, "LA".into(), false),
        Err(ZkError::QueueSuspended { .. })
    ));

    // Microtx lane accepted.
    assert!(q.submit(hash(0x03), 100, 42161, "LA".into(), true).is_ok());
}

// ─── 5. Queue: halt rejects everything ───────────────────────────────────────

#[test]
fn queue_halt_rejects_all() {
    let cfg = small_queue_cfg(1, 1, 1);
    let q   = ProofQueue::new(cfg);

    q.submit(hash(0xf0), 100, 42161, "SA".into(), true).unwrap();
    assert_eq!(q.pressure(), QueuePressure::Halt);

    assert!(matches!(
        q.submit(hash(0xf1), 100, 42161, "SA".into(), true),
        Err(ZkError::QueueFull { .. })
    ));
    assert!(matches!(
        q.submit(hash(0xf2), 100, 42161, "SA".into(), false),
        Err(ZkError::QueueFull { .. })
    ));
}

// ─── 6. Concurrent queue submissions ─────────────────────────────────────────

#[tokio::test]
async fn queue_concurrent_submit_from_multiple_tasks() {
    let cfg = ZkConfig::default();
    let q   = ProofQueue::new(cfg);

    let mut handles = Vec::new();
    for i in 0u8..16 {
        let q2 = q.clone();
        handles.push(tokio::spawn(async move {
            q2.submit([i; 32], i as u128 * 1000, 42161, "MSA".into(), i % 2 == 0)
        }));
    }

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    let successes = results.iter().filter(|r| r.is_ok()).count();
    assert!(successes > 0, "at least some submissions should succeed");
    assert_eq!(q.depth(), successes, "depth should equal successful submissions");
}

// ─── 7. Prover produces valid non-empty proof ─────────────────────────────────

#[test]
fn prover_generates_non_empty_proof() {
    let p     = T1SoftwareProver::new(42161);
    let proof = p.prove(hash(0x30), 1_000_000, "SA").unwrap();
    assert!(!proof.proof_bytes.is_empty());
    assert_eq!(proof.chain_id, 42161);
    assert_eq!(proof.strategy_id, "SA");
    assert_eq!(proof.blueprint_hash, hash(0x30));
    assert_eq!(proof.net_profit_wei, 1_000_000);
}

#[test]
fn prover_different_inputs_different_proofs() {
    let p  = T1SoftwareProver::new(42161);
    let p1 = p.prove(hash(0x41), 100, "LA").unwrap();
    let p2 = p.prove(hash(0x42), 100, "LA").unwrap();
    assert_ne!(p1.proof_bytes, p2.proof_bytes);
}

// ─── 8. Worker pool end-to-end ────────────────────────────────────────────────

#[tokio::test]
async fn worker_pool_proves_single_request() {
    let cfg   = fast_cfg();
    let queue = ProofQueue::new(cfg.clone());
    let pool  = ProofWorkerPool::start(cfg, queue.clone());

    let rx = queue.submit(
        hash(0x50), 2_000_000, 42161, "LA".into(), false,
    ).unwrap();

    let result = timeout(Duration::from_secs(90), rx)
        .await
        .expect("timed out")
        .expect("oneshot closed");

    assert!(result.is_ok(), "proof should succeed: {:?}", result.err());
    let proof = result.unwrap();
    assert_eq!(proof.blueprint_hash, hash(0x50));
    assert!(!proof.proof_bytes.is_empty());

    pool.shutdown();
}

#[tokio::test]
async fn worker_pool_depth_returns_to_zero() {
    let cfg   = fast_cfg();
    let queue = ProofQueue::new(cfg.clone());
    let pool  = ProofWorkerPool::start(cfg, queue.clone());

    let mut rxs = Vec::new();
    for i in 0u8..4 {
        rxs.push(
            queue.submit([i; 32], i as u128 * 100, 42161, "CNRY".into(), true)
                .unwrap()
        );
    }

    for rx in rxs {
        let _ = timeout(Duration::from_secs(120), rx)
            .await
            .expect("timed out")
            .expect("oneshot closed");
    }

    assert_eq!(queue.depth(), 0, "queue depth must be 0 after all proofs complete");
    pool.shutdown();
}

// ─── 9. Checkpoint record → restart → recover ────────────────────────────────

#[test]
fn checkpoint_record_and_recover_full_cycle() {
    let mut cfg = ZkConfig::default();
    cfg.checkpoint_dir = std::env::temp_dir()
        .join(format!("omega_ckpt_cycle_{}", rand::random::<u32>()))
        .to_str().unwrap().to_string();
    cfg.max_checkpoints = 100;

    let mgr = ProofCheckpointManager::new(&cfg);
    mgr.record(101, hash(0xa0), 777_000, 42161, "MSA".into(), false);
    mgr.record(102, hash(0xa1), 888_000, 42161, "LA".into(),  true);

    // Simulate restart — drop and recreate.
    drop(mgr);
    let mgr2     = ProofCheckpointManager::new(&cfg);
    let recovered = mgr2.recover().unwrap();

    assert_eq!(recovered.len(), 2);
    let ids: Vec<u64> = recovered.iter().map(|e| e.request_id).collect();
    assert!(ids.contains(&101));
    assert!(ids.contains(&102));

    // Completing removes from recovery.
    mgr2.complete(101);
    mgr2.complete(102);

    drop(mgr2);
    let mgr3      = ProofCheckpointManager::new(&cfg);
    let after_complete = mgr3.recover().unwrap();
    assert!(after_complete.is_empty(), "completed proofs must not be recovered");
}

// ─── 10. QueuePressure helpers ────────────────────────────────────────────────

#[test]
fn queue_pressure_halt_worthy_and_degraded() {
    assert!(QueuePressure::Halt.is_halt_worthy());
    assert!(!QueuePressure::Suspend.is_halt_worthy());
    assert!(!QueuePressure::Throttle.is_halt_worthy());
    assert!(!QueuePressure::Normal.is_halt_worthy());

    assert!(QueuePressure::Suspend.is_degraded_worthy());
    assert!(!QueuePressure::Halt.is_degraded_worthy());
    assert!(!QueuePressure::Throttle.is_degraded_worthy());
    assert!(!QueuePressure::Normal.is_degraded_worthy());
}

// ─── 11. ZkError categorisation ──────────────────────────────────────────────

#[test]
fn zk_error_halt_worthy_classification() {
    assert!(ZkError::QueueFull { depth: 512, halt_threshold: 512 }.is_halt_worthy());
    assert!(ZkError::PoolShutdown.is_halt_worthy());
    assert!(!ZkError::ProofTimeout { elapsed_ms: 1200, sla_ms: 1200 }.is_halt_worthy());
    assert!(!ZkError::QueueSuspended { depth: 256 }.is_halt_worthy());
}

#[test]
fn zk_error_degraded_worthy_classification() {
    assert!(ZkError::QueueSuspended { depth: 256 }.is_degraded_worthy());
    assert!(ZkError::ProofTimeout { elapsed_ms: 5000, sla_ms: 4000 }.is_degraded_worthy());
    assert!(!ZkError::QueueFull { depth: 512, halt_threshold: 512 }.is_degraded_worthy());
    assert!(!ZkError::PoolShutdown.is_degraded_worthy());
}

// ─── 12. Metrics smoke test ───────────────────────────────────────────────────

#[test]
fn metrics_register_without_panic() {
    metrics::register_all();
    // If we reach here, all metrics registered successfully.
}

// ─── 13. ZkConfig::sla_ms dispatch ───────────────────────────────────────────

#[test]
fn config_sla_ms_correct() {
    let cfg = ZkConfig::default();
    assert_eq!(cfg.sla_ms(true),  MICROTX_SLA_MS);
    assert_eq!(cfg.sla_ms(false), NORMAL_SLA_MS);
}

// ─── 14. ProofCommitment display and equality ─────────────────────────────────

#[test]
fn proof_commitment_display_hex_prefixed() {
    let c = compute_proof_commitment(&hash(0x00), 0, 42161, "CNRY");
    let s = c.to_string();
    assert!(s.starts_with("0x"), "commitment display must be 0x-prefixed");
    assert_eq!(s.len(), 66, "0x + 64 hex chars");
}

#[test]
fn proof_commitment_as_bytes_roundtrip() {
    let c    = compute_proof_commitment(&hash(0xbb), 12345, 42161, "MEV");
    let b    = *c.as_bytes();
    let hex  = hex::encode(b);
    assert_eq!(hex.len(), 64);
}

// ─── 15. Queue depth / complete symmetry ─────────────────────────────────────

#[test]
fn queue_depth_symmetry_submit_and_complete() {
    let q = ProofQueue::new(ZkConfig::default());

    // Submit 5 items.
    for i in 0u8..5 {
        q.submit([i; 32], 100, 42161, "SA".into(), false).unwrap();
    }
    assert_eq!(q.depth(), 5);

    // Complete 5 items.
    for _ in 0..5 { q.complete(); }
    assert_eq!(q.depth(), 0);

    // Extra complete never underflows.
    q.complete();
    assert_eq!(q.depth(), 0);
}

use futures::future::join_all;