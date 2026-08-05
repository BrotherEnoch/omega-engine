// crates/omega-zk/src/checkpoint.rs
//
// Proof checkpoint manager (spec: "proof queue auto-throttle + checkpoint manager").
//
// Purpose:
//   When the proof worker pool is shut down mid-operation (HALT, restart, upgrade),
//   in-flight proof requests are lost.  The checkpoint manager persists the minimal
//   state needed to re-issue those requests after restart, preventing blueprint
//   execution gaps.
//
// What is checkpointed:
//   A `ProofCheckpointEntry` for each in-flight request containing:
//     - blueprint_hash, net_profit_wei, chain_id, strategy_id, is_microtx.
//   This is sufficient to re-queue the proof on startup.  The actual proof bytes
//   are NOT checkpointed (they are regenerated).
//
// Lifecycle:
//   1. `record(req)` — called when a ProofRequest is dequeued by a worker and
//      proof generation begins.
//   2. `complete(id)` — called when proof generation finishes (success or failure).
//      Removes the checkpoint entry.
//   3. `recover()` — called on startup; returns all incomplete entries so the
//      caller can re-submit them to the queue.
//
// Storage:
//   One JSON file per checkpoint entry: `{checkpoint_dir}/{request_id}.json`.
//   On `complete()` the file is deleted.
//   On `recover()` all files in `checkpoint_dir` are read and returned.
//
// Thread safety:
//   DashMap<u64, ProofCheckpointEntry> tracks in-memory state.
//   Disk writes are synchronous (no async IO contention on the hot path — only
//   one write per proof start, one delete per proof end).
//   In production with many concurrent workers this is acceptable because the
//   write rate equals the proof throughput rate (~16 proofs/sec max for T1Software).
//
// Spec: keep last `max_checkpoints` entries on disk; prune older ones.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::ZkConfig;
use crate::error::ZkError;
use crate::metrics;

// ─── Checkpoint entry ─────────────────────────────────────────────────────────

/// Serialisable state needed to re-issue a proof request after restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofCheckpointEntry {
    pub request_id: u64,
    pub blueprint_hash: [u8; 32],
    pub net_profit_wei: u128,
    pub chain_id: u64,
    pub strategy_id: String,
    pub is_microtx: bool,
    /// ISO-8601 timestamp when this checkpoint was written.
    pub recorded_at: String,
}

// ─── Manager ─────────────────────────────────────────────────────────────────

/// Proof checkpoint manager.
///
/// `Arc<ProofCheckpointManager>` shared between worker tasks.
pub struct ProofCheckpointManager {
    in_flight: DashMap<u64, ProofCheckpointEntry>,
    checkpoint_dir: PathBuf,
    max_checkpoints: usize,
}

impl ProofCheckpointManager {
    /// Create and ensure the checkpoint directory exists.
    pub fn new(cfg: &ZkConfig) -> Arc<Self> {
        let dir = PathBuf::from(&cfg.checkpoint_dir);
        if !dir.exists() {
            if let Err(e) = fs::create_dir_all(&dir) {
                tracing::warn!(
                    dir = %dir.display(),
                    error = %e,
                    "failed to create checkpoint dir — checkpoints disabled"
                );
            }
        }
        Arc::new(Self {
            in_flight: DashMap::new(),
            checkpoint_dir: dir,
            max_checkpoints: cfg.max_checkpoints,
        })
    }

    // ── Record ────────────────────────────────────────────────────────────────

    /// Record that a proof request is now in-flight.
    ///
    /// Writes a JSON checkpoint file and inserts into the in-memory map.
    /// Called by the worker task when proof generation begins.
    pub fn record(
        &self,
        request_id: u64,
        blueprint_hash: [u8; 32],
        net_profit_wei: u128,
        chain_id: u64,
        strategy_id: String,
        is_microtx: bool,
    ) {
        let entry = ProofCheckpointEntry {
            request_id,
            blueprint_hash,
            net_profit_wei,
            chain_id,
            strategy_id,
            is_microtx,
            recorded_at: chrono::Utc::now().to_rfc3339(),
        };

        // Write to disk first (durability before in-memory).
        if let Err(e) = self.write_to_disk(&entry) {
            tracing::warn!(
                request_id,
                error = %e,
                "checkpoint write failed — proof may be lost on restart"
            );
        } else {
            metrics::CHECKPOINTS_WRITTEN.inc();
        }

        self.in_flight.insert(request_id, entry);

        // Prune if over limit.
        self.prune_if_needed();
    }

    // ── Complete ──────────────────────────────────────────────────────────────

    /// Remove the checkpoint for a completed (or failed) proof request.
    ///
    /// Deletes the checkpoint file and removes from the in-memory map.
    /// Idempotent: safe to call even if the entry does not exist.
    pub fn complete(&self, request_id: u64) {
        self.in_flight.remove(&request_id);
        let path = self.entry_path(request_id);
        if path.exists() {
            if let Err(e) = fs::remove_file(&path) {
                tracing::warn!(
                    request_id,
                    error = %e,
                    "failed to delete checkpoint file"
                );
            }
        }
    }

    // ── Recover ───────────────────────────────────────────────────────────────

    /// Read all incomplete checkpoint entries from disk.
    ///
    /// Called on startup to re-queue in-flight proofs that were interrupted
    /// by a HALT, restart, or crash.
    ///
    /// Returns a `Vec<ProofCheckpointEntry>` — caller submits each to the queue.
    pub fn recover(&self) -> Result<Vec<ProofCheckpointEntry>, ZkError> {
        if !self.checkpoint_dir.exists() {
            return Ok(Vec::new());
        }

        let entries =
            fs::read_dir(&self.checkpoint_dir).map_err(|e| ZkError::CheckpointReadFailed {
                detail: e.to_string(),
            })?;

        let mut recovered = Vec::new();

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            match fs::read_to_string(&path) {
                Ok(contents) => match serde_json::from_str::<ProofCheckpointEntry>(&contents) {
                    Ok(cp) => {
                        tracing::info!(
                            request_id = cp.request_id,
                            strategy   = %cp.strategy_id,
                            "recovered proof checkpoint"
                        );
                        metrics::CHECKPOINTS_RECOVERED.inc();
                        recovered.push(cp);
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "malformed checkpoint file — skipping"
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "failed to read checkpoint file — skipping"
                    );
                }
            }
        }

        tracing::info!(count = recovered.len(), "checkpoint recovery complete");
        Ok(recovered)
    }

    // ── Observability ─────────────────────────────────────────────────────────

    /// Number of currently in-flight proofs (in-memory count).
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    /// Return all in-flight request IDs.
    pub fn in_flight_ids(&self) -> Vec<u64> {
        self.in_flight.iter().map(|e| *e.key()).collect()
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn entry_path(&self, request_id: u64) -> PathBuf {
        self.checkpoint_dir.join(format!("{}.json", request_id))
    }

    fn write_to_disk(&self, entry: &ProofCheckpointEntry) -> Result<(), ZkError> {
        let path = self.entry_path(entry.request_id);
        let contents =
            serde_json::to_string_pretty(entry).map_err(|e| ZkError::CheckpointWriteFailed {
                request_id: entry.request_id,
                detail: e.to_string(),
            })?;
        fs::write(&path, contents).map_err(|e| ZkError::CheckpointWriteFailed {
            request_id: entry.request_id,
            detail: e.to_string(),
        })?;
        Ok(())
    }

    fn prune_if_needed(&self) {
        if self.in_flight.len() <= self.max_checkpoints {
            return;
        }
        // Collect entries sorted by request_id ascending; remove oldest.
        let mut ids: Vec<u64> = self.in_flight.iter().map(|e| *e.key()).collect();
        ids.sort_unstable();
        let to_remove = ids.len().saturating_sub(self.max_checkpoints);
        for id in ids.into_iter().take(to_remove) {
            self.complete(id);
            tracing::debug!(request_id = id, "pruned old checkpoint");
        }
    }
}

#[cfg(test)]
mod checkpoint_tests {
    use super::*;
    use crate::config::ZkConfig;
    use std::env;

    fn temp_cfg() -> ZkConfig {
        ZkConfig {
            checkpoint_dir: env::temp_dir()
                .join(format!("omega_zk_ckpt_{}", rand::random::<u32>()))
                .to_str()
                .unwrap()
                .to_string(),
            max_checkpoints: 10,
            ..Default::default()
        }
    }

    fn make_manager() -> Arc<ProofCheckpointManager> {
        ProofCheckpointManager::new(&temp_cfg())
    }

    fn sample_entry(id: u64) -> (u64, [u8; 32], u128, u64, String, bool) {
        (id, [id as u8; 32], 1_000_000, 42161, "LA".into(), false)
    }

    #[test]
    fn record_creates_in_memory_entry() {
        let m = make_manager();
        let (id, hash, profit, chain, strat, microtx) = sample_entry(1);
        m.record(id, hash, profit, chain, strat, microtx);
        assert_eq!(m.in_flight_count(), 1);
        assert!(m.in_flight_ids().contains(&1));
    }

    #[test]
    fn complete_removes_entry() {
        let m = make_manager();
        let (id, hash, profit, chain, strat, microtx) = sample_entry(2);
        m.record(id, hash, profit, chain, strat, microtx);
        assert_eq!(m.in_flight_count(), 1);
        m.complete(2);
        assert_eq!(m.in_flight_count(), 0);
    }

    #[test]
    fn complete_is_idempotent() {
        let m = make_manager();
        m.complete(99); // never recorded
        assert_eq!(m.in_flight_count(), 0);
    }

    #[test]
    fn recover_returns_written_entries() {
        let cfg = temp_cfg();
        let m = ProofCheckpointManager::new(&cfg);

        for i in 1u64..=3 {
            let (id, hash, profit, chain, strat, microtx) = sample_entry(i);
            m.record(id, hash, profit, chain, strat, microtx);
        }

        // Drop and recreate — simulates restart.
        drop(m);
        let m2 = ProofCheckpointManager::new(&cfg);
        let recovered = m2.recover().unwrap();
        assert_eq!(recovered.len(), 3);

        let ids: Vec<u64> = recovered.iter().map(|e| e.request_id).collect();
        for i in 1u64..=3 {
            assert!(ids.contains(&i), "missing checkpoint for request {}", i);
        }
    }

    #[test]
    fn complete_before_recover_removes_file() {
        let cfg = temp_cfg();
        let m = ProofCheckpointManager::new(&cfg);

        let (id, hash, profit, chain, strat, microtx) = sample_entry(10);
        m.record(id, hash, profit, chain, strat, microtx);
        m.complete(10);

        drop(m);
        let m2 = ProofCheckpointManager::new(&cfg);
        let recovered = m2.recover().unwrap();
        assert!(
            recovered.is_empty(),
            "completed proof should not be recovered"
        );
    }

    #[test]
    fn recover_empty_dir_returns_empty_vec() {
        let m = make_manager();
        let recovered = m.recover().unwrap();
        assert!(recovered.is_empty());
    }

    #[test]
    fn prune_keeps_max_checkpoints() {
        let cfg = ZkConfig {
            max_checkpoints: 3,
            ..temp_cfg()
        };
        let m = ProofCheckpointManager::new(&cfg);

        for i in 1u64..=5 {
            let (id, hash, profit, chain, strat, microtx) = sample_entry(i);
            m.record(id, hash, profit, chain, strat, microtx);
        }

        // After 5 records with max=3, in-memory count should be ≤ 3.
        assert!(
            m.in_flight_count() <= 3,
            "expected ≤3 in-flight, got {}",
            m.in_flight_count()
        );
    }

    #[test]
    fn checkpoint_entry_fields_preserved() {
        let cfg = temp_cfg();
        let m = ProofCheckpointManager::new(&cfg);
        m.record(77, [0xab; 32], 9_999_999, 42161, "MEV".into(), true);

        drop(m);
        let m2 = ProofCheckpointManager::new(&cfg);
        let recovered = m2.recover().unwrap();
        assert_eq!(recovered.len(), 1);
        let e = &recovered[0];
        assert_eq!(e.request_id, 77);
        assert_eq!(e.blueprint_hash, [0xab; 32]);
        assert_eq!(e.net_profit_wei, 9_999_999);
        assert_eq!(e.chain_id, 42161);
        assert_eq!(e.strategy_id, "MEV");
        assert!(e.is_microtx);
    }
}
