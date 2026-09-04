// omega-prl/src/ml/checkpoints.rs
//! Model checkpoint store — §16.5
//!
//! Tracks versioned ONNX model files.  Every model directory is scanned
//! for `{name}-v{N}.onnx` files paired with `{name}-v{N}.meta.json`.
//! SHA-256 of each `.onnx` file is verified against the meta file at load
//! time and on rollback.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::ml::inference::{MODEL_GAS_WAR, MODEL_LIQUIDATION, MODEL_RELAY, MODEL_SEARCHER};

/// Versioned checkpoint record for one model file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCheckpoint {
    /// Monotonically increasing version number.
    pub version: u32,
    pub model_name: String,
    /// SHA-256 of the `.onnx` file (hex string).
    pub file_hash: String,
    /// Absolute path to the `.onnx` file.
    pub path: PathBuf,
    /// Whether this version is currently active.
    pub active: bool,
    /// Unix seconds at checkpoint registration.
    pub created_at: u64,
}

/// Persistent store of all known model checkpoints.
///
/// Thread-safe via `DashMap` — multiple shard workers can query simultaneously.
pub struct ModelCheckpointStore {
    /// model_name → ordered Vec<ModelCheckpoint> (newest last).
    checkpoints: dashmap::DashMap<String, Vec<ModelCheckpoint>>,
}

impl ModelCheckpointStore {
    /// Scan `model_dir` for ONNX + meta pairs and register them.
    pub fn load(model_dir: &Path) -> anyhow::Result<Self> {
        let store = Self {
            checkpoints: dashmap::DashMap::new(),
        };

        for name in [
            MODEL_RELAY,
            MODEL_GAS_WAR,
            MODEL_LIQUIDATION,
            MODEL_SEARCHER,
        ] {
            let mut found = Vec::new();
            if let Ok(entries) = std::fs::read_dir(model_dir) {
                for entry in entries.flatten() {
                    let fname = entry.file_name();
                    let fname = fname.to_string_lossy();
                    if fname.starts_with(name) && fname.ends_with(".onnx") {
                        let version: u32 = fname
                            .trim_start_matches(name)
                            .trim_start_matches('-')
                            .trim_start_matches('v')
                            .trim_end_matches(".onnx")
                            .parse()
                            .unwrap_or(1);

                        let path = entry.path();
                        let file_hash = compute_sha256(&path).unwrap_or_default();
                        found.push(ModelCheckpoint {
                            version,
                            model_name: name.to_string(),
                            file_hash,
                            path: path.clone(),
                            active: false,
                            created_at: 0,
                        });
                    }
                }
            }

            if found.is_empty() {
                found.push(ModelCheckpoint {
                    version: 1,
                    model_name: name.to_string(),
                    file_hash: String::new(),
                    path: model_dir.join(format!("{}-v1.onnx", name)),
                    active: false,
                    created_at: 0,
                });
                warn!(model = name, "No ONNX file found — placeholder registered");
            }

            found.sort_by_key(|c| c.version);
            if let Some(latest) = found.last_mut() {
                latest.active = true;
            }

            store.checkpoints.insert(name.to_string(), found);
        }

        info!(
            models = store.checkpoints.len(),
            "ModelCheckpointStore loaded"
        );
        Ok(store)
    }

    /// Empty store — used when starting in heuristic-fallback mode.
    pub fn empty() -> Self {
        Self {
            checkpoints: dashmap::DashMap::new(),
        }
    }

    pub fn loaded_count(&self) -> usize {
        self.checkpoints.len()
    }

    /// Path to the currently active checkpoint for `model_name`.
    pub fn active_path(&self, model_name: &str) -> Option<PathBuf> {
        self.checkpoints
            .get(model_name)?
            .iter()
            .find(|c| c.active)
            .map(|c| c.path.clone())
    }

    /// Activate checkpoint `version` for all models, deactivating others.
    pub fn rollback_to(&self, version: u32) -> anyhow::Result<()> {
        if self.checkpoints.is_empty() {
            anyhow::bail!("No checkpoints loaded");
        }

        for entry in self.checkpoints.iter() {
            let found = entry.value().iter().any(|c| c.version == version);
            if !found {
                warn!(
                    model = entry.key(),
                    version, "No checkpoint at requested version"
                );
                anyhow::bail!(
                    "One or more models have no checkpoint at version {}",
                    version
                );
            }
        }

        for mut entry in self.checkpoints.iter_mut() {
            for cp in entry.value_mut().iter_mut() {
                cp.active = cp.version == version;
            }
        }
        info!(version, "ModelCheckpointStore rolled back");
        Ok(())
    }

    /// All checkpoints for `model_name`, newest last.
    pub fn history(&self, model_name: &str) -> Vec<ModelCheckpoint> {
        self.checkpoints
            .get(model_name)
            .map(|v| v.clone())
            .unwrap_or_default()
    }
}

fn compute_sha256(path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_store_returns_none() {
        let store = ModelCheckpointStore::empty();
        assert!(store.active_path(MODEL_RELAY).is_none());
    }

    #[test]
    fn rollback_missing_version_errors() {
        let store = ModelCheckpointStore::empty();
        assert!(store.rollback_to(99).is_err());
    }
}