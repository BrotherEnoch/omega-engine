ï»¿// crates/omega-health/src/persistence.rs
//
// HealthLog â€” append-only, newline-delimited JSON log of all health FSM
// transitions.
//
// Spec Â§16:
//   Health state transitions are always-sampled events (100% sampling
//   rate).  The log is the primary audit record â€” the tracing event in
//   state_machine.rs is secondary.
//
// ## File format
//
//   One JSON object per line (NDJSON / JSON Lines).
//   Each line is a serialised `HealthLogEntry`.
//   Lines are terminated with '\n' and flushed immediately after each
//   write so that crash recovery sees a complete record for every
//   applied transition.
//
// ## Rotation
//
//   `HealthLog::open` opens in append mode.  Rotation is the
//   responsibility of the operator (logrotate / systemd) â€” the engine
//   does not rotate the file itself.  After a rotation signal the
//   operator should call `HealthLog::reopen` so the writer targets the
//   new file descriptor.
//
// ## Durability
//
//   `append` calls `flush` after every write.  `BufWriter` is used for
//   throughput â€” transitions are infrequent (<1/s in normal operation)
//   so the buffering is conservative.  If stronger durability is needed
//   (survives kernel crash) callers should wrap with `sync_all` â€” not
//   done here because health log loss is recoverable from the tracing
//   stream, unlike Vault profit records.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// HealthLogEntry
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// A single persisted health FSM transition record.
///
/// Serialised as one line of NDJSON in the health log file.
/// All string fields use the canonical Display representations from
/// omega-core (e.g. "HEALTHY", "relay") so external consumers can
/// parse without knowledge of the Rust enum layout.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthLogEntry {
    /// UTC timestamp of the transition (ISO-8601 / RFC-3339).
    pub timestamp:  DateTime<Utc>,

    /// Layer that transitioned â€” canonical snake_case label (Â§2).
    /// Example: `"relay"`, `"loss_attribution"`.
    pub layer_id:   String,

    /// State before the transition.
    /// One of: `"HEALTHY"`, `"DEGRADED"`, `"HALTED"`.
    pub from_state: String,

    /// State after the transition.
    pub to_state:   String,

    /// Human-readable reason supplied by the caller.
    pub reason:     String,
}

impl HealthLogEntry {
    /// Validate that `from_state` and `to_state` contain recognised
    /// values and that the transition is not a no-op.
    ///
    /// Returns `Err` if the entry would represent an invalid or
    /// uncrossable FSM transition.  Used in tests and in any
    /// replay/import tooling.
    pub fn validate(&self) -> Result<(), String> {
        const VALID: &[&str] = &["HEALTHY", "DEGRADED", "HALTED"];
        if !VALID.contains(&self.from_state.as_str()) {
            return Err(format!("invalid from_state: {}", self.from_state));
        }
        if !VALID.contains(&self.to_state.as_str()) {
            return Err(format!("invalid to_state: {}", self.to_state));
        }
        if self.from_state == self.to_state {
            return Err(format!(
                "no-op entry: {} â†’ {} for layer {}",
                self.from_state, self.to_state, self.layer_id,
            ));
        }
        // Blocked transition: HALTED â†’ DEGRADED
        if self.from_state == "HALTED" && self.to_state == "DEGRADED" {
            return Err(format!(
                "blocked transition HALTED â†’ DEGRADED for layer {}; \
                 recovery must go through HEALTHY",
                self.layer_id,
            ));
        }
        Ok(())
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// HealthLog
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Append-only NDJSON health transition log.
///
/// `HealthLog` is not `Clone` â€” there must be exactly one writer per
/// log file.  The `LayerHealthImpl` in `state_machine.rs` wraps it in
/// a `Mutex<HealthLog>` for shared access.
pub struct HealthLog {
    path:   PathBuf,
    writer: BufWriter<File>,
}

impl HealthLog {
    /// Open (or create) the log file at `path` in append mode.
    ///
    /// Fails if the path is not writable.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;

        Ok(Self {
            path:   path.to_owned(),
            writer: BufWriter::new(file),
        })
    }

    /// Append one entry to the log.
    ///
    /// Serialises to JSON, writes a newline terminator, and flushes the
    /// `BufWriter`.  Every successfully returned call guarantees the
    /// entry is visible to the OS (buffered write, not synced to disk).
    ///
    /// Returns `Err` if serialisation or I/O fails.  On error the
    /// caller (state_machine.rs) logs and continues â€” persistence
    /// failure must NOT block or revert the health state transition.
    pub fn append(&mut self, entry: &HealthLogEntry) -> anyhow::Result<()> {
        serde_json::to_writer(&mut self.writer, entry)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }

    /// Re-open the log file, discarding the current file descriptor.
    ///
    /// Called after a log rotation signal (e.g. SIGHUP or a governance
    /// hot-reload that changes the log path).  Flushes any buffered
    /// data before closing the old descriptor.
    pub fn reopen(&mut self) -> anyhow::Result<()> {
        self.writer.flush()?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.writer = BufWriter::new(file);
        Ok(())
    }

    /// Flush buffered data without closing the file.
    pub fn flush(&mut self) -> anyhow::Result<()> {
        self.writer.flush().map_err(anyhow::Error::from)
    }

    /// Path this log writes to.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;

    fn make_entry(from: &str, to: &str) -> HealthLogEntry {
        HealthLogEntry {
            timestamp:  Utc::now(),
            layer_id:   "relay".to_string(),
            from_state: from.to_string(),
            to_state:   to.to_string(),
            reason:     "test".to_string(),
        }
    }

    #[test]
    fn append_and_read_back() {
        let dir  = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("health.log");
        let mut log = HealthLog::open(&path).expect("open");

        let entry = make_entry("HEALTHY", "DEGRADED");
        log.append(&entry).expect("append");

        // Read back and verify
        let file    = File::open(&path).expect("open for read");
        let mut lines = std::io::BufReader::new(file).lines();
        let line    = lines.next().expect("at least one line").expect("read line");
        let parsed: HealthLogEntry = serde_json::from_str(&line).expect("parse");
        assert_eq!(parsed.from_state, "HEALTHY");
        assert_eq!(parsed.to_state,   "DEGRADED");
        assert_eq!(parsed.layer_id,   "relay");
    }

    #[test]
    fn multiple_entries_each_on_own_line() {
        let dir  = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("health.log");
        let mut log = HealthLog::open(&path).expect("open");

        log.append(&make_entry("HEALTHY",  "DEGRADED")).unwrap();
        log.append(&make_entry("DEGRADED", "HALTED")).unwrap();

        let file  = File::open(&path).expect("read");
        let lines: Vec<_> = std::io::BufReader::new(file)
            .lines()
            .collect::<Result<_, _>>()
            .expect("read lines");
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn append_mode_preserves_existing_entries() {
        let dir  = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("health.log");

        {
            let mut log = HealthLog::open(&path).expect("open first");
            log.append(&make_entry("HEALTHY", "DEGRADED")).unwrap();
        }
        {
            let mut log = HealthLog::open(&path).expect("open second");
            log.append(&make_entry("DEGRADED", "HALTED")).unwrap();
        }

        let file  = File::open(&path).expect("read");
        let count = std::io::BufReader::new(file).lines().count();
        assert_eq!(count, 2, "second open must not truncate");
    }

    #[test]
    fn entry_validation_rejects_noop() {
        let e = make_entry("HEALTHY", "HEALTHY");
        assert!(e.validate().is_err());
    }

    #[test]
    fn entry_validation_rejects_halted_to_degraded() {
        let e = make_entry("HALTED", "DEGRADED");
        assert!(e.validate().is_err());
    }

    #[test]
    fn entry_validation_rejects_invalid_state_name() {
        let e = make_entry("UNKNOWN", "HEALTHY");
        assert!(e.validate().is_err());
    }

    #[test]
    fn entry_validation_accepts_valid_transitions() {
        let valid = [
            ("HEALTHY",  "DEGRADED"),
            ("HEALTHY",  "HALTED"),
            ("DEGRADED", "HALTED"),
            ("DEGRADED", "HEALTHY"),
            ("HALTED",   "HEALTHY"),
        ];
        for (from, to) in valid {
            let e = make_entry(from, to);
            assert!(e.validate().is_ok(), "expected valid: {from} â†’ {to}");
        }
    }
}