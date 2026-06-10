// crates/omega-compliance/src/rules.rs
//
// Versioned OFA rule registry (spec §8).
//
// ## Rule set versioning
//
//   Rule sets are immutable once activated.  The registry holds the
//   full history of all rule sets and exposes the currently active one.
//
//   Activation semantics:
//     - A rule set is "active" when its `activated_at ≤ Utc::now()`.
//     - Among all active rule sets, the one with the highest version
//       number is the effective rule set.
//     - Downgrades are blocked: `register` returns `Err` when the new
//       version is ≤ the current active version.
//
//   Governance path: L2 fast-approve (§5).  The operator submits a
//   signed governance message containing the new `OfaRuleSet`.  The
//   control-plane validates the signature and calls `register`.
//
// ## Thread safety
//
//   `RuleRegistry` is `Send + Sync`.  The internal `Vec` is protected
//   by a `std::sync::RwLock`.  Reads (active_rule_set) are the hot
//   path and use a read lock.  Writes (register) are infrequent.

use std::sync::RwLock;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::ofa::OfaRuleSet;

// ─────────────────────────────────────────────────────────────────────────────
// RegistryError
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error(
        "Downgrade rejected: new version {new} ≤ active version {active}. \
         Rule sets can only be upgraded."
    )]
    VersionDowngrade { new: u32, active: u32 },

    #[error("Future activation rejected: rule sets must activate in the past or present")]
    FutureActivation,

    #[error("No active rule set found")]
    NoActiveRuleSet,
}

// ─────────────────────────────────────────────────────────────────────────────
// RuleSetEntry
// ─────────────────────────────────────────────────────────────────────────────

/// A stored rule set with its registration metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSetEntry {
    pub rule_set: OfaRuleSet,
    /// UTC timestamp when this rule set was registered in the engine.
    pub registered_at: chrono::DateTime<Utc>,
}

// ─────────────────────────────────────────────────────────────────────────────
// RuleRegistry
// ─────────────────────────────────────────────────────────────────────────────

/// Thread-safe versioned OFA rule registry.
///
/// Holds the full history of all rule sets; exposes the currently
/// effective one via `active_rule_set()`.
pub struct RuleRegistry {
    /// All registered rule sets, kept in ascending version order.
    entries: RwLock<Vec<RuleSetEntry>>,
}

impl RuleRegistry {
    /// Create a new registry pre-seeded with the default rule set.
    ///
    /// The default rule set (version 1) is activated at the Unix epoch
    /// so it is immediately effective without governance action.
    pub fn new() -> Self {
        let default = RuleSetEntry {
            rule_set: OfaRuleSet::default(),
            registered_at: Utc::now(),
        };
        Self {
            entries: RwLock::new(vec![default]),
        }
    }

    /// Register a new rule set.
    ///
    /// Returns `Err(RegistryError::VersionDowngrade)` when the new
    /// rule set's version is ≤ the current active version.
    ///
    /// Returns `Err(RegistryError::FutureActivation)` when
    /// `activated_at` is in the future — rule sets must not be
    /// pre-scheduled via this path (future scheduling is a governance
    /// concern handled at the API layer).
    pub fn register(&self, rule_set: OfaRuleSet) -> Result<(), RegistryError> {
        let now = Utc::now();

        // Block future-dated activations
        if rule_set.activated_at > now {
            return Err(RegistryError::FutureActivation);
        }

        let mut entries = self.entries.write().expect("rule registry RwLock poisoned");

        // Find the current highest-version active rule set
        let active_version = entries
            .iter()
            .filter(|e| e.rule_set.activated_at <= now)
            .map(|e| e.rule_set.version)
            .max()
            .unwrap_or(0);

        if rule_set.version <= active_version {
            return Err(RegistryError::VersionDowngrade {
                new: rule_set.version,
                active: active_version,
            });
        }

        tracing::info!(
            version      = rule_set.version,
            activated_at = %rule_set.activated_at,
            consent_max_age_secs     = rule_set.consent_max_age_secs,
            backrun_slippage_cap_bps = rule_set.backrun_slippage_cap_bps,
            "OFA rule set registered (L2 fast-approve)",
        );

        entries.push(RuleSetEntry {
            rule_set,
            registered_at: now,
        });

        // Keep entries sorted by version ascending for deterministic iteration
        entries.sort_by_key(|e| e.rule_set.version);

        Ok(())
    }

    /// Return the currently effective rule set.
    ///
    /// "Effective" = highest version among all rule sets whose
    /// `activated_at ≤ Utc::now()`.
    ///
    /// Returns `Err(RegistryError::NoActiveRuleSet)` only when the
    /// registry was somehow constructed with no entries — this cannot
    /// happen via the normal `new()` constructor.
    pub fn active_rule_set(&self) -> Result<OfaRuleSet, RegistryError> {
        let now = Utc::now();
        let entries = self.entries.read().expect("rule registry RwLock poisoned");

        entries
            .iter()
            .filter(|e| e.rule_set.activated_at <= now)
            .max_by_key(|e| e.rule_set.version)
            .map(|e| e.rule_set.clone())
            .ok_or(RegistryError::NoActiveRuleSet)
    }

    /// List all registered rule set entries, newest first.
    pub fn history(&self) -> Vec<RuleSetEntry> {
        let entries = self.entries.read().expect("rule registry RwLock poisoned");
        let mut h: Vec<RuleSetEntry> = entries.clone();
        h.sort_by_key(|entry| std::cmp::Reverse(entry.rule_set.version));
        h
    }

    /// Number of rule sets currently in the registry.
    pub fn len(&self) -> usize {
        self.entries.read().expect("RwLock poisoned").len()
    }

    /// Returns `true` when the registry contains no rule sets.
    pub fn is_empty(&self) -> bool {
        self.entries.read().expect("RwLock poisoned").is_empty()
    }

    /// Returns `true` when the registry contains only the default rule set.
    pub fn is_default(&self) -> bool {
        self.len() == 1
    }
}

impl Default for RuleRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rule_set(version: u32) -> OfaRuleSet {
        OfaRuleSet {
            version,
            activated_at: Utc::now() - chrono::Duration::seconds(1),
            consent_max_age_secs: 3600,
            order_max_age_blocks: 10,
            backrun_slippage_cap_bps: 40,
        }
    }

    #[test]
    fn default_registry_has_active_rule_set() {
        let reg = RuleRegistry::new();
        assert!(reg.active_rule_set().is_ok());
        assert_eq!(reg.active_rule_set().unwrap().version, 1);
    }

    #[test]
    fn register_upgrades_active_version() {
        let reg = RuleRegistry::new();
        reg.register(make_rule_set(2)).unwrap();
        assert_eq!(reg.active_rule_set().unwrap().version, 2);
    }

    #[test]
    fn downgrade_rejected() {
        let reg = RuleRegistry::new();
        reg.register(make_rule_set(5)).unwrap();
        let err = reg.register(make_rule_set(3)).unwrap_err();
        assert!(matches!(
            err,
            RegistryError::VersionDowngrade { new: 3, active: 5 }
        ));
        // Active version unchanged
        assert_eq!(reg.active_rule_set().unwrap().version, 5);
    }

    #[test]
    fn same_version_downgrade_rejected() {
        let reg = RuleRegistry::new();
        let err = reg.register(make_rule_set(1)).unwrap_err();
        assert!(matches!(
            err,
            RegistryError::VersionDowngrade { new: 1, active: 1 }
        ));
    }

    #[test]
    fn future_activation_rejected() {
        let reg = RuleRegistry::new();
        let future_rule = OfaRuleSet {
            version: 99,
            activated_at: Utc::now() + chrono::Duration::hours(1),
            ..Default::default()
        };
        let err = reg.register(future_rule).unwrap_err();
        assert_eq!(err, RegistryError::FutureActivation);
    }

    #[test]
    fn history_newest_first() {
        let reg = RuleRegistry::new();
        reg.register(make_rule_set(2)).unwrap();
        reg.register(make_rule_set(3)).unwrap();
        let h = reg.history();
        assert_eq!(h[0].rule_set.version, 3);
        assert_eq!(h[1].rule_set.version, 2);
        assert_eq!(h[2].rule_set.version, 1);
    }

    #[test]
    fn multiple_upgrades_active_is_highest() {
        let reg = RuleRegistry::new();
        reg.register(make_rule_set(2)).unwrap();
        reg.register(make_rule_set(10)).unwrap();
        reg.register(make_rule_set(7)).unwrap_err(); // 7 < 10 → rejected
        assert_eq!(reg.active_rule_set().unwrap().version, 10);
    }
}
