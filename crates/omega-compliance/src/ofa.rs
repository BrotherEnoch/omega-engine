// crates/omega-compliance/src/ofa.rs
//
// Order Flow Agreement (OFA) compliance validation (spec §8).
//
// ## What OFA is
//
//   OFA is a user-protection mechanism: when a user submits a swap
//   through an OFA-compliant relay (e.g. MEV-Share), they consent to
//   searchers backrunning their transaction in exchange for a portion
//   of the extracted value.  The OFA contract specifies:
//     - Consent: the user has opted into this order flow program
//     - Slippage: the backrun must not worsen the user's slippage
//     - Order validity: the order must not be expired or malformed
//
// ## Compliance obligations (spec §8)
//
//   Every blueprint with `ofa_compliant = true` MUST pass all three
//   checks before relay submission:
//
//     1. ConsentCheck   — user has a valid, unexpired OFA consent record
//     2. SlippageCheck  — `price_impact_bps ≤ consent.max_slippage_bps`
//     3. OrderCheck     — order is well-formed and within validity window
//
//   A blueprint that fails any check is discarded with the corresponding
//   DropCode and is NOT submitted to any relay.
//
// ## Versioned rule sets (spec §8)
//
//   OFA rules are versioned.  Each `OfaRuleSet` has an activation
//   timestamp; the compliance checker uses the most recently activated
//   rule set that is ≤ the current time.  Rule set updates use the L2
//   fast-approve governance path (§5).  Downgrades are blocked — the
//   active version can only increase.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use omega_core::errors::DropCode;
use omega_core::types::blueprint::ExecutionBlueprint;

// ─────────────────────────────────────────────────────────────────────────────
// OfaConsentRecord
// ─────────────────────────────────────────────────────────────────────────────

/// A user's consent record for OFA participation.
///
/// Stored by the OFA relay and supplied to the compliance checker.
/// The relay is responsible for fetching and verifying the on-chain
/// consent signature; omega-compliance trusts the provided record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfaConsentRecord {
    /// User's wallet address (hex-encoded).
    pub user: String,
    /// Maximum slippage the user accepts in basis points.
    pub max_slippage_bps: u16,
    /// When this consent record expires.
    pub expires_at: DateTime<Utc>,
    /// OFA program identifier (e.g. "mev_share_v1", "flashbots_ofa_v2").
    pub program_id: String,
    /// Whether this consent is still active (not revoked).
    pub is_active: bool,
}

impl OfaConsentRecord {
    /// Returns `true` when the consent is valid at the given instant.
    ///
    /// A consent is valid when:
    ///   - `is_active` is true (not revoked)
    ///   - `expires_at` is in the future
    pub fn is_valid_at(&self, now: DateTime<Utc>) -> bool {
        self.is_active && self.expires_at > now
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OfaOrder
// ─────────────────────────────────────────────────────────────────────────────

/// A parsed OFA order from the MEV-Share SSE stream.
///
/// The order is emitted by the relay after the user's transaction is
/// included and describes the backrun opportunity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfaOrder {
    /// Unique order identifier from the relay.
    pub order_id: String,
    /// Block number at which this order was created.
    pub created_at: u64,
    /// Block number after which this order expires.
    pub expires_at: u64,
    /// Maximum slippage the backrun may impose, in basis points.
    pub max_slippage_bps: u16,
    /// Whether the order has been filled by another searcher.
    pub is_filled: bool,
}

impl OfaOrder {
    /// Returns `true` when the order is valid at `current_block`.
    ///
    /// An order is valid when:
    ///   - It has not been filled
    ///   - `current_block ≤ expires_at`
    pub fn is_valid_at_block(&self, current_block: u64) -> bool {
        !self.is_filled && current_block <= self.expires_at
    }

    /// Returns `true` when the order is well-formed.
    ///
    /// An order is malformed when:
    ///   - `order_id` is empty
    ///   - `expires_at < created_at` (impossible validity window)
    ///   - `max_slippage_bps > 10_000` (>100% slippage is nonsensical)
    pub fn is_well_formed(&self) -> bool {
        !self.order_id.is_empty()
            && self.expires_at >= self.created_at
            && self.max_slippage_bps <= 10_000
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OfaRuleSet
// ─────────────────────────────────────────────────────────────────────────────

/// A versioned OFA compliance rule set (spec §8).
///
/// Rule set updates are applied via L2 fast-approve governance.
/// The active rule set is the most recently activated one
/// (activated_at ≤ Utc::now()).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfaRuleSet {
    /// Monotonically increasing version number.
    pub version: u32,
    /// When this rule set became active (UTC).
    pub activated_at: DateTime<Utc>,
    /// Maximum age of a consent record in seconds before it is
    /// considered stale (default 86400 = 24 hours).
    pub consent_max_age_secs: u64,
    /// Maximum order age in blocks (default 20 blocks ≈ 5s on Arbitrum).
    pub order_max_age_blocks: u64,
    /// Maximum slippage imposed by backrun, in basis points (default 50).
    pub backrun_slippage_cap_bps: u16,
}

impl Default for OfaRuleSet {
    fn default() -> Self {
        Self {
            version: 1,
            activated_at: DateTime::UNIX_EPOCH,
            consent_max_age_secs: 86_400,
            order_max_age_blocks: 20,
            backrun_slippage_cap_bps: 50,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OfaCheckError
// ─────────────────────────────────────────────────────────────────────────────

/// A typed OFA compliance failure.
///
/// Each variant maps to exactly one `DropCode` for the Loss Attribution
/// Engine.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OfaCheckError {
    #[error("OFA consent missing or expired for user {user}")]
    ConsentMissingOrExpired { user: String },

    #[error("OFA consent revoked for user {user}")]
    ConsentRevoked { user: String },

    #[error("Blueprint price_impact_bps {impact} exceeds consent max_slippage_bps {max}")]
    SlippageExceedsConsent { impact: u16, max: u16 },

    #[error("Blueprint price_impact_bps {impact} exceeds rule set backrun cap {cap}")]
    SlippageExceedsRuleCap { impact: u16, cap: u16 },

    #[error("OFA order is malformed: order_id={order_id}")]
    OrderMalformed { order_id: String },

    #[error("OFA order has expired at block {current} (expires_at={expires})")]
    OrderExpired { current: u64, expires: u64 },

    #[error("OFA order is already filled: order_id={order_id}")]
    OrderAlreadyFilled { order_id: String },
}

impl OfaCheckError {
    /// Maps this error to the `DropCode` used in the Loss Attribution Engine.
    pub fn drop_code(&self) -> DropCode {
        match self {
            OfaCheckError::ConsentMissingOrExpired { .. }
            | OfaCheckError::ConsentRevoked { .. } => DropCode::MissOfaConsent,

            OfaCheckError::SlippageExceedsConsent { .. }
            | OfaCheckError::SlippageExceedsRuleCap { .. } => DropCode::MissOfaSlippage,

            OfaCheckError::OrderMalformed { .. }
            | OfaCheckError::OrderExpired { .. }
            | OfaCheckError::OrderAlreadyFilled { .. } => DropCode::MissOfaOrder,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OfaChecker
// ─────────────────────────────────────────────────────────────────────────────

/// Stateless OFA compliance checker (spec §8).
///
/// All methods are pure functions — no I/O, no async.
pub struct OfaChecker;

impl OfaChecker {
    /// Validate OFA consent for a blueprint.
    ///
    /// Returns `Ok(())` when the consent is valid, active, and unexpired.
    /// Returns `Err(OfaCheckError)` when any condition fails.
    pub fn check_consent(
        consent: &OfaConsentRecord,
        rules: &OfaRuleSet,
        now: DateTime<Utc>,
    ) -> Result<(), OfaCheckError> {
        if !consent.is_active {
            return Err(OfaCheckError::ConsentRevoked {
                user: consent.user.clone(),
            });
        }
        if !consent.is_valid_at(now) {
            return Err(OfaCheckError::ConsentMissingOrExpired {
                user: consent.user.clone(),
            });
        }
        // Check consent age against rule set max
        let age_secs = now
            .signed_duration_since(consent.expires_at - chrono::Duration::days(1))
            .num_seconds()
            .max(0) as u64;
        if age_secs > rules.consent_max_age_secs {
            return Err(OfaCheckError::ConsentMissingOrExpired {
                user: consent.user.clone(),
            });
        }
        Ok(())
    }

    /// Validate OFA slippage constraints for a blueprint.
    ///
    /// Returns `Ok(())` when `price_impact_bps` does not exceed either
    /// the user's consent slippage cap or the rule set backrun cap.
    pub fn check_slippage(
        bp: &ExecutionBlueprint,
        consent: &OfaConsentRecord,
        rules: &OfaRuleSet,
    ) -> Result<(), OfaCheckError> {
        let impact = bp.price_impact_bps.unwrap_or(0);

        if impact > consent.max_slippage_bps {
            return Err(OfaCheckError::SlippageExceedsConsent {
                impact,
                max: consent.max_slippage_bps,
            });
        }

        if impact > rules.backrun_slippage_cap_bps {
            return Err(OfaCheckError::SlippageExceedsRuleCap {
                impact,
                cap: rules.backrun_slippage_cap_bps,
            });
        }

        Ok(())
    }

    /// Validate the OFA order at the current block.
    ///
    /// Returns `Ok(())` when the order is well-formed, unfilled, and
    /// within its validity window.
    pub fn check_order(order: &OfaOrder, current_block: u64) -> Result<(), OfaCheckError> {
        if !order.is_well_formed() {
            return Err(OfaCheckError::OrderMalformed {
                order_id: order.order_id.clone(),
            });
        }
        if order.is_filled {
            return Err(OfaCheckError::OrderAlreadyFilled {
                order_id: order.order_id.clone(),
            });
        }
        if !order.is_valid_at_block(current_block) {
            return Err(OfaCheckError::OrderExpired {
                current: current_block,
                expires: order.expires_at,
            });
        }
        Ok(())
    }

    /// Run all three OFA checks for an `ofa_compliant = true` blueprint.
    ///
    /// Short-circuits on the first failure.  The caller receives the
    /// `DropCode` to record in the loss attribution pipeline.
    pub fn validate_blueprint(
        bp: &ExecutionBlueprint,
        consent: &OfaConsentRecord,
        order: &OfaOrder,
        rules: &OfaRuleSet,
        now: DateTime<Utc>,
        current_block: u64,
    ) -> Result<(), OfaCheckError> {
        Self::check_consent(consent, rules, now)?;
        Self::check_slippage(bp, consent, rules)?;
        Self::check_order(order, current_block)?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, Bytes, B256, U256};
    use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
    use omega_core::types::lane::{Lane, Simulator};

    fn consent(active: bool, max_slippage_bps: u16, expires_minutes: i64) -> OfaConsentRecord {
        OfaConsentRecord {
            user: "0xUser".into(),
            max_slippage_bps,
            expires_at: Utc::now() + chrono::Duration::minutes(expires_minutes),
            program_id: "mev_share_v1".into(),
            is_active: active,
        }
    }

    fn order(filled: bool, created: u64, expires: u64) -> OfaOrder {
        OfaOrder {
            order_id: "ord_001".into(),
            created_at: created,
            expires_at: expires,
            max_slippage_bps: 50,
            is_filled: filled,
        }
    }

    fn rules() -> OfaRuleSet {
        OfaRuleSet::default()
    }

    fn dummy_bp(price_impact_bps: Option<u16>) -> ExecutionBlueprint {
        ExecutionBlueprint {
            blueprint_hash: B256::ZERO,
            chain_id: 42161,
            strategy_id: StrategyId::Mev,
            lane: Lane::Normal,
            simulator: Simulator::Anvil,
            signal_state_hash: B256::ZERO,
            state_version: 1,
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::ZERO,
            flashloan_available: U256::ZERO,
            calldata: Bytes::new(),
            strategy_bytecode_hash: B256::ZERO,
            l2_exec_gas_estimate: 200_000,
            l1_data_gas_estimate: 4_000,
            extraction_gas: 21_000,
            expected_profit_net: U256::from(1_000_000_000_000_000_u128),
            dynamic_min_profit: U256::from(500_000_000_000_000_u128),
            l2_buffer_factor: 1.15,
            l1_data_buffer_factor: 1.10,
            slippage_bps: 30,
            base_fee_at_creation: 10,
            l1_data_fee_at_creation: 2,
            priority_fee_gwei: 100,
            price_impact_bps,
            ofa_compliant: true,
            expiry_block: 1_000_001,
            nonce: 1,
            confirmation_depth: 12,
            relay_targets: vec!["mev_share".into()],
            zk_proof_commitment: None,
        }
    }

    // ── ConsentCheck ─────────────────────────────────────────────────────────

    #[test]
    fn consent_valid() {
        let c = consent(true, 100, 60);
        assert!(OfaChecker::check_consent(&c, &rules(), Utc::now()).is_ok());
    }

    #[test]
    fn consent_revoked() {
        let c = consent(false, 100, 60);
        let err = OfaChecker::check_consent(&c, &rules(), Utc::now()).unwrap_err();
        assert_eq!(err.drop_code(), DropCode::MissOfaConsent);
        assert!(matches!(err, OfaCheckError::ConsentRevoked { .. }));
    }

    #[test]
    fn consent_expired() {
        let c = consent(true, 100, -1); // expired 1 minute ago
        let err = OfaChecker::check_consent(&c, &rules(), Utc::now()).unwrap_err();
        assert_eq!(err.drop_code(), DropCode::MissOfaConsent);
        assert!(matches!(err, OfaCheckError::ConsentMissingOrExpired { .. }));
    }

    // ── SlippageCheck ─────────────────────────────────────────────────────────

    #[test]
    fn slippage_within_limits() {
        let bp = dummy_bp(Some(30));
        let c = consent(true, 100, 60);
        assert!(OfaChecker::check_slippage(&bp, &c, &rules()).is_ok());
    }

    #[test]
    fn slippage_exceeds_consent() {
        let bp = dummy_bp(Some(150));
        let c = consent(true, 100, 60); // user cap = 100 bps
        let err = OfaChecker::check_slippage(&bp, &c, &rules()).unwrap_err();
        assert_eq!(err.drop_code(), DropCode::MissOfaSlippage);
        assert!(matches!(
            err,
            OfaCheckError::SlippageExceedsConsent {
                impact: 150,
                max: 100
            }
        ));
    }

    #[test]
    fn slippage_exceeds_rule_cap() {
        // Rule cap is 50 bps; user allows 200 bps
        let bp = dummy_bp(Some(80));
        let c = consent(true, 200, 60);
        let err = OfaChecker::check_slippage(&bp, &c, &rules()).unwrap_err();
        assert_eq!(err.drop_code(), DropCode::MissOfaSlippage);
        assert!(matches!(
            err,
            OfaCheckError::SlippageExceedsRuleCap {
                impact: 80,
                cap: 50
            }
        ));
    }

    #[test]
    fn no_price_impact_passes_slippage() {
        let bp = dummy_bp(None); // no AMM swaps
        let c = consent(true, 100, 60);
        assert!(OfaChecker::check_slippage(&bp, &c, &rules()).is_ok());
    }

    // ── OrderCheck ───────────────────────────────────────────────────────────

    #[test]
    fn order_valid() {
        let o = order(false, 1000, 1020);
        assert!(OfaChecker::check_order(&o, 1010).is_ok());
    }

    #[test]
    fn order_expired() {
        let o = order(false, 1000, 1005);
        let err = OfaChecker::check_order(&o, 1010).unwrap_err(); // current > expires
        assert_eq!(err.drop_code(), DropCode::MissOfaOrder);
        assert!(matches!(err, OfaCheckError::OrderExpired { .. }));
    }

    #[test]
    fn order_already_filled() {
        let o = order(true, 1000, 1020);
        let err = OfaChecker::check_order(&o, 1010).unwrap_err();
        assert_eq!(err.drop_code(), DropCode::MissOfaOrder);
        assert!(matches!(err, OfaCheckError::OrderAlreadyFilled { .. }));
    }

    #[test]
    fn order_malformed_empty_id() {
        let o = OfaOrder {
            order_id: String::new(),
            created_at: 1000,
            expires_at: 1020,
            max_slippage_bps: 50,
            is_filled: false,
        };
        let err = OfaChecker::check_order(&o, 1010).unwrap_err();
        assert_eq!(err.drop_code(), DropCode::MissOfaOrder);
        assert!(matches!(err, OfaCheckError::OrderMalformed { .. }));
    }

    // ── validate_blueprint ────────────────────────────────────────────────────

    #[test]
    fn full_validation_passes() {
        let bp = dummy_bp(Some(30));
        let c = consent(true, 100, 60);
        let o = order(false, 1_000_000, 1_000_020);
        assert!(
            OfaChecker::validate_blueprint(&bp, &c, &o, &rules(), Utc::now(), 1_000_010,).is_ok()
        );
    }

    #[test]
    fn full_validation_short_circuits_on_consent() {
        let bp = dummy_bp(Some(30));
        let c = consent(false, 100, 60); // revoked
        let o = order(false, 1_000_000, 1_000_020);
        let err = OfaChecker::validate_blueprint(&bp, &c, &o, &rules(), Utc::now(), 1_000_010)
            .unwrap_err();
        assert_eq!(err.drop_code(), DropCode::MissOfaConsent);
    }
}
