// crates/omega-execution/src/inflight.rs
//
// Durable in-flight blueprint journal for crash recovery (priority 7).
//
// ## Problem
//
// If the process dies after Stage 5 submission but before Stage 7
// confirmation, the engine has no local record of what was submitted.
// On restart it cannot:
//   - re-attach pending bundles to InclusionTracker
//   - avoid double-submitting the same idempotency key after a cold start
//     (process-local IdempotencyCache is empty)
//   - feed kill-switch outcomes for already-submitted capital
//
// ## Design
//
// Append-only JSONL journal on local disk. Each line is one
// `InFlightRecord`. On startup, `recover()` reads the journal, returns
// still-open records, and the caller re-tracks them with
// InclusionTracker / re-seeds idempotency keys.
//
// Records are marked terminal (`included` / `missed` / `abandoned`) when
// Stage 7 resolves them. Terminal records are retained for audit but
// skipped on recovery.
//
// ## Failure mode
//
// Disk full / IO error → fail closed on `record_submitted` (do not submit
// further blueprints without a durable trail). Recovery IO errors are
// logged and return empty (safe: may re-submit after restart only if
// idempotency/nonce layers also lost state — operator must investigate).

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

/// Lifecycle state of a submitted blueprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InFlightStatus {
    /// Submitted to at least one relay; awaiting Stage 7.
    Submitted,
    /// Stage 7 confirmed on-chain inclusion.
    Included,
    /// Stage 7 resolved as not included (grace expired or failed receipt).
    Missed,
    /// Operator or system abandoned (e.g. journal compact after max age).
    Abandoned,
}

/// One durable journal line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InFlightRecord {
    pub recorded_at: DateTime<Utc>,
    pub status: InFlightStatus,
    pub strategy_id: String,
    pub nonce: u64,
    pub bundle_hash: String,
    pub blueprint_hash: String,
    pub chain_id: u64,
    pub target_block: u64,
    pub expected_profit_net_wei: u128,
    pub idempotency_key_hex: String,
}

/// Thread-safe append-only journal.
pub struct InFlightJournal {
    path: PathBuf,
    lock: Mutex<()>,
}

impl InFlightJournal {
    /// Open (or create) the journal at `path`.
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Ensure file exists.
        let _ = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            lock: Mutex::new(()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a Submitted record. Fails closed on IO error.
    pub fn record_submitted(&self, rec: InFlightRecord) -> std::io::Result<()> {
        debug_assert_eq!(rec.status, InFlightStatus::Submitted);
        self.append(&rec)
    }

    /// Append a terminal status update for an existing blueprint/bundle.
    #[allow(clippy::too_many_arguments)]
    pub fn record_terminal(
        &self,
        blueprint_hash: &str,
        bundle_hash: &str,
        status: InFlightStatus,
        strategy_id: &str,
        nonce: u64,
        chain_id: u64,
        expected_profit_net_wei: u128,
        target_block: u64,
        idempotency_key_hex: &str,
    ) -> std::io::Result<()> {
        assert!(matches!(
            status,
            InFlightStatus::Included | InFlightStatus::Missed | InFlightStatus::Abandoned
        ));
        self.append(&InFlightRecord {
            recorded_at: Utc::now(),
            status,
            strategy_id: strategy_id.to_string(),
            nonce,
            bundle_hash: bundle_hash.to_string(),
            blueprint_hash: blueprint_hash.to_string(),
            chain_id,
            target_block,
            expected_profit_net_wei,
            idempotency_key_hex: idempotency_key_hex.to_string(),
        })
    }

    fn append(&self, rec: &InFlightRecord) -> std::io::Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        let line = serde_json::to_string(rec).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.flush()?;
        Ok(())
    }

    /// Reconstruct open (Submitted) records after crash.
    ///
    /// Last-write-wins per `blueprint_hash`: a later Included/Missed line
    /// clears an earlier Submitted for the same hash.
    pub fn recover_open(&self) -> std::io::Result<Vec<InFlightRecord>> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let f = File::open(&self.path)?;
        let reader = BufReader::new(f);
        // blueprint_hash -> latest record
        let mut latest: std::collections::HashMap<String, InFlightRecord> =
            std::collections::HashMap::new();
        for (lineno, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<InFlightRecord>(&line) {
                Ok(rec) => {
                    latest.insert(rec.blueprint_hash.clone(), rec);
                }
                Err(e) => {
                    warn!(
                        path = %self.path.display(),
                        line = lineno + 1,
                        error = %e,
                        "InFlightJournal: skipping corrupt journal line"
                    );
                }
            }
        }
        let open: Vec<_> = latest
            .into_values()
            .filter(|r| r.status == InFlightStatus::Submitted)
            .collect();
        info!(
            path = %self.path.display(),
            open_count = open.len(),
            "InFlightJournal: recovery scan complete"
        );
        Ok(open)
    }

    /// Number of lines in the journal (observability).
    pub fn line_count(&self) -> std::io::Result<usize> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if !self.path.exists() {
            return Ok(0);
        }
        let f = File::open(&self.path)?;
        Ok(BufReader::new(f).lines().count())
    }
}

/// Best-effort path from env `OMEGA_INFLIGHT_JOURNAL` or default under
/// `./data/inflight_journal.jsonl`.
pub fn default_journal_path() -> PathBuf {
    std::env::var("OMEGA_INFLIGHT_JOURNAL")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data/inflight_journal.jsonl"))
}

/// Open the configured journal or log and return None on failure
/// (caller must fail closed at phase ≥ 1 if journal is required).
pub fn open_default_journal() -> Option<InFlightJournal> {
    let path = default_journal_path();
    match InFlightJournal::open(&path) {
        Ok(j) => {
            info!(path = %path.display(), "InFlightJournal opened");
            Some(j)
        }
        Err(e) => {
            error!(
                path = %path.display(),
                error = %e,
                "InFlightJournal failed to open — crash recovery unavailable"
            );
            None
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "omega_inflight_{}_{}.jsonl",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn sample(status: InFlightStatus, bp: &str) -> InFlightRecord {
        InFlightRecord {
            recorded_at: Utc::now(),
            status,
            strategy_id: "SA".into(),
            nonce: 1,
            bundle_hash: "0xbundle".into(),
            blueprint_hash: bp.into(),
            chain_id: 42161,
            target_block: 100,
            expected_profit_net_wei: 1_000,
            idempotency_key_hex: "0xidem".into(),
        }
    }

    #[test]
    fn submitted_survives_recover() {
        let path = tmp_path("survives");
        let j = InFlightJournal::open(&path).unwrap();
        j.record_submitted(sample(InFlightStatus::Submitted, "0xaaa"))
            .unwrap();
        let open = j.recover_open().unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].blueprint_hash, "0xaaa");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn included_clears_open() {
        let path = tmp_path("clears");
        let j = InFlightJournal::open(&path).unwrap();
        j.record_submitted(sample(InFlightStatus::Submitted, "0xbbb"))
            .unwrap();
        // tiny delay so timestamps differ (not required for correctness)
        std::thread::sleep(Duration::from_millis(1));
        j.record_terminal(
            "0xbbb",
            "0xbundle",
            InFlightStatus::Included,
            "SA",
            1,
            42161,
            1_000,
            100,
            "0xidem",
        )
        .unwrap();
        let open = j.recover_open().unwrap();
        assert!(open.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn last_write_wins_per_blueprint() {
        let path = tmp_path("lww");
        let j = InFlightJournal::open(&path).unwrap();
        j.record_submitted(sample(InFlightStatus::Submitted, "0xccc"))
            .unwrap();
        j.record_submitted(sample(InFlightStatus::Submitted, "0xddd"))
            .unwrap();
        j.record_terminal(
            "0xccc",
            "0xbundle",
            InFlightStatus::Missed,
            "SA",
            1,
            42161,
            0,
            100,
            "0xidem",
        )
        .unwrap();
        let open = j.recover_open().unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].blueprint_hash, "0xddd");
        let _ = std::fs::remove_file(&path);
    }
}