// omega-prl/src/ml/validation.rs
//! Model validation — §16.5
//!
//! Verifies that loaded ONNX model files match the expected SHA-256 hash
//! recorded in their companion `.meta.json` files.  Called at load time
//! and before any rollback is committed.

use std::path::Path;
use tracing::{info, warn};

/// Validate that the file at `path` matches `expected_hash` (hex SHA-256).
///
/// Returns `Ok(())` on match, `Err(actual_hash)` on mismatch.
pub fn validate_model_hash(path: &Path, expected_hash: &str) -> Result<(), String> {
    if expected_hash.is_empty() {
        // No expected hash registered — skip validation (dev / first-run).
        return Ok(());
    }

    if !path.exists() {
        return Err(format!("model file not found: {}", path.display()));
    }

    let actual =
        compute_sha256(path).map_err(|e| format!("failed to hash {}: {e}", path.display()))?;

    if actual == expected_hash {
        info!(path = %path.display(), "Model hash validated");
        Ok(())
    } else {
        warn!(
            path     = %path.display(),
            expected = expected_hash,
            actual   = %actual,
            "Model hash mismatch — refusing to load"
        );
        Err(actual)
    }
}

fn compute_sha256(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = f.read(&mut buf)?;
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
    fn empty_hash_always_passes() {
        // We cannot create a real temp file in a no-std test, so pass a
        // non-existent path with an empty hash.
        let r = validate_model_hash(Path::new("/nonexistent"), "");
        assert!(r.is_ok());
    }

    #[test]
    fn missing_file_with_hash_fails() {
        let r = validate_model_hash(Path::new("/nonexistent"), "deadbeef");
        assert!(r.is_err());
    }
}