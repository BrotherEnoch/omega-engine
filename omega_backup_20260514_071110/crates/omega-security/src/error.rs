// crates/omega-security/src/error.rs
//
// Unified error type for omega-security.
// Every public function that can fail returns Result<T, SecurityError>.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SecurityError {
    // â”€â”€ Signing â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    #[error("Blueprint signing failed: {detail}")]
    SigningFailed { detail: String },

    #[error("Signature verification failed for blueprint {blueprint_hash}")]
    SignatureInvalid { blueprint_hash: String },

    #[error("No active signing key available")]
    NoActiveKey,

    // â”€â”€ Key management â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    #[error("Key rotation rejected: pending key already set; wait for window to expire")]
    RotationAlreadyPending,

    #[error("Key rotation window expired at block {expired_at}, current block {current_block}")]
    RotationWindowExpired { expired_at: u64, current_block: u64 },

    #[error("Key rotation requires L2 governance approval (2-of-5 multisig)")]
    RotationRequiresGovernance,

    #[error("HSM endpoint unreachable: {detail}")]
    HsmUnavailable { detail: String },

    // â”€â”€ Replay protection â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    #[error("Blueprint replay detected: hash {hash} already executed on chain {chain_id}")]
    ReplayDetected { hash: String, chain_id: u64 },

    #[error("Nonce mismatch for strategy {strategy_id} on chain {chain_id}: expected {expected}, got {got}")]
    NonceMismatch { strategy_id: String, chain_id: u64, expected: u64, got: u64 },

    #[error("Nonce overflow for strategy {strategy_id}: cannot exceed u64::MAX")]
    NonceOverflow { strategy_id: String },

    // â”€â”€ OFA compliance â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    #[error("OFA violation: missing user consent signature on blueprint {blueprint_hash}")]
    MissingConsentSig { blueprint_hash: String },

    #[error("OFA violation: user slippage exceeded by {excess_bps} bps (max {max_bps} bps)")]
    SlippageExceeded { excess_bps: u16, max_bps: u16 },

    #[error("OFA violation: user transaction must appear before omega transaction in bundle")]
    BundleOrderViolation,

    #[error("OFA violation: bundle submitted to non-private relay {relay}")]
    NonPrivateRelay { relay: String },

    #[error("OFA rule set version mismatch: expected v{expected}, got v{got}")]
    RuleVersionMismatch { expected: u32, got: u32 },

    #[error("OFA rule set not loaded â€” call load_rules() before compliance checks")]
    RulesNotLoaded,

    // â”€â”€ Integrity â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    #[error("Bytecode integrity check failed for strategy {strategy_id}: hash mismatch")]
    BytecodeMismatch { strategy_id: String },

    #[error("Strategy {strategy_id} is frozen â€” no further blueprints permitted")]
    StrategyFrozen { strategy_id: String },

    #[error("Strategy {strategy_id} not found in integrity registry")]
    StrategyUnknown { strategy_id: String },

    #[error("Chain ID mismatch: blueprint targets chain {bp_chain}, orchestrator expects {expected_chain}")]
    ChainIdMismatch { bp_chain: u64, expected_chain: u64 },

    // â”€â”€ Internal â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    #[error("Internal security error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl SecurityError {
    /// True for errors that should trigger an L0 HALT (irreversible integrity failures).
    pub fn is_halt_worthy(&self) -> bool {
        matches!(
            self,
            SecurityError::ReplayDetected { .. }
                | SecurityError::BytecodeMismatch { .. }
                | SecurityError::ChainIdMismatch { .. }
        )
    }

    /// True for errors that represent an OFA compliance violation (logged + drop, no halt).
    pub fn is_ofa_violation(&self) -> bool {
        matches!(
            self,
            SecurityError::MissingConsentSig { .. }
                | SecurityError::SlippageExceeded { .. }
                | SecurityError::BundleOrderViolation
                | SecurityError::NonPrivateRelay { .. }
        )
    }
}