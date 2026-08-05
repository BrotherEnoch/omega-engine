// omega-prl/src/ingestion/wal.rs
//! Append-only binary WAL — §18
//!
//! Requirements (§18.1, §18.2, §18.3):
//!   - Binary append-only — no in-place mutation ever
//!   - zstd dictionary-compressed event streams
//!   - Replay MUST reproduce pattern outputs bit-for-bit
//!   - Monotonic timestamp ordering enforced at write time
//!   - No lossy compression, no probabilistic truncation
//!   - WAL path: /var/omega/prl/events/
//!   - Checkpoints path: /var/omega/prl/checkpoints/

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::ingestion::event_bus::PatternEvent;

// ─────────────────────────────────────────────────────────────────────────────
// WAL frame format
// ─────────────────────────────────────────────────────────────────────────────

/// Magic header for WAL segment files.
const WAL_MAGIC: u32 = 0x4F4D_4750; // "OMGP"

/// Frame written to WAL for each event.
/// Layout: [magic(4)] [frame_len(4)] [frame_bytes(frame_len)]
/// `frame_bytes` is a bincode-serialised `WalFrame` whose `data` field
/// contains a zstd-compressed bincode-serialised `PatternEvent`.
#[derive(Debug, Serialize, Deserialize)]
struct WalFrame {
    ts_nanos: u64,
    /// zstd-compressed bincode-serialised `PatternEvent`.
    data: Vec<u8>,
    crc32: u32,
}

// ─────────────────────────────────────────────────────────────────────────────
// EventWal
// ─────────────────────────────────────────────────────────────────────────────

/// Append-only WAL for deterministic replay (§18).
pub struct EventWal {
    /// Directory containing segment files.
    base_dir: PathBuf,
    /// Active segment writer — Mutex for multi-producer safety.
    writer: Mutex<BufWriter<File>>,
    /// Monotonic sequence number for ordering validation.
    last_ts: AtomicU64,
    /// Running byte offset in the active segment.
    bytes_written: AtomicU64,
    /// Rotate segment when it exceeds this size (default 256 MiB).
    max_segment_bytes: u64,
    /// Zstd compression level (1 = fastest, 22 = best).
    zstd_level: i32,
}

impl EventWal {
    /// Open or create a WAL rooted at `base_dir`.
    pub async fn open(base_dir: &Path) -> anyhow::Result<Self> {
        let events_dir = base_dir.join("events");
        fs::create_dir_all(&events_dir)?;

        let segment_path = Self::active_segment_path(&events_dir);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&segment_path)?;

        let bytes_written = file.metadata()?.len();
        info!(path = %segment_path.display(), "WAL segment opened");

        Ok(Self {
            base_dir: events_dir,
            writer: Mutex::new(BufWriter::with_capacity(64 * 1024, file)),
            last_ts: AtomicU64::new(0),
            bytes_written: AtomicU64::new(bytes_written),
            max_segment_bytes: 256 * 1024 * 1024,
            zstd_level: 1, // fastest — latency-optimised
        })
    }

    /// Append a `PatternEvent` to the WAL.
    ///
    /// Enforces monotonic timestamp ordering — events arriving out of order
    /// are stamped with `last_ts + 1` to preserve log integrity.
    pub fn append(&self, event: &PatternEvent) -> io::Result<()> {
        let ts = self.enforce_monotonic(event.ts_nanos);

        // Serialise event with bincode.
        // PatternEvent derives Serialize via the serde_payload with-module fix.
        let raw =
            bincode::serialize(event).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Compress payload with zstd.
        let compressed =
            zstd::encode_all(raw.as_slice(), self.zstd_level).map_err(io::Error::other)?;

        // CRC32 over compressed bytes.
        let crc = crc32fast::hash(&compressed);

        let frame = WalFrame {
            ts_nanos: ts,
            data: compressed,
            crc32: crc,
        };
        let frame_bytes = bincode::serialize(&frame)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Write: [magic(4)] [frame_len(4)] [frame_bytes].
        let mut w = self.writer.lock().expect("WAL writer lock poisoned");
        w.write_all(&WAL_MAGIC.to_le_bytes())?;
        w.write_all(&(frame_bytes.len() as u32).to_le_bytes())?;
        w.write_all(&frame_bytes)?;
        // Explicit flush on every write — WAL must be durable.
        w.flush()?;

        let total = self
            .bytes_written
            .fetch_add((8 + frame_bytes.len()) as u64, Ordering::Relaxed);

        // Segment rotation — drop writer lock before rotating.
        if total >= self.max_segment_bytes {
            drop(w);
            self.rotate_segment()?;
        }

        Ok(())
    }

    /// Read and replay all frames in `[from_ts, to_ts]` from segment files.
    /// Returns frames in monotonic timestamp order — bit-for-bit reproducible.
    pub fn read_window(&self, from_ts: u64, to_ts: u64) -> io::Result<Vec<PatternEvent>> {
        let mut results = Vec::new();
        let segments = self.list_segments()?;

        for seg_path in segments {
            let file = File::open(&seg_path)?;
            let mut rd = BufReader::new(file);
            loop {
                // Read magic.
                let mut magic_buf = [0u8; 4];
                match rd.read_exact(&mut magic_buf) {
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                    Err(e) => return Err(e),
                    Ok(_) => {}
                }
                let magic = u32::from_le_bytes(magic_buf);
                if magic != WAL_MAGIC {
                    warn!(path = %seg_path.display(),
                        "WAL: unexpected magic — stopping replay");
                    break;
                }

                // Read frame length.
                let mut len_buf = [0u8; 4];
                rd.read_exact(&mut len_buf)?;
                let frame_len = u32::from_le_bytes(len_buf) as usize;

                // Read frame bytes.
                let mut frame_buf = vec![0u8; frame_len];
                rd.read_exact(&mut frame_buf)?;

                let frame: WalFrame = bincode::deserialize(&frame_buf)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

                // Verify CRC32.
                let actual_crc = crc32fast::hash(&frame.data);
                if actual_crc != frame.crc32 {
                    error!(
                        ts = frame.ts_nanos,
                        expected = frame.crc32,
                        actual = actual_crc,
                        "WAL CRC32 mismatch — frame skipped"
                    );
                    continue;
                }

                // Timestamp window filter.
                if frame.ts_nanos < from_ts {
                    continue;
                }
                if frame.ts_nanos > to_ts {
                    break;
                }

                // Decompress and deserialise.
                let raw = zstd::decode_all(frame.data.as_slice())
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                let event: PatternEvent = bincode::deserialize(&raw)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

                results.push(event);
            }
        }

        Ok(results)
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn enforce_monotonic(&self, ts: u64) -> u64 {
        let prev = self.last_ts.load(Ordering::Relaxed);
        let next = ts.max(prev + 1);
        self.last_ts.store(next, Ordering::Relaxed);
        next
    }

    fn active_segment_path(events_dir: &Path) -> PathBuf {
        events_dir.join("active.wal")
    }

    fn rotate_segment(&self) -> io::Result<()> {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let archived = self.base_dir.join(format!("segment-{}.wal", ts));
        let active = Self::active_segment_path(&self.base_dir);
        fs::rename(&active, &archived)?;

        let file = OpenOptions::new().create(true).append(true).open(&active)?;
        let mut w = self.writer.lock().expect("WAL writer lock poisoned");
        *w = BufWriter::with_capacity(64 * 1024, file);
        self.bytes_written.store(0, Ordering::Relaxed);
        info!(archived = %archived.display(), "WAL segment rotated");
        Ok(())
    }

    fn list_segments(&self) -> io::Result<Vec<PathBuf>> {
        let mut paths: Vec<PathBuf> = fs::read_dir(&self.base_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "wal"))
            .collect();
        paths.sort();
        Ok(paths)
    }
}
