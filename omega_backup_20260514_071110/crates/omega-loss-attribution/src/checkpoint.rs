ï»¿// crates/omega-loss-attribution/src/checkpoint.rs
//
// Gas model checkpoint persistence (spec Â§13.2, fix I1).
//
// ## Spec Â§13.2
//
//   Checkpoint path: /var/omega/checkpoints/gas-model-checkpoint-{version}.bin
//   Format: bincode-serialised ModelCheckpoint
//   Retention: keep last 10 checkpoints; prune older ones
//   API: GET /api/v1/la/gas-model/checkpoints â†’ list with version, win_rate,
//        sample_count
//
// ## File naming
//
//   gas-model-checkpoint-{version}.bin
//   where version = total_losses / checkpoint_interval
//   (e.g. version 5 = after 5,000 losses at interval 1,000)
//
// ## Durability
//
//   `save` calls `File::sync_all()` after writing â€” the checkpoint is a
//   safety-critical record.  If the process crashes between a model
//   update and the next checkpoint, the engine reverts to the last
//   synced version rather than operating with an unvalidated model.
//
// ## Pruning
//
//   After save, `prune` removes all but the `retention` most recent
//   versions (default 10, from `MlConfig::checkpoint_retention`).
//   Pruning is best-effort: failure to delete an old checkpoint is
//   logged at WARN but does not fail the save.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::classifier::FeatureKey;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ModelCheckpoint
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Serialised state of the online gas model at a validation boundary (Â§13.2).
///
/// Saved every `MlConfig::checkpoint_interval` losses (default 1,000).
/// Loaded on startup to resume from the last validated state rather than
/// cold-starting the multiplier map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCheckpoint {
    /// Checkpoint sequence number = total_losses / checkpoint_interval.
    /// Used for file naming and API listing.
    pub version: u64,

    /// Holdout win rate at the time this checkpoint was validated (Â§13.1).
    /// The revert threshold check compares against this field.
    pub win_rate: f64,

    /// Full fee multiplier map at checkpoint time.
    /// Restored verbatim on revert (Â§13.1 fix C1).
    pub multipliers: HashMap<FeatureKey, f64>,

    /// UTC timestamp when this checkpoint was saved.
    pub saved_at: DateTime<Utc>,

    /// Total loss events seen at checkpoint time.
    pub sample_count: u64,

    /// 30-day rolling baseline win rate at checkpoint time.
    /// Used by the ceiling escalation path (Â§13.3) to compare against
    /// post-escalation performance.
    pub baseline_win_rate: f64,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// CheckpointError
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path:   PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Bincode serialisation error: {0}")]
    Serialise(#[from] bincode::Error),

    #[error("Checkpoint directory does not exist: {0}")]
    DirMissing(PathBuf),
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// path helpers
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn checkpoint_path(dir: &Path, version: u64) -> PathBuf {
    dir.join(format!("gas-model-checkpoint-{version}.bin"))
}

/// Parse the version number from a checkpoint filename.
///
/// Accepts: `gas-model-checkpoint-{N}.bin`
/// Returns `None` for any filename that does not match the pattern.
fn parse_version(file_name: &str) -> Option<u64> {
    let stem = file_name.strip_suffix(".bin")?;
    let rest = stem.strip_prefix("gas-model-checkpoint-")?;
    rest.parse().ok()
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// save
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Serialise and persist a checkpoint to `dir`.
///
/// Creates `dir` if it does not exist.  Writes atomically via a `.tmp`
/// file and then renames, so a crash during write never leaves a
/// partially-written checkpoint.  Calls `sync_all` before rename to
/// ensure durability.
///
/// After a successful save, `prune(dir, retention)` is called to remove
/// old checkpoints.
pub fn save(
    checkpoint: &ModelCheckpoint,
    dir:        &Path,
    retention:  usize,
) -> Result<(), CheckpointError> {
    // â”€â”€ Create directory if needed â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    fs::create_dir_all(dir).map_err(|e| CheckpointError::Io {
        path:   dir.to_owned(),
        source: e,
    })?;

    let final_path = checkpoint_path(dir, checkpoint.version);
    let tmp_path   = final_path.with_extension("bin.tmp");

    // â”€â”€ Write to .tmp â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    {
        let file = File::create(&tmp_path).map_err(|e| CheckpointError::Io {
            path:   tmp_path.clone(),
            source: e,
        })?;
        let mut writer = BufWriter::new(&file);
        let bytes = bincode::serialize(checkpoint)?;
        writer.write_all(&bytes).map_err(|e| CheckpointError::Io {
            path:   tmp_path.clone(),
            source: e,
        })?;
        writer.flush().map_err(|e| CheckpointError::Io {
            path:   tmp_path.clone(),
            source: e,
        })?;
        // Sync before rename for durability
        file.sync_all().map_err(|e| CheckpointError::Io {
            path:   tmp_path.clone(),
            source: e,
        })?;
    }

    // â”€â”€ Atomic rename â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    fs::rename(&tmp_path, &final_path).map_err(|e| CheckpointError::Io {
        path:   final_path.clone(),
        source: e,
    })?;

    tracing::info!(
        version      = checkpoint.version,
        win_rate     = checkpoint.win_rate,
        sample_count = checkpoint.sample_count,
        path         = %final_path.display(),
        "Gas model checkpoint saved",
    );

    // â”€â”€ Prune old checkpoints â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    prune(dir, retention);

    Ok(())
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// load_latest
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Load the highest-version checkpoint from `dir`.
///
/// Returns `None` when no checkpoint files are present (fresh deployment).
/// Returns `Err` only on I/O or deserialisation failure of the candidate
/// file â€” corrupt old versions are skipped (logged at WARN) until a
/// readable version is found.
pub fn load_latest(dir: &Path) -> Result<Option<ModelCheckpoint>, CheckpointError> {
    if !dir.exists() {
        return Ok(None);
    }

    let mut versions = list_versions(dir)?;
    if versions.is_empty() {
        return Ok(None);
    }

    // Sort descending â€” try highest version first
    versions.sort_unstable_by(|a, b| b.cmp(a));

    for version in versions {
        let path = checkpoint_path(dir, version);
        match load_one(&path) {
            Ok(ckpt) => {
                tracing::info!(
                    version      = ckpt.version,
                    win_rate     = ckpt.win_rate,
                    sample_count = ckpt.sample_count,
                    "Gas model checkpoint loaded",
                );
                return Ok(Some(ckpt));
            }
            Err(e) => {
                tracing::warn!(
                    version = version,
                    error   = %e,
                    "Skipping corrupt checkpoint â€” trying older version",
                );
            }
        }
    }

    Ok(None)
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// load_version
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Load a specific checkpoint version from `dir`.
///
/// Used by the governance API endpoint
/// `POST /api/v1/la/gas-model/revert/{version}` (Â§17.2).
pub fn load_version(dir: &Path, version: u64) -> Result<ModelCheckpoint, CheckpointError> {
    let path = checkpoint_path(dir, version);
    load_one(&path)
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// list_checkpoints
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// List all checkpoint versions present in `dir`, sorted ascending.
///
/// Used by the governance API endpoint
/// `GET /api/v1/la/gas-model/checkpoints` (Â§17.2).
pub fn list_checkpoints(dir: &Path) -> Result<Vec<CheckpointMeta>, CheckpointError> {
    if !dir.exists() {
        return Ok(vec![]);
    }

    let mut versions = list_versions(dir)?;
    versions.sort_unstable();

    let mut metas = Vec::with_capacity(versions.len());
    for version in versions {
        let path = checkpoint_path(dir, version);
        match load_one(&path) {
            Ok(ckpt) => metas.push(CheckpointMeta {
                version:      ckpt.version,
                win_rate:     ckpt.win_rate,
                sample_count: ckpt.sample_count,
                saved_at:     ckpt.saved_at,
            }),
            Err(e) => {
                tracing::warn!(version, error = %e, "Skipping unreadable checkpoint in list");
            }
        }
    }

    Ok(metas)
}

/// Lightweight metadata for the checkpoint listing API (Â§17.2).
#[derive(Debug, Clone, Serialize)]
pub struct CheckpointMeta {
    pub version:      u64,
    pub win_rate:     f64,
    pub sample_count: u64,
    pub saved_at:     DateTime<Utc>,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// prune
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Remove old checkpoint files, keeping only the `retention` most recent.
///
/// Best-effort: deletion failures are logged at WARN and do not propagate.
fn prune(dir: &Path, retention: usize) {
    let mut versions = match list_versions(dir) {
        Ok(v)  => v,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to list checkpoints for pruning");
            return;
        }
    };

    if versions.len() <= retention {
        return;
    }

    // Sort ascending â€” oldest first
    versions.sort_unstable();
    let to_delete = versions.len() - retention;

    for version in versions.into_iter().take(to_delete) {
        let path = checkpoint_path(dir, version);
        match fs::remove_file(&path) {
            Ok(()) => {
                tracing::info!(version, "Pruned old gas model checkpoint");
            }
            Err(e) => {
                tracing::warn!(
                    version,
                    path = %path.display(),
                    error = %e,
                    "Failed to prune checkpoint â€” continuing",
                );
            }
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// internal helpers
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn load_one(path: &Path) -> Result<ModelCheckpoint, CheckpointError> {
    let bytes = fs::read(path).map_err(|e| CheckpointError::Io {
        path:   path.to_owned(),
        source: e,
    })?;
    let ckpt: ModelCheckpoint = bincode::deserialize(&bytes)?;
    Ok(ckpt)
}

fn list_versions(dir: &Path) -> Result<Vec<u64>, CheckpointError> {
    let entries = fs::read_dir(dir).map_err(|e| CheckpointError::Io {
        path:   dir.to_owned(),
        source: e,
    })?;

    let versions = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name  = entry.file_name();
            let name  = name.to_string_lossy();
            parse_version(&name)
        })
        .collect();

    Ok(versions)
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_checkpoint(version: u64, win_rate: f64) -> ModelCheckpoint {
        ModelCheckpoint {
            version,
            win_rate,
            multipliers:       HashMap::new(),
            saved_at:          Utc::now(),
            sample_count:      version * 1000,
            baseline_win_rate: 0.60,
        }
    }

    #[test]
    fn save_and_load_latest() {
        let dir = tempfile::tempdir().unwrap();
        let ckpt = make_checkpoint(3, 0.72);
        save(&ckpt, dir.path(), 10).unwrap();

        let loaded = load_latest(dir.path()).unwrap().expect("checkpoint present");
        assert_eq!(loaded.version,      3);
        assert!((loaded.win_rate - 0.72).abs() < 1e-9);
        assert_eq!(loaded.sample_count, 3000);
    }

    #[test]
    fn load_latest_returns_highest_version() {
        let dir = tempfile::tempdir().unwrap();
        save(&make_checkpoint(1, 0.60), dir.path(), 10).unwrap();
        save(&make_checkpoint(5, 0.75), dir.path(), 10).unwrap();
        save(&make_checkpoint(3, 0.70), dir.path(), 10).unwrap();

        let loaded = load_latest(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.version, 5, "must return highest version");
    }

    #[test]
    fn load_latest_on_empty_dir_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_latest(dir.path()).unwrap().is_none());
    }

    #[test]
    fn load_latest_on_nonexistent_dir_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does_not_exist");
        assert!(load_latest(&missing).unwrap().is_none());
    }

    #[test]
    fn load_specific_version() {
        let dir = tempfile::tempdir().unwrap();
        save(&make_checkpoint(7, 0.80), dir.path(), 10).unwrap();
        let loaded = load_version(dir.path(), 7).unwrap();
        assert_eq!(loaded.version, 7);
    }

    #[test]
    fn pruning_keeps_only_retention_count() {
        let dir = tempfile::tempdir().unwrap();
        for v in 1..=15_u64 {
            save(&make_checkpoint(v, 0.5 + v as f64 * 0.01), dir.path(), 10).unwrap();
        }
        // After saving version 15, only 10 checkpoints should remain
        let versions = list_versions(dir.path()).unwrap();
        assert_eq!(versions.len(), 10, "pruning must retain exactly 10 files");
        // Versions 1..5 must have been pruned
        for v in 1..=5_u64 {
            assert!(
                !checkpoint_path(dir.path(), v).exists(),
                "version {v} should have been pruned",
            );
        }
        // Versions 6..15 must remain
        for v in 6..=15_u64 {
            assert!(
                checkpoint_path(dir.path(), v).exists(),
                "version {v} should be retained",
            );
        }
    }

    #[test]
    fn list_checkpoints_sorted_ascending() {
        let dir = tempfile::tempdir().unwrap();
        for v in [3_u64, 1, 5, 2, 4] {
            save(&make_checkpoint(v, 0.5 + v as f64 * 0.01), dir.path(), 10).unwrap();
        }
        let metas = list_checkpoints(dir.path()).unwrap();
        let versions: Vec<u64> = metas.iter().map(|m| m.version).collect();
        assert_eq!(versions, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn parse_version_correct() {
        assert_eq!(parse_version("gas-model-checkpoint-42.bin"), Some(42));
        assert_eq!(parse_version("gas-model-checkpoint-0.bin"),  Some(0));
        assert_eq!(parse_version("gas-model-checkpoint-.bin"),   None);
        assert_eq!(parse_version("other-file.bin"),               None);
        assert_eq!(parse_version("gas-model-checkpoint-abc.bin"), None);
    }
}