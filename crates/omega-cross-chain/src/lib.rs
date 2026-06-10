// crates/omega-cross-chain/src/lib.rs
//
// omega-cross-chain — Cross-chain capability boundary for the Omega Engine.
//
// ## Status: EXPLICITLY DEFERRED (spec §11.5, §V2)
//
//   Cross-chain liquidation arbitrage requires an atomic bridge with
//   finality strictly below the 80ms LA execution window.  No production
//   atomic bridge meets this requirement as of v12 (April 2026).
//
//   This is NOT a gap — it is an intentional architectural boundary.
//   The spec §11.5 closure note states:
//
//     "When an atomic bridge with <50ms finality becomes production-
//      available (e.g., shared sequencer networks or native cross-chain
//      messaging with Ethereum-equivalent security), cross-chain LA
//      should be re-evaluated as a new phase."
//
// ## What this crate provides in v12
//
//   1. `AtomicBridgeRequirement` — the concrete latency and security
//      requirements a bridge must satisfy before activation.
//
//   2. `BridgeFinality` — a measured or claimed finality reading from a
//      specific bridge.  Used by `AtomicBridgeRequirement::is_satisfied`
//      to evaluate candidate bridges.
//
//   3. `CrossChainCapability` — the runtime capability descriptor.
//      Always returns `CrossChainCapability::deferred()` in v12.
//      Exposes `is_available()` → `false` so call sites can query
//      without hard-coding the deferral reason.
//
//   4. `DeferralRecord` — the structured audit record of why cross-chain
//      was deferred, matching spec §11.5.  Serialisable for the
//      control-plane API.
//
//   5. `ReEvaluationCriteria` — the checklist a future engineer must
//      satisfy to re-activate cross-chain.  Prevents re-activation
//      without a complete technical review.
//
// ## Dependency graph (§22.1)
//
//   omega-cross-chain ← omega-core, omega-relay
//
//   In v12 only omega-core is required.  omega-relay will be added
//   when a bridge client needs to submit cross-chain bundles.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use omega_core::ChainId;

// ─────────────────────────────────────────────────────────────────────────────
// Error type
// ─────────────────────────────────────────────────────────────────────────────

/// Error returned when cross-chain functionality is requested.
///
/// In v12 all cross-chain operations return this error.  The message
/// contains the spec section reference so operators can trace the
/// deferral decision.
///
/// ## Field naming note
///
/// thiserror treats a struct field literally named `source` as the error
/// cause and requires it to implement `std::error::Error`.  `ChainId` does
/// not implement `Error`, so the chain fields in `FinalityExceeded` are
/// named `source_chain` and `dest_chain` to avoid the implicit source
/// interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum CrossChainError {
    /// Capability is deferred — the bridge latency requirement is not met.
    #[error(
        "Cross-chain LA deferred (spec §11.5): requires atomic bridge with \
         finality < {required_ms}ms; best available is {available_ms}ms"
    )]
    Deferred { required_ms: u64, available_ms: u64 },

    /// A bridge was proposed but failed the finality requirement check.
    #[error(
        "Bridge '{bridge_name}' finality {actual_ms}ms exceeds limit \
         {required_ms}ms for chain pair {source_chain:?} → {dest_chain:?}"
    )]
    FinalityExceeded {
        bridge_name: String,
        actual_ms: u64,
        required_ms: u64,
        source_chain: ChainId,
        dest_chain: ChainId,
    },

    /// The source and destination chains are the same.
    #[error("Cross-chain requires distinct source and destination chains; both are {chain:?}")]
    SameChain { chain: ChainId },
}

// ─────────────────────────────────────────────────────────────────────────────
// AtomicBridgeRequirement
// ─────────────────────────────────────────────────────────────────────────────

/// The concrete technical requirements a bridge must satisfy before
/// cross-chain LA can be activated (spec §11.5).
///
/// All three criteria must be satisfied simultaneously.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomicBridgeRequirement {
    /// Maximum allowable bridge finality in milliseconds.
    ///
    /// Spec §11.5: "< 50ms" (conservative margin below the 80ms LA window).
    pub max_finality_ms: u64,

    /// Whether the bridge must offer Ethereum-equivalent economic security.
    pub ethereum_equivalent_security: bool,

    /// Whether the bridge must support atomic cross-chain message passing.
    pub requires_atomicity: bool,
}

impl AtomicBridgeRequirement {
    /// The v12 requirement set from spec §11.5.
    pub fn v12() -> Self {
        Self {
            max_finality_ms: 50,
            ethereum_equivalent_security: true,
            requires_atomicity: true,
        }
    }

    /// Evaluate whether a `BridgeFinality` reading satisfies this requirement.
    pub fn is_satisfied(
        &self,
        finality: &BridgeFinality,
        source: ChainId,
        destination: ChainId,
    ) -> Result<(), CrossChainError> {
        if source == destination {
            return Err(CrossChainError::SameChain { chain: source });
        }

        if finality.measured_ms > self.max_finality_ms {
            return Err(CrossChainError::FinalityExceeded {
                bridge_name: finality.bridge_name.clone(),
                actual_ms: finality.measured_ms,
                required_ms: self.max_finality_ms,
                source_chain: source,
                dest_chain: destination,
            });
        }

        if !finality.ethereum_equivalent_security && self.ethereum_equivalent_security {
            tracing::warn!(
                bridge = %finality.bridge_name,
                "Bridge does not claim Ethereum-equivalent security (§11.5)",
            );
        }
        if !finality.supports_atomicity && self.requires_atomicity {
            tracing::warn!(
                bridge = %finality.bridge_name,
                "Bridge does not support atomic cross-chain execution (§11.5)",
            );
        }

        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// BridgeFinality
// ─────────────────────────────────────────────────────────────────────────────

/// A measured or claimed finality reading from a specific bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeFinality {
    pub bridge_name: String,
    pub measured_ms: u64,
    pub ethereum_equivalent_security: bool,
    pub supports_atomicity: bool,
    pub measured_at: DateTime<Utc>,
    pub measurement_source: String,
}

impl BridgeFinality {
    /// Construct a `BridgeFinality` representing "no qualifying bridge exists".
    pub fn none_available() -> Self {
        Self {
            bridge_name: "none".to_string(),
            measured_ms: u64::MAX,
            ethereum_equivalent_security: false,
            supports_atomicity: false,
            measured_at: Utc::now(),
            measurement_source: "n/a".to_string(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DeferralRecord
// ─────────────────────────────────────────────────────────────────────────────

/// Structured audit record of the cross-chain deferral decision.
///
/// ## &'static str → String
///
/// The fields are `String` rather than `&'static str` so that
/// `#[derive(Deserialize)]` can satisfy the `'de: 'static` bound — serde's
/// `Deserialize` derive introduces a lifetime `'de` on the impl, and
/// `&'static str` fields require `'de: 'static` which cannot be guaranteed
/// for arbitrary deserializer inputs.  `String` owns its data and carries
/// no lifetime constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeferralRecord {
    pub spec_section: String,
    pub reason: String,
    pub re_activation_condition: String,
    pub candidate_technologies: Vec<String>,
    pub effective_since: String,
}

impl DeferralRecord {
    /// The canonical v12 deferral record (spec §11.5).
    pub fn v12() -> Self {
        Self {
            spec_section: "§11.5, §V2".into(),
            reason: "Atomic cross-chain liquidation requires a bridge with finality \
                     < 80ms (the LA execution window).  No production atomic bridge \
                     meets this requirement as of April 2026."
                .into(),
            re_activation_condition: "A production atomic bridge with < 50ms finality \
                     becomes available with Ethereum-equivalent security guarantees."
                .into(),
            candidate_technologies: vec![
                "shared sequencer networks (Espresso, Astria)".into(),
                "native cross-chain messaging (ICS-20 with Ethereum security)".into(),
                "zkBridge with sub-50ms proof generation".into(),
            ],
            effective_since: "2026-04-01".into(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ReEvaluationCriteria
// ─────────────────────────────────────────────────────────────────────────────

/// The checklist a future engineer must complete to re-activate cross-chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReEvaluationCriteria {
    pub items: Vec<ReEvaluationItem>,
}

/// One item in the re-evaluation checklist.
///
/// Fields are `String` for the same reason as `DeferralRecord` — serde's
/// `Deserialize` derive cannot satisfy `'de: 'static` required by `&'static str`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReEvaluationItem {
    pub id: String,
    pub description: String,
    pub completed: bool,
}

impl ReEvaluationCriteria {
    /// Returns the complete v12 re-evaluation checklist.
    ///
    /// All `completed` fields are `false` — they must be filled in by
    /// the engineer proposing re-activation.
    pub fn v12() -> Self {
        Self {
            items: vec![
                ReEvaluationItem {
                    id: "bridge_latency".into(),
                    description: "Bridge finality independently measured at < 50ms under \
                                  production mainnet load for ≥ 30 consecutive days."
                        .into(),
                    completed: false,
                },
                ReEvaluationItem {
                    id: "bridge_security".into(),
                    description: "Bridge economic security audited by Trail of Bits or \
                                  Spearbit; validator set bonding ≥ Ethereum mainnet magnitude."
                        .into(),
                    completed: false,
                },
                ReEvaluationItem {
                    id: "atomicity_proof".into(),
                    description: "Atomic execution (both-or-neither) formally verified \
                                  or audited for the specific bridge + chain pair."
                        .into(),
                    completed: false,
                },
                ReEvaluationItem {
                    id: "omega_rpc_stream".into(),
                    description: "omega-rpc gains a bridge subscription stream \
                                  (bridge finality events, proof confirmations)."
                        .into(),
                    completed: false,
                },
                ReEvaluationItem {
                    id: "strategy_variant".into(),
                    description: "omega-strategies gains a CrossChainLA strategy variant \
                                  that builds cross-chain blueprints and handles \
                                  double-spend protection across chains."
                        .into(),
                    completed: false,
                },
                ReEvaluationItem {
                    id: "sequencer_restart_guard".into(),
                    description: "SequencerRestartHandler extended to cover cross-chain \
                                  dedup: same position must not be submitted on both \
                                  source and destination chains simultaneously."
                        .into(),
                    completed: false,
                },
                ReEvaluationItem {
                    id: "governance_proposal".into(),
                    description: "L3 governance proposal (48h timelock) submitted with \
                                  all above items complete and bridge audit attached."
                        .into(),
                    completed: false,
                },
            ],
        }
    }

    /// Returns `true` only when all items are marked complete.
    pub fn all_complete(&self) -> bool {
        self.items.iter().all(|i| i.completed)
    }

    /// Returns the IDs of incomplete items.
    pub fn incomplete(&self) -> Vec<&str> {
        self.items
            .iter()
            .filter(|i| !i.completed)
            .map(|i| i.id.as_str())
            .collect()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CrossChainCapability
// ─────────────────────────────────────────────────────────────────────────────

/// Runtime cross-chain capability descriptor.
///
/// In v12 `is_available()` always returns `false`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossChainCapability {
    pub available: bool,
    pub requirement: AtomicBridgeRequirement,
    pub best_bridge: BridgeFinality,
    pub deferral: DeferralRecord,
    pub criteria: ReEvaluationCriteria,
}

impl CrossChainCapability {
    /// Construct the v12 deferred capability descriptor.
    pub fn deferred() -> Self {
        let req = AtomicBridgeRequirement::v12();
        let bridge = BridgeFinality::none_available();

        tracing::info!(
            required_ms = req.max_finality_ms,
            best_ms = bridge.measured_ms,
            spec_section = "§11.5",
            "Cross-chain LA deferred — no qualifying bridge available",
        );

        Self {
            available: false,
            requirement: req,
            best_bridge: bridge,
            deferral: DeferralRecord::v12(),
            criteria: ReEvaluationCriteria::v12(),
        }
    }

    #[inline]
    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Evaluate whether a candidate bridge satisfies the requirement.
    ///
    /// Returns `Ok(())` when the bridge qualifies.  Does NOT activate
    /// cross-chain — activation requires a governance proposal.
    pub fn evaluate_bridge(
        &self,
        bridge: &BridgeFinality,
        source: ChainId,
        destination: ChainId,
    ) -> Result<(), CrossChainError> {
        self.requirement.is_satisfied(bridge, source, destination)
    }

    /// Return the `CrossChainError::Deferred` error for call sites that
    /// attempt to use cross-chain when it is not available.
    pub fn unavailable_error(&self) -> CrossChainError {
        CrossChainError::Deferred {
            required_ms: self.requirement.max_finality_ms,
            available_ms: if self.best_bridge.measured_ms == u64::MAX {
                0
            } else {
                self.best_bridge.measured_ms
            },
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_is_not_available() {
        let cap = CrossChainCapability::deferred();
        assert!(!cap.is_available());
    }

    #[test]
    fn deferred_error_has_correct_required_ms() {
        let cap = CrossChainCapability::deferred();
        let err = cap.unavailable_error();
        match err {
            CrossChainError::Deferred { required_ms, .. } => {
                assert_eq!(required_ms, 50, "v12 requires < 50ms finality (§11.5)");
            }
            other => panic!("expected Deferred, got {other:?}"),
        }
    }

    #[test]
    fn v12_requirement_max_finality_50ms() {
        let req = AtomicBridgeRequirement::v12();
        assert_eq!(req.max_finality_ms, 50);
        assert!(req.ethereum_equivalent_security);
        assert!(req.requires_atomicity);
    }

    #[test]
    fn qualifying_bridge_passes_requirement() {
        let req = AtomicBridgeRequirement::v12();
        let bridge = BridgeFinality {
            bridge_name: "HyperBridge".into(),
            measured_ms: 30,
            ethereum_equivalent_security: true,
            supports_atomicity: true,
            measured_at: Utc::now(),
            measurement_source: "independent_audit".into(),
        };
        assert!(req
            .is_satisfied(&bridge, ChainId::Arbitrum, ChainId::Ethereum)
            .is_ok());
    }

    #[test]
    fn slow_bridge_fails_requirement() {
        let req = AtomicBridgeRequirement::v12();
        let bridge = BridgeFinality {
            bridge_name: "SlowBridge".into(),
            measured_ms: 200,
            ethereum_equivalent_security: true,
            supports_atomicity: true,
            measured_at: Utc::now(),
            measurement_source: "published_spec".into(),
        };
        let err = req
            .is_satisfied(&bridge, ChainId::Arbitrum, ChainId::Ethereum)
            .unwrap_err();
        assert!(matches!(
            err,
            CrossChainError::FinalityExceeded { actual_ms: 200, .. }
        ));
    }

    #[test]
    fn same_chain_is_rejected() {
        let req = AtomicBridgeRequirement::v12();
        let bridge = BridgeFinality {
            bridge_name: "Loopback".into(),
            measured_ms: 1,
            ethereum_equivalent_security: true,
            supports_atomicity: true,
            measured_at: Utc::now(),
            measurement_source: "published_spec".into(),
        };
        let err = req
            .is_satisfied(&bridge, ChainId::Arbitrum, ChainId::Arbitrum)
            .unwrap_err();
        assert!(matches!(
            err,
            CrossChainError::SameChain {
                chain: ChainId::Arbitrum
            }
        ));
    }

    #[test]
    fn v12_criteria_are_all_incomplete() {
        let criteria = ReEvaluationCriteria::v12();
        assert!(!criteria.all_complete());
        assert_eq!(criteria.incomplete().len(), criteria.items.len());
    }

    #[test]
    fn criteria_item_count_matches_spec() {
        let criteria = ReEvaluationCriteria::v12();
        assert_eq!(
            criteria.items.len(),
            7,
            "must have exactly 7 re-evaluation items"
        );
    }

    #[test]
    fn all_complete_only_when_all_marked() {
        let mut criteria = ReEvaluationCriteria::v12();
        for item in criteria.items.iter_mut() {
            item.completed = true;
        }
        assert!(criteria.all_complete());
        assert!(criteria.incomplete().is_empty());
    }

    #[test]
    fn deferral_record_references_correct_spec_section() {
        let rec = DeferralRecord::v12();
        assert!(rec.spec_section.contains("§11.5"), "must cite §11.5");
        assert!(rec.spec_section.contains("§V2"), "must cite §V2");
    }

    #[test]
    fn deferral_is_serialisable() {
        let cap = CrossChainCapability::deferred();
        let json = serde_json::to_string(&cap).expect("serialisation must not fail");
        assert!(json.contains("available"));
        assert!(json.contains("false"));
    }

    #[test]
    fn capability_evaluate_bridge_delegates_correctly() {
        let cap = CrossChainCapability::deferred();
        let fast = BridgeFinality {
            bridge_name: "Fast".into(),
            measured_ms: 40,
            ethereum_equivalent_security: true,
            supports_atomicity: true,
            measured_at: Utc::now(),
            measurement_source: "audit".into(),
        };
        assert!(cap
            .evaluate_bridge(&fast, ChainId::Arbitrum, ChainId::Ethereum)
            .is_ok());
    }
}
