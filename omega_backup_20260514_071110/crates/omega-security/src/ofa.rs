// crates/omega-security/src/ofa.rs
//
// Versioned OFA (Order Flow Auction) rule registry + compliance check (spec S8).
//
// Spec references:
//   Section 8  â€” "Security + OFA compliance + versioned rules"
//   config/ofa_rules.toml â€” rule schema (version, effective_date, [[rules]])
//   omega-compliance crate â€” this module provides the rule data; compliance
//                            orchestration sits in omega-compliance.
//
// OFA rule types (from spec ofa_rules.toml):
//   RequireConsentSig    â€” user must sign a consent message before their tx
//                          is backrun.  Blueprint must carry the sig.
//   EnforceUserSlippage  â€” omega's extractable value from the user tx must not
//                          exceed max_excess_bps basis points above their stated
//                          slippage tolerance.
//   EnforceBundleOrder   â€” user tx must appear BEFORE omega tx in the bundle.
//                          Prevents front-running in the same bundle.
//   PrivateRelayOnly     â€” bundles must ONLY be sent to the allowed private
//                          relays.  Public/blind relay submission is forbidden.
//
// Rule-set lifecycle:
//   1. Engineering team edits config/ofa_rules.toml and bumps `version`.
//   2. L2 fast-approve (5-minute governance) required for OFA rule changes.
//   3. Control plane calls OfaRuleRegistry::load_rules() with the new rule set.
//   4. The registry hot-swaps the rule set (ArcSwap â€” zero-downtime).
//   5. All subsequent compliance checks use the new rules immediately.
//
// Thread-safety:
//   ArcSwap<OfaRuleSet> provides lock-free reads; rule updates are single-writer.
//   OfaComplianceInput and the check functions are pure / allocation-free on the
//   hot path when all rules pass.

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

use crate::error::SecurityError;
use crate::metrics;

// â”€â”€â”€ Rule definitions â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// A single OFA compliance rule, matching the spec ofa_rules.toml schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OfaRule {
    /// User must provide a consent signature before their tx is backrun.
    RequireConsentSig {
        /// ABI schema version for the consent message (must match).
        schema_version: u32,
    },

    /// Omega's excess extraction from the user tx must be within `max_excess_bps`.
    EnforceUserSlippage {
        max_excess_bps: u16,
    },

    /// User transaction must precede omega transaction in the bundle.
    EnforceBundleOrder {
        user_tx_before_omega: bool,
    },

    /// Bundle must only be submitted to listed private relays.
    PrivateRelayOnly {
        allowed_relays: Vec<String>,
    },
}

impl OfaRule {
    /// Human-readable rule name for metrics labels.
    pub fn label(&self) -> &'static str {
        match self {
            OfaRule::RequireConsentSig { .. }   => "require_consent_sig",
            OfaRule::EnforceUserSlippage { .. } => "enforce_user_slippage",
            OfaRule::EnforceBundleOrder { .. }  => "enforce_bundle_order",
            OfaRule::PrivateRelayOnly { .. }    => "private_relay_only",
        }
    }
}

/// A versioned, dated collection of OFA rules.
/// Loaded from config/ofa_rules.toml and hot-swapped on L2 approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfaRuleSet {
    /// Monotonically increasing version number. Starts at 1.
    pub version:        u32,
    /// ISO-8601 effective date string (informational, not enforced in code).
    pub effective_date: String,
    /// Ordered list of rules â€” all rules are checked; first failure is returned.
    pub rules:          Vec<OfaRule>,
}

impl OfaRuleSet {
    /// True if this rule set contains a `PrivateRelayOnly` rule.
    pub fn has_relay_restriction(&self) -> bool {
        self.rules.iter().any(|r| matches!(r, OfaRule::PrivateRelayOnly { .. }))
    }

    /// Collect the set of allowed relay names (union across all PrivateRelayOnly rules).
    pub fn allowed_relays(&self) -> HashSet<&str> {
        self.rules
            .iter()
            .filter_map(|r| {
                if let OfaRule::PrivateRelayOnly { allowed_relays } = r {
                    Some(allowed_relays.iter().map(|s| s.as_str()))
                } else {
                    None
                }
            })
            .flatten()
            .collect()
    }
}

// â”€â”€â”€ Compliance input â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// All fields from the blueprint and submission context needed to evaluate OFA rules.
/// Constructed by omega-strategies before blueprint submission.
#[derive(Debug, Clone)]
pub struct OfaComplianceInput {
    /// Hex-encoded blueprint hash (for error messages).
    pub blueprint_hash: String,
    /// Strategy that produced this blueprint.
    pub strategy_id: String,
    /// True if the blueprint carries a valid user consent signature.
    pub has_consent_sig: bool,
    /// Schema version of the consent signature (must match RequireConsentSig.schema_version).
    pub consent_schema_version: u32,
    /// Excess slippage the blueprint would extract from the user tx, in basis points.
    pub excess_slippage_bps: u16,
    /// True if the user transaction appears before the omega transaction in the bundle.
    pub user_tx_is_first: bool,
    /// Name of the relay this bundle will be submitted to.
    pub target_relay: String,
}

// â”€â”€â”€ Compliance result â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Outcome of an OFA compliance check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfaComplianceResult {
    /// All rules passed.
    Compliant,
    /// One or more rules failed.
    Violation(SecurityError),
}

impl OfaComplianceResult {
    pub fn is_compliant(&self) -> bool {
        matches!(self, OfaComplianceResult::Compliant)
    }

    /// Convert to Result for use in early-return chains.
    pub fn into_result(self) -> Result<(), SecurityError> {
        match self {
            OfaComplianceResult::Compliant      => Ok(()),
            OfaComplianceResult::Violation(err) => Err(err),
        }
    }
}

// â”€â”€â”€ Registry â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Hot-swappable OFA rule registry.
///
/// `Arc<OfaRuleRegistry>` is shared across all strategy tasks and the
/// control-plane handler.  Rule updates are zero-downtime (ArcSwap store).
pub struct OfaRuleRegistry {
    /// Current rule set (None until load_rules() is called).
    current: ArcSwap<Option<OfaRuleSet>>,
}

impl OfaRuleRegistry {
    /// Create an empty registry. Must call `load_rules()` before compliance checks.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            current: ArcSwap::new(Arc::new(None)),
        })
    }

    /// Create a registry pre-loaded with the default production rule set.
    pub fn with_default_rules() -> Arc<Self> {
        let registry = Self::new();
        registry.load_rules(default_rule_set());
        registry
    }

    /// Hot-swap the rule set (called after L2 fast-approve).
    ///
    /// The new rules take effect on the next call to `check()`.
    pub fn load_rules(&self, rule_set: OfaRuleSet) {
        let version = rule_set.version;
        let rule_count = rule_set.rules.len();
        self.current.store(Arc::new(Some(rule_set)));
        tracing::info!(version, rule_count, "OFA rule set loaded");
        // Update version gauge for all strategies (generic label).
        metrics::OFA_RULE_VERSION
            .with_label_values(&["*"])
            .set(version as f64);
    }

    /// Return the currently loaded rule set version, or None if not loaded.
    pub fn current_version(&self) -> Option<u32> {
        self.current.load().as_ref().as_ref().map(|rs| rs.version)
    }

    /// Evaluate all OFA rules against `input`.
    ///
    /// Rules are checked in declaration order (spec: "all rules enforced").
    /// The first failing rule short-circuits and returns its violation.
    ///
    /// Non-MEV strategies (SA, MSA, LA operating without user order flow) do not
    /// require OFA compliance â€” callers should gate on `blueprint.ofa_compliant`.
    pub fn check(&self, input: &OfaComplianceInput) -> OfaComplianceResult {
        let guard = self.current.load();
        let rule_set = match guard.as_ref().as_ref() {
            Some(rs) => rs,
            None => {
                // Rules not loaded â€” this is a configuration error, not a violation.
                // Log and pass through so the system doesn't halt on startup ordering.
                tracing::warn!(
                    blueprint = %input.blueprint_hash,
                    "OFA rules not loaded â€” compliance check skipped"
                );
                return OfaComplianceResult::Compliant;
            }
        };

        for rule in &rule_set.rules {
            if let Some(err) = evaluate_rule(rule, input) {
                let label = match &err {
                    SecurityError::MissingConsentSig { .. }  => "missing_consent",
                    SecurityError::SlippageExceeded { .. }   => "slippage",
                    SecurityError::BundleOrderViolation      => "order",
                    SecurityError::NonPrivateRelay { .. }    => "relay",
                    _                                        => "other",
                };
                metrics::OFA_CHECKS
                    .with_label_values(&[&input.strategy_id, label])
                    .inc();
                tracing::warn!(
                    blueprint = %input.blueprint_hash,
                    strategy  = %input.strategy_id,
                    rule      = rule.label(),
                    "OFA compliance violation"
                );
                return OfaComplianceResult::Violation(err);
            }
        }

        metrics::OFA_CHECKS
            .with_label_values(&[&input.strategy_id, "pass"])
            .inc();
        OfaComplianceResult::Compliant
    }
}

impl Default for OfaRuleRegistry {
    fn default() -> Self {
        Self { current: ArcSwap::new(Arc::new(None)) }
    }
}

// â”€â”€â”€ Rule evaluation â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Evaluate a single rule against the input. Returns None if the rule passes.
fn evaluate_rule(rule: &OfaRule, input: &OfaComplianceInput) -> Option<SecurityError> {
    match rule {
        OfaRule::RequireConsentSig { schema_version } => {
            if !input.has_consent_sig {
                return Some(SecurityError::MissingConsentSig {
                    blueprint_hash: input.blueprint_hash.clone(),
                });
            }
            if input.consent_schema_version != *schema_version {
                return Some(SecurityError::RuleVersionMismatch {
                    expected: *schema_version,
                    got:      input.consent_schema_version,
                });
            }
            None
        }

        OfaRule::EnforceUserSlippage { max_excess_bps } => {
            if input.excess_slippage_bps > *max_excess_bps {
                return Some(SecurityError::SlippageExceeded {
                    excess_bps: input.excess_slippage_bps,
                    max_bps:    *max_excess_bps,
                });
            }
            None
        }

        OfaRule::EnforceBundleOrder { user_tx_before_omega } => {
            if *user_tx_before_omega && !input.user_tx_is_first {
                return Some(SecurityError::BundleOrderViolation);
            }
            None
        }

        OfaRule::PrivateRelayOnly { allowed_relays } => {
            if !allowed_relays.contains(&input.target_relay) {
                return Some(SecurityError::NonPrivateRelay {
                    relay: input.target_relay.clone(),
                });
            }
            None
        }
    }
}

// â”€â”€â”€ Default production rule set â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Default OFA rule set matching config/ofa_rules.toml version 1.
pub fn default_rule_set() -> OfaRuleSet {
    OfaRuleSet {
        version:        1,
        effective_date: "2026-04-19".into(),
        rules: vec![
            OfaRule::RequireConsentSig { schema_version: 1 },
            OfaRule::EnforceUserSlippage { max_excess_bps: 50 },
            OfaRule::EnforceBundleOrder { user_tx_before_omega: true },
            OfaRule::PrivateRelayOnly {
                allowed_relays: vec![
                    "flashbots".into(),
                    "bloxroute".into(),
                    "titan".into(),
                    "eden".into(),
                ],
            },
        ],
    }
}

// â”€â”€â”€ Helper: compliant input builder â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

impl OfaComplianceInput {
    /// Build a fully-compliant input for testing or non-MEV blueprints.
    pub fn compliant(blueprint_hash: &str, strategy_id: &str) -> Self {
        Self {
            blueprint_hash:         blueprint_hash.to_string(),
            strategy_id:            strategy_id.to_string(),
            has_consent_sig:        true,
            consent_schema_version: 1,
            excess_slippage_bps:    0,
            user_tx_is_first:       true,
            target_relay:           "flashbots".into(),
        }
    }
}

#[cfg(test)]
mod ofa_tests {
    use super::*;

    fn registry_with_defaults() -> Arc<OfaRuleRegistry> {
        OfaRuleRegistry::with_default_rules()
    }

    fn compliant_input() -> OfaComplianceInput {
        OfaComplianceInput::compliant("0xdeadbeef", "MEV")
    }

    // â”€â”€ Full compliance â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn fully_compliant_input_passes() {
        let reg = registry_with_defaults();
        assert!(reg.check(&compliant_input()).is_compliant());
    }

    // â”€â”€ RequireConsentSig â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn missing_consent_sig_fails() {
        let reg = registry_with_defaults();
        let mut input = compliant_input();
        input.has_consent_sig = false;
        assert!(!reg.check(&input).is_compliant());
        assert!(matches!(
            reg.check(&input),
            OfaComplianceResult::Violation(SecurityError::MissingConsentSig { .. })
        ));
    }

    #[test]
    fn wrong_consent_schema_version_fails() {
        let reg = registry_with_defaults();
        let mut input = compliant_input();
        input.consent_schema_version = 99;
        assert!(matches!(
            reg.check(&input),
            OfaComplianceResult::Violation(SecurityError::RuleVersionMismatch { .. })
        ));
    }

    // â”€â”€ EnforceUserSlippage â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn excess_slippage_above_50bps_fails() {
        let reg = registry_with_defaults();
        let mut input = compliant_input();
        input.excess_slippage_bps = 51;
        assert!(matches!(
            reg.check(&input),
            OfaComplianceResult::Violation(SecurityError::SlippageExceeded { .. })
        ));
    }

    #[test]
    fn excess_slippage_exactly_50bps_passes() {
        let reg = registry_with_defaults();
        let mut input = compliant_input();
        input.excess_slippage_bps = 50;
        assert!(reg.check(&input).is_compliant());
    }

    // â”€â”€ EnforceBundleOrder â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn omega_tx_before_user_tx_fails() {
        let reg = registry_with_defaults();
        let mut input = compliant_input();
        input.user_tx_is_first = false;
        assert!(matches!(
            reg.check(&input),
            OfaComplianceResult::Violation(SecurityError::BundleOrderViolation)
        ));
    }

    // â”€â”€ PrivateRelayOnly â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn non_private_relay_fails() {
        let reg = registry_with_defaults();
        let mut input = compliant_input();
        input.target_relay = "public_mempool".into();
        assert!(matches!(
            reg.check(&input),
            OfaComplianceResult::Violation(SecurityError::NonPrivateRelay { .. })
        ));
    }

    #[test]
    fn allowed_relay_passes() {
        let reg = registry_with_defaults();
        for relay in ["flashbots", "bloxroute", "titan", "eden"] {
            let mut input = compliant_input();
            input.target_relay = relay.into();
            assert!(reg.check(&input).is_compliant(), "relay {} should be allowed", relay);
        }
    }

    // â”€â”€ Rule set versioning â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn hot_swap_rule_set_version_updates() {
        let reg = OfaRuleRegistry::new();
        assert_eq!(reg.current_version(), None);

        reg.load_rules(default_rule_set());
        assert_eq!(reg.current_version(), Some(1));

        let mut v2 = default_rule_set();
        v2.version = 2;
        reg.load_rules(v2);
        assert_eq!(reg.current_version(), Some(2));
    }

    #[test]
    fn unloaded_registry_passes_with_warning() {
        // Rules not loaded â†’ check passes (logged as warning, not error)
        let reg = OfaRuleRegistry::new();
        assert!(reg.check(&compliant_input()).is_compliant());
    }

    #[test]
    fn empty_rule_set_always_passes() {
        let reg = OfaRuleRegistry::new();
        reg.load_rules(OfaRuleSet {
            version:        99,
            effective_date: "2099-01-01".into(),
            rules:          vec![],
        });
        // No rules â†’ always compliant
        let mut bad_input = compliant_input();
        bad_input.has_consent_sig = false;
        bad_input.excess_slippage_bps = 9999;
        bad_input.user_tx_is_first = false;
        bad_input.target_relay = "public_mempool".into();
        assert!(reg.check(&bad_input).is_compliant());
    }

    // â”€â”€ allowed_relays helper â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn allowed_relays_returns_correct_set() {
        let rs = default_rule_set();
        let relays = rs.allowed_relays();
        assert!(relays.contains("flashbots"));
        assert!(relays.contains("bloxroute"));
        assert!(relays.contains("titan"));
        assert!(relays.contains("eden"));
        assert!(!relays.contains("public_mempool"));
    }

    // â”€â”€ Fast-fail: first violation returned â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn first_violation_is_returned_not_last() {
        // consent sig (rule 1) fails AND slippage (rule 2) fails.
        // Rule 1 (consent) should be returned since it comes first.
        let reg = registry_with_defaults();
        let mut input = compliant_input();
        input.has_consent_sig    = false;
        input.excess_slippage_bps = 9999;
        assert!(matches!(
            reg.check(&input),
            OfaComplianceResult::Violation(SecurityError::MissingConsentSig { .. })
        ));
    }

    // â”€â”€ Custom rule set â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn custom_slippage_threshold_respected() {
        let reg = OfaRuleRegistry::new();
        reg.load_rules(OfaRuleSet {
            version:        5,
            effective_date: "2026-01-01".into(),
            rules:          vec![OfaRule::EnforceUserSlippage { max_excess_bps: 10 }],
        });
        let mut input = compliant_input();
        input.excess_slippage_bps = 11;
        assert!(!reg.check(&input).is_compliant());
        input.excess_slippage_bps = 10;
        assert!(reg.check(&input).is_compliant());
    }
}