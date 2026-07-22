// omega-prl/src/governance/rollback.rs
//! Signature set rollback manager â€” Â§20.1
//!
//! Every governance update to pattern signatures saves a versioned checkpoint
//! before applying.  On `rollback(version)` the previous set is restored and
//! atomically swapped into the `PatternMatcher` via `ArcSwap`.

use tracing::{info, warn};
use crate::patterns::signatures::PatternSignature;

/// Versioned snapshot of a signature set.
#[derive(Debug, Clone)]
pub struct SignatureCheckpoint {
    pub version:    u32,
    pub signatures: Vec<PatternSignature>,
    /// Unix seconds when the checkpoint was saved.
    pub saved_at:   u64,
    pub saved_by:   String,
}

/// Rolling history of signature checkpoints.
///
/// Retains at most `max_history` entries; oldest are pruned on overflow.
pub struct SignatureRollbackManager {
    history:     Vec<SignatureCheckpoint>,
    max_history: usize,
}

impl SignatureRollbackManager {
    pub fn new(max_history: usize) -> Self {
        Self { history: Vec::new(), max_history: max_history.max(1) }
    }

    /// Save a checkpoint before any governance update.
    pub fn save_checkpoint(
        &mut self,
        version:  u32,
        sigs:     Vec<PatternSignature>,
        saved_by: &str,
        now_secs: u64,
    ) {
        self.history.push(SignatureCheckpoint {
            version,
            signatures: sigs,
            saved_at:   now_secs,
            saved_by:   saved_by.to_string(),
        });
        if self.history.len() > self.max_history {
            self.history.remove(0);
        }
        info!(version, saved_by, "PRL signature checkpoint saved");
    }

    /// Look up a checkpoint by version.
    pub fn get(&self, version: u32) -> Option<&SignatureCheckpoint> {
        self.history.iter().find(|c| c.version == version)
    }

    /// Roll back to `version`.  Returns the signature set or an error.
    pub fn rollback(&self, version: u32) -> Result<Vec<PatternSignature>, String> {
        match self.get(version) {
            Some(cp) => {
                warn!(version, "PRL signature rollback executed");
                Ok(cp.signatures.clone())
            }
            None => Err(format!("No checkpoint at version {version}")),
        }
    }

    pub fn latest_version(&self) -> Option<u32> {
        self.history.last().map(|c| c.version)
    }

    pub fn len(&self) -> usize { self.history.len() }
    pub fn is_empty(&self) -> bool { self.history.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patterns::signatures::builtin_signatures;

    #[test]
    fn rollback_round_trip() {
        let mut mgr  = SignatureRollbackManager::new(10);
        let sigs     = builtin_signatures();
        let count    = sigs.len();
        mgr.save_checkpoint(1, sigs, "test", 0);
        let rolled = mgr.rollback(1).unwrap();
        assert_eq!(rolled.len(), count);
    }

    #[test]
    fn missing_version_errors() {
        let mgr = SignatureRollbackManager::new(10);
        assert!(mgr.rollback(99).is_err());
    }

    #[test]
    fn history_pruned_at_max() {
        let mut mgr = SignatureRollbackManager::new(2);
        for v in 1..=5 {
            mgr.save_checkpoint(v, vec![], "test", 0);
        }
        assert_eq!(mgr.len(), 2);
        // Only the last two versions retained
        assert!(mgr.get(4).is_some());
        assert!(mgr.get(5).is_some());
        assert!(mgr.get(1).is_none());
    }
}