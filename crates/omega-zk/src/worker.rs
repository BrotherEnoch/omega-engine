// crates/omega-zk/src/worker.rs
//
// Async proof worker pool (spec: "ZK proof async worker pool (T1 software baseline)").
//
// Design:
//   • N independent tokio tasks (one per configured worker_count, default = CPU count ≤ 8).
//   • Each worker loops: recv from crossbeam channel → spawn_blocking(prove) → send response.
//   • `spawn_blocking` offloads the CPU-bound Winterfell STARK prover to a dedicated
//     blocking thread pool (tokio's rayon-equivalent), keeping the async runtime free.
//   • SLA enforcement: tokio::time::timeout wraps each spawn_blocking call.
//     - Microtx: 1200 ms.
//     - Normal:  4000 ms.
//   • On timeout: the ProofRequest receives `Err(ZkError::ProofTimeout)`; the worker
//     continues to the next request without panicking.
//   • On prover panic: the JoinHandle error is caught; `WORKER_PANICS` metric incremented;
//     the worker restarts its loop (never crashes the pool).
//   • Worker pool shutdown: `ProofWorkerPool::shutdown()` closes the queue sender,
//     causing all `recv()` calls to return `Err(Disconnected)`, terminating workers cleanly.
//
// Parallelism:
//   Multiple workers drain the queue concurrently — max TPS = worker_count × 1/proof_time.
//   For T1Software (~500ms per proof), 8 workers → ~16 proofs/second throughput.
//
// Queue interaction:
//   After completing a proof (success or failure), the worker calls `queue.complete()`
//   to decrement the depth counter and update the pressure FSM.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::config::ZkConfig;
use crate::error::ZkError;
use crate::metrics;
use crate::prover::T1SoftwareProver;
use crate::queue::{ProofQueue, ProofRequest};

/// Manages the pool of async proof worker tasks.
pub struct ProofWorkerPool {
    handles: Vec<JoinHandle<()>>,
    queue: ProofQueue,
    worker_count: usize,
}

impl ProofWorkerPool {
    /// Spawn `cfg.worker_count` worker tasks against `queue`.
    pub fn start(cfg: ZkConfig, queue: ProofQueue) -> Self {
        metrics::register_all();

        let worker_count = cfg.worker_count;
        let chain_id = 42161u64; // passed through cfg in production; hardcoded for now

        tracing::info!(worker_count, "proof worker pool starting");

        let mut handles = Vec::with_capacity(worker_count);

        for worker_id in 0..worker_count {
            let q = queue.clone();
            let cfg = cfg.clone();

            let handle = tokio::spawn(async move {
                run_worker(worker_id, q, cfg, chain_id).await;
            });

            handles.push(handle);
        }

        metrics::WORKER_COUNT
            .with_label_values(&["idle"])
            .set(worker_count as f64);

        Self {
            handles,
            queue,
            worker_count,
        }
    }

    /// Graceful shutdown: abort all worker tasks.
    ///
    /// In production shutdown happens naturally when the crossbeam Sender
    /// is dropped (workers receive Disconnected); abort() here is belt-and-braces.
    pub fn shutdown(self) {
        tracing::info!(
            worker_count = self.worker_count,
            "proof worker pool shutting down"
        );
        for h in self.handles {
            h.abort();
        }
    }

    /// Current queue depth (delegated to queue).
    pub fn queue_depth(&self) -> usize {
        self.queue.depth()
    }

    /// Current queue pressure.
    pub fn queue_pressure(&self) -> crate::queue::QueuePressure {
        self.queue.pressure()
    }
}

// ─── Worker loop ──────────────────────────────────────────────────────────────

async fn run_worker(worker_id: usize, queue: ProofQueue, cfg: ZkConfig, chain_id: u64) {
    let prover = Arc::new(T1SoftwareProver::new(chain_id));

    tracing::debug!(worker_id, "proof worker started");

    loop {
        // Block synchronously on the crossbeam receiver.
        // We call this inside spawn_blocking so the async runtime is not stalled.
        let req = {
            let receiver = queue.receiver.clone();
            match tokio::task::spawn_blocking(move || receiver.recv()).await {
                Ok(Ok(req)) => req,
                Ok(Err(_)) => {
                    // Channel disconnected — shutdown.
                    tracing::info!(worker_id, "proof worker channel closed, exiting");
                    break;
                }
                Err(join_err) => {
                    // spawn_blocking panicked — should never happen for a simple recv.
                    tracing::error!(worker_id, %join_err, "proof worker recv panicked");
                    metrics::WORKER_PANICS.inc();
                    continue;
                }
            }
        };

        process_request(worker_id, req, &prover, &queue, &cfg).await;
    }

    tracing::debug!(worker_id, "proof worker exited");
}

async fn process_request(
    worker_id: usize,
    req: ProofRequest,
    prover: &Arc<T1SoftwareProver>,
    queue: &ProofQueue,
    cfg: &ZkConfig,
) {
    let request_id = req.id;
    let is_microtx = req.is_microtx;
    let strategy_id = req.strategy_id.clone();
    let blueprint_hash = req.blueprint_hash;
    let net_profit_wei = req.net_profit_wei;
    let sla_ms = cfg.sla_ms(is_microtx);
    let lane = if is_microtx { "microtx" } else { "normal" };

    metrics::WORKER_COUNT.with_label_values(&["proving"]).inc();
    metrics::WORKER_COUNT.with_label_values(&["idle"]).dec();

    tracing::debug!(worker_id, request_id, strategy = %strategy_id, is_microtx,
        "starting proof generation");

    let prover2 = Arc::clone(prover);
    let strategy_id2 = strategy_id.clone();
    let prove_future = tokio::task::spawn_blocking(move || {
        prover2.prove(blueprint_hash, net_profit_wei, &strategy_id2)
    });

    let result = tokio::time::timeout(Duration::from_millis(sla_ms), prove_future).await;

    let proof_result = match result {
        // Within SLA, no panic.
        Ok(Ok(Ok(proof))) => {
            let gen_ms = proof.generation_ms;
            metrics::PROOF_LATENCY_MS
                .with_label_values(&[lane, &strategy_id])
                .observe(gen_ms as f64);
            metrics::PROOFS_GENERATED
                .with_label_values(&[lane, &strategy_id, prover2_tier(&blueprint_hash)])
                .inc();

            if gen_ms > sla_ms {
                metrics::PROOF_SLA_VIOLATIONS
                    .with_label_values(&[lane])
                    .inc();
                tracing::warn!(
                    worker_id,
                    request_id,
                    gen_ms,
                    sla_ms,
                    "proof completed but exceeded SLA"
                );
            }
            Ok(proof)
        }

        // Prover returned an error.
        Ok(Ok(Err(e))) => {
            metrics::PROOF_FAILURES
                .with_label_values(&["prover_error"])
                .inc();
            tracing::error!(worker_id, request_id, error = %e, "proof generation failed");
            Err(e)
        }

        // spawn_blocking panicked.
        Ok(Err(join_err)) => {
            metrics::WORKER_PANICS.inc();
            metrics::PROOF_FAILURES
                .with_label_values(&["prover_error"])
                .inc();
            tracing::error!(worker_id, request_id, %join_err, "proof worker panicked");
            Err(ZkError::WorkerPanic {
                worker_id,
                request_id,
            })
        }

        // Timeout.
        Err(_elapsed) => {
            metrics::PROOF_FAILURES
                .with_label_values(&["timeout"])
                .inc();
            metrics::PROOF_SLA_VIOLATIONS
                .with_label_values(&[lane])
                .inc();
            tracing::warn!(worker_id, request_id, sla_ms, "proof timed out");
            Err(ZkError::ProofTimeout {
                elapsed_ms: sla_ms,
                sla_ms,
            })
        }
    };

    // Decrement queue depth regardless of outcome.
    queue.complete();

    metrics::WORKER_COUNT.with_label_values(&["proving"]).dec();
    metrics::WORKER_COUNT.with_label_values(&["idle"]).inc();

    // Send response back to the requesting strategy task.
    // If the receiver was dropped (blueprint expired), this is a no-op.
    let _ = req.response_tx.send(proof_result);
}

fn prover2_tier(_: &[u8; 32]) -> &'static str {
    "t1_software"
}

#[cfg(test)]
mod worker_tests {
    use super::*;
    use crate::config::ZkConfig;
    use crate::queue::ProofQueue;

    #[tokio::test]
    async fn worker_pool_generates_proof_end_to_end() {
        let cfg = ZkConfig {
            worker_count: 2,
            microtx_sla_ms: 30_000,
            normal_sla_ms: 30_000,
            ..Default::default()
        };

        let queue = ProofQueue::new(cfg.clone());
        let pool = ProofWorkerPool::start(cfg, queue.clone());

        let rx = queue
            .submit([0xde; 32], 500_000_000, 42161, "LA".into(), false)
            .unwrap();

        let result = tokio::time::timeout(Duration::from_secs(60), rx)
            .await
            .expect("timed out waiting for proof")
            .expect("oneshot closed");

        assert!(result.is_ok(), "proof should succeed: {:?}", result.err());
        let proof = result.unwrap();
        assert_eq!(proof.blueprint_hash, [0xde; 32]);
        assert!(!proof.proof_bytes.is_empty());

        pool.shutdown();
    }

    #[tokio::test]
    async fn worker_pool_drains_multiple_requests() {
        let cfg = ZkConfig {
            worker_count: 4,
            normal_sla_ms: 30_000,
            microtx_sla_ms: 30_000,
            ..Default::default()
        };

        let queue = ProofQueue::new(cfg.clone());
        let pool = ProofWorkerPool::start(cfg, queue.clone());

        let mut rxs = Vec::new();
        for i in 0u8..4 {
            let rx = queue
                .submit([i; 32], 100 + i as u128, 42161, "SA".into(), false)
                .unwrap();
            rxs.push(rx);
        }

        for rx in rxs {
            let result = tokio::time::timeout(Duration::from_secs(120), rx)
                .await
                .expect("timed out")
                .expect("oneshot closed");
            assert!(result.is_ok());
        }

        assert_eq!(
            queue.depth(),
            0,
            "queue should be empty after all proofs complete"
        );
        pool.shutdown();
    }
}
