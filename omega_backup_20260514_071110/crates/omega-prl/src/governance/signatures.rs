// omega-prl/src/governance/signatures.rs
//! Governance audit log â€” Â§20.1
//!
//! Every governance action is appended to an in-memory append-only log.
//! The log cannot be cleared or truncated by any code path â€” structural
//! guarantee (no `clear()`, no `remove()`).
//!
//! Production: the log is also persisted to the WAL at
//! `/var/omega/prl/governance/audit.wal` for cross-restart durability.

use serde::{Deserialize, Serialize};
use tracing::info;

/// Single governance action record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceAction {
    pub action_id:      u64,
    pub action_type:    GovernanceActionType,
    pub actor:          String,
    pub timestamp_secs: u64,
    pub description:    String,
    pub is_emergency:   bool,
    /// Hash of resulting state (for deterministic verification).
    pub state_hash:     [u8; 32],
}

/// Governance action categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum GovernanceActionType {
    ThresholdUpdate   = 0,
    SignatureDeployed = 1,
    SignatureReverted = 2,
    ModelReverted     = 3,
    BlacklistApproval = 4,
    EmergencyPatch    = 5,
}

/// Append-only governance audit log (Â§20.1).
///
/// Governance MAY NOT disable this log.
pub struct GovernanceAuditLog {
    actions: Vec<GovernanceAction>,
    next_id: u64,
}

impl GovernanceAuditLog {
    pub fn new() -> Self {
        Self { actions: Vec::new(), next_id: 1 }
    }

    /// Append an action.  Returns the assigned `action_id`.
    pub fn append(
        &mut self,
        action_type:  GovernanceActionType,
        actor:        &str,
        description:  &str,
        is_emergency: bool,
        timestamp:    u64,
        state_hash:   [u8; 32],
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.actions.push(GovernanceAction {
            action_id:      id,
            action_type,
            actor:          actor.to_string(),
            timestamp_secs: timestamp,
            description:    description.to_string(),
            is_emergency,
            state_hash,
        });
        info!(id, actor, is_emergency, action_type = ?action_type,
            "Governance action recorded");
        id
    }

    pub fn actions(&self) -> &[GovernanceAction] { &self.actions }
    pub fn len(&self)     -> usize { self.actions.len() }
    pub fn is_empty(&self) -> bool { self.actions.is_empty() }

    /// Most recent action, if any.
    pub fn latest(&self) -> Option<&GovernanceAction> { self.actions.last() }
}

impl Default for GovernanceAuditLog {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_increments_id() {
        let mut log = GovernanceAuditLog::new();
        let id1 = log.append(GovernanceActionType::ThresholdUpdate,
            "alice", "update 1", false, 0, [0; 32]);
        let id2 = log.append(GovernanceActionType::SignatureDeployed,
            "alice", "update 2", false, 1, [0; 32]);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
    }

    #[test]
    fn log_is_append_only_structurally() {
        // Compile-time: GovernanceAuditLog has no clear() or remove() method.
        let mut log = GovernanceAuditLog::new();
        log.append(GovernanceActionType::EmergencyPatch,
            "bot", "emergency", true, 0, [0; 32]);
        assert_eq!(log.len(), 1);
    }
}