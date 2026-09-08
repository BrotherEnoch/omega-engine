// crates/omega-execution/src/audit.rs
//
// Structured audit log for every admitted and rejected blueprint
// (priority 19). Designed so a trade can be reconstructed from logs alone:
// timestamp, strategy, signal, risk outcome, size, submission result,
// confirmation, provisional/realized P&L, shutdown/halt context.
//
// Emits via `tracing` at INFO (admitted/confirmed) or WARN (rejected).
// Field names are stable snake_case for log pipelines.

use alloy_primitives::{Address, B256, U256};
use chrono::{DateTime, Utc};
use tracing::{info, warn};

/// Why a blueprint was rejected before or during submission.
#[derive(Debug, Clone)]
pub enum RejectReason {
    Halted,
    PhaseZero,
    IntegrityFailure,
    KillSwitch { scope: String, detail: String },
    IntegrityRegistry { detail: String },
    RiskCheck { drop_code: String },
    DuplicateIdempotency,
    InvalidFlashloanIdentity { detail: String },
    Signing { detail: String },
    Relay { detail: String },
    Other { detail: String },
}

impl RejectReason {
    pub fn as_str(&self) -> &str {
        match self {
            RejectReason::Halted => "halted",
            RejectReason::PhaseZero => "phase_zero",
            RejectReason::IntegrityFailure => "integrity_failure",
            RejectReason::KillSwitch { .. } => "kill_switch",
            RejectReason::IntegrityRegistry { .. } => "integrity_registry",
            RejectReason::RiskCheck { .. } => "risk_check",
            RejectReason::DuplicateIdempotency => "duplicate_idempotency",
            RejectReason::InvalidFlashloanIdentity { .. } => "invalid_flashloan_identity",
            RejectReason::Signing { .. } => "signing",
            RejectReason::Relay { .. } => "relay",
            RejectReason::Other { .. } => "other",
        }
    }
}

/// Snapshot of blueprint fields relevant to audit reconstruction.
#[derive(Debug, Clone)]
pub struct BlueprintAuditView {
    pub blueprint_hash: B256,
    pub strategy_id: String,
    pub chain_id: u64,
    pub nonce: u64,
    pub signal_id: String,
    pub flashloan_token: Address,
    pub flashloan_amount: U256,
    pub expected_profit_net: U256,
    pub idempotency_key: B256,
    pub lane: String,
}

/// Log a rejection (never submitted, or failed before acceptance).
pub fn log_rejected(
    at: DateTime<Utc>,
    bp: &BlueprintAuditView,
    reason: &RejectReason,
    active_phase: u8,
) {
    warn!(
        audit = "blueprint_rejected",
        at = %at.to_rfc3339(),
        blueprint_hash = %format!("0x{}", hex::encode(bp.blueprint_hash.as_slice())),
        strategy_id = %bp.strategy_id,
        chain_id = bp.chain_id,
        nonce = bp.nonce,
        signal_id = %bp.signal_id,
        flashloan_token = %bp.flashloan_token,
        flashloan_amount = %bp.flashloan_amount,
        expected_profit_net = %bp.expected_profit_net,
        idempotency_key = %format!("0x{}", hex::encode(bp.idempotency_key.as_slice())),
        lane = %bp.lane,
        active_phase,
        reject_reason = reason.as_str(),
        reject_detail = ?reason,
        "audit: blueprint rejected"
    );
}

/// Log successful passage through the pipeline (relay accepted).
pub fn log_submitted(
    at: DateTime<Utc>,
    bp: &BlueprintAuditView,
    bundle_hash: &str,
    any_accepted: bool,
    active_phase: u8,
) {
    info!(
        audit = "blueprint_submitted",
        at = %at.to_rfc3339(),
        blueprint_hash = %format!("0x{}", hex::encode(bp.blueprint_hash.as_slice())),
        strategy_id = %bp.strategy_id,
        chain_id = bp.chain_id,
        nonce = bp.nonce,
        signal_id = %bp.signal_id,
        flashloan_token = %bp.flashloan_token,
        flashloan_amount = %bp.flashloan_amount,
        expected_profit_net = %bp.expected_profit_net,
        idempotency_key = %format!("0x{}", hex::encode(bp.idempotency_key.as_slice())),
        lane = %bp.lane,
        bundle_hash = %bundle_hash,
        any_accepted,
        active_phase,
        "audit: blueprint submitted to relay"
    );
}

/// Log Stage 7 confirmation outcome.
#[allow(clippy::too_many_arguments)]
pub fn log_confirmed(
    at: DateTime<Utc>,
    strategy_id: &str,
    blueprint_hash: &str,
    bundle_hash: &str,
    included: bool,
    provisional_realized_wei: Option<i128>,
    vault_realized_wei: Option<i128>,
    kill_tripped: bool,
) {
    info!(
        audit = "blueprint_confirmed",
        at = %at.to_rfc3339(),
        strategy_id = %strategy_id,
        blueprint_hash = %blueprint_hash,
        bundle_hash = %bundle_hash,
        included,
        provisional_realized_wei = ?provisional_realized_wei,
        vault_realized_wei = ?vault_realized_wei,
        kill_tripped,
        "audit: Stage-7 confirmation processed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_reason_labels_are_stable() {
        assert_eq!(RejectReason::Halted.as_str(), "halted");
        assert_eq!(
            RejectReason::InvalidFlashloanIdentity { detail: "x".into() }.as_str(),
            "invalid_flashloan_identity"
        );
    }
}
