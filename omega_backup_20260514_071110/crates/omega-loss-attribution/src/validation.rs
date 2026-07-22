// crates/omega-loss-attribution/src/validation.rs
//
// Validation layer for the Loss Attribution Engine (Â§13).
//
// Every loss event and execution trace entering the attribution pipeline
// must pass validation before being processed.  Invalid events are
// counted by the dashboard and logged; they are never fed into the ML
// model.
//
// ## Design notes
//
// `ValidationError` uses a `code: &'static str` rather than `String`
// so that error codes can be used as Prometheus label values without
// allocation.  The `message` field is for human-readable context only.
//
// `ExecutionTrace::tx_hash` is `alloy_primitives::TxHash` (= B256),
// matching the type used in omega-health's ReorgGuard and throughout
// the crate graph.  The original `String` forced parsing at every call
// site and made the type unsound (any string was accepted).
//
// `LossEvent` here is the *pipeline-internal* event that carries the
// estimated USD loss for dashboard aggregation.  It is distinct from
// `classifier::LossEvent` (which carries the ML training signal).
// The dashboard module uses this type; the ML learner uses the classifier
// type.

use alloy_primitives::{TxHash, B256};
use std::collections::HashMap;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ValidationError
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// A structured validation failure.
///
/// `code` is a `&'static str` â€” suitable as a Prometheus label without
/// heap allocation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ValidationError {
    /// Machine-readable SCREAMING_SNAKE_CASE error code.
    pub code:    &'static str,
    /// Human-readable description for logs and ops dashboards.
    pub message: String,
}

impl ValidationError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

/// Standard result type for all validators in this crate.
pub type ValidationResult<T> = Result<T, ValidationError>;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ExecutionTrace
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Canonical execution trace fragment for attribution (Â§13).
///
/// `tx_hash` is `TxHash` (= `B256`) â€” type-safe, zero-copy,
/// and compatible with the reorg guard in omega-health.
#[derive(Debug, Clone)]
pub struct ExecutionTrace {
    /// On-chain transaction hash.
    pub tx_hash:      TxHash,
    /// Block number the transaction was included in (or attempted at).
    pub block_number: u64,
    /// Actual gas units consumed on-chain.
    pub gas_used:     u64,
    /// Whether the transaction succeeded (`true`) or reverted (`false`).
    pub success:      bool,
    /// Arbitrary protocol-specific metadata (pool addresses, position IDs, etc.).
    pub metadata:     HashMap<String, String>,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// PipelineLossEvent
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// A pipeline-level loss event for dashboard aggregation.
///
/// This is distinct from `classifier::LossEvent` (the ML training signal).
/// The dashboard module uses this type to track estimated USD loss; the
/// ML learner uses `classifier::LossEvent`.
#[derive(Debug, Clone)]
pub struct PipelineLossEvent {
    /// On-chain transaction hash.
    pub tx_hash:            TxHash,
    /// Estimated loss in USD at the time of attribution.
    pub estimated_loss_usd: f64,
    /// Human-readable cause description.
    pub cause:              String,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Validator trait
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Extensible validator interface.
///
/// Each pipeline stage may have its own validator implementation.
/// `validate` must be pure and side-effect-free.
pub trait Validator<T>: Send + Sync {
    fn validate(&self, input: &T) -> ValidationResult<()>;
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ExecutionTraceValidator
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Validates structural integrity of an `ExecutionTrace`.
pub struct ExecutionTraceValidator;

impl Validator<ExecutionTrace> for ExecutionTraceValidator {
    fn validate(&self, trace: &ExecutionTrace) -> ValidationResult<()> {
        // tx_hash must not be the zero hash
        if trace.tx_hash == B256::ZERO {
            return Err(ValidationError::new(
                "ZERO_TX_HASH",
                "Transaction hash is zero â€” trace was not submitted on-chain",
            ));
        }

        // Block number must be non-zero
        if trace.block_number == 0 {
            return Err(ValidationError::new(
                "INVALID_BLOCK",
                "Block number must be greater than 0",
            ));
        }

        // Gas used must be > 0 for a submitted transaction
        if trace.gas_used == 0 {
            return Err(ValidationError::new(
                "ZERO_GAS_USED",
                "Gas used is 0 â€” trace appears to be a pre-execution estimate, \
                 not an on-chain result",
            ));
        }

        Ok(())
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// PipelineLossEventValidator
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Validates a `PipelineLossEvent` before it enters the dashboard.
pub struct PipelineLossEventValidator;

impl Validator<PipelineLossEvent> for PipelineLossEventValidator {
    fn validate(&self, event: &PipelineLossEvent) -> ValidationResult<()> {
        if event.tx_hash == B256::ZERO {
            return Err(ValidationError::new(
                "ZERO_TX_HASH",
                "Loss event missing valid transaction hash",
            ));
        }

        if event.estimated_loss_usd.is_nan() {
            return Err(ValidationError::new(
                "NAN_LOSS",
                "estimated_loss_usd is NaN â€” invalid numerical state",
            ));
        }

        if event.estimated_loss_usd.is_infinite() {
            return Err(ValidationError::new(
                "INFINITE_LOSS",
                "estimated_loss_usd is infinite â€” oracle or calculation error",
            ));
        }

        if event.estimated_loss_usd < 0.0 {
            return Err(ValidationError::new(
                "NEGATIVE_LOSS",
                format!(
                    "estimated_loss_usd is negative ({:.6}) â€” \
                     negative losses must be recorded as gains separately",
                    event.estimated_loss_usd,
                ),
            ));
        }

        Ok(())
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// AttributionValidator
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Composite validator for the full trace + loss event pair.
///
/// Validates both inputs and then cross-checks that they reference the
/// same on-chain transaction.
pub struct AttributionValidator {
    pub trace_validator: ExecutionTraceValidator,
    pub loss_validator:  PipelineLossEventValidator,
}

impl AttributionValidator {
    pub fn new() -> Self {
        Self {
            trace_validator: ExecutionTraceValidator,
            loss_validator:  PipelineLossEventValidator,
        }
    }

    /// Validate both trace and loss event, then cross-check consistency.
    pub fn validate_pair(
        &self,
        trace: &ExecutionTrace,
        event: &PipelineLossEvent,
    ) -> ValidationResult<()> {
        self.trace_validator.validate(trace)?;
        self.loss_validator.validate(event)?;

        if trace.tx_hash != event.tx_hash {
            return Err(ValidationError::new(
                "TX_HASH_MISMATCH",
                format!(
                    "trace.tx_hash ({}) != event.tx_hash ({}) â€” \
                     trace and loss event do not reference the same transaction",
                    trace.tx_hash, event.tx_hash,
                ),
            ));
        }

        Ok(())
    }
}

impl Default for AttributionValidator {
    fn default() -> Self {
        Self::new()
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_trace() -> ExecutionTrace {
        ExecutionTrace {
            tx_hash:      TxHash::from([1u8; 32]),
            block_number: 100,
            gas_used:     150_000,
            success:      true,
            metadata:     HashMap::new(),
        }
    }

    fn valid_event() -> PipelineLossEvent {
        PipelineLossEvent {
            tx_hash:            TxHash::from([1u8; 32]),
            estimated_loss_usd: 1_500.0,
            cause:              "LOST_GAS_LOW".into(),
        }
    }

    // â”€â”€ ExecutionTraceValidator â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn valid_trace_passes() {
        assert!(ExecutionTraceValidator.validate(&valid_trace()).is_ok());
    }

    #[test]
    fn zero_tx_hash_rejected() {
        let mut t = valid_trace();
        t.tx_hash = B256::ZERO;
        let e = ExecutionTraceValidator.validate(&t).unwrap_err();
        assert_eq!(e.code, "ZERO_TX_HASH");
    }

    #[test]
    fn zero_block_number_rejected() {
        let mut t = valid_trace();
        t.block_number = 0;
        let e = ExecutionTraceValidator.validate(&t).unwrap_err();
        assert_eq!(e.code, "INVALID_BLOCK");
    }

    #[test]
    fn zero_gas_used_rejected() {
        let mut t = valid_trace();
        t.gas_used = 0;
        let e = ExecutionTraceValidator.validate(&t).unwrap_err();
        assert_eq!(e.code, "ZERO_GAS_USED");
    }

    // â”€â”€ PipelineLossEventValidator â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn valid_event_passes() {
        assert!(PipelineLossEventValidator.validate(&valid_event()).is_ok());
    }

    #[test]
    fn nan_loss_rejected() {
        let mut e = valid_event();
        e.estimated_loss_usd = f64::NAN;
        let err = PipelineLossEventValidator.validate(&e).unwrap_err();
        assert_eq!(err.code, "NAN_LOSS");
    }

    #[test]
    fn infinite_loss_rejected() {
        let mut e = valid_event();
        e.estimated_loss_usd = f64::INFINITY;
        let err = PipelineLossEventValidator.validate(&e).unwrap_err();
        assert_eq!(err.code, "INFINITE_LOSS");
    }

    #[test]
    fn negative_loss_rejected() {
        let mut e = valid_event();
        e.estimated_loss_usd = -0.01;
        let err = PipelineLossEventValidator.validate(&e).unwrap_err();
        assert_eq!(err.code, "NEGATIVE_LOSS");
    }

    #[test]
    fn zero_loss_is_valid() {
        let mut e = valid_event();
        e.estimated_loss_usd = 0.0;
        assert!(PipelineLossEventValidator.validate(&e).is_ok(),
            "zero loss is a valid outcome");
    }

    // â”€â”€ AttributionValidator â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn valid_pair_passes() {
        let v = AttributionValidator::new();
        assert!(v.validate_pair(&valid_trace(), &valid_event()).is_ok());
    }

    #[test]
    fn mismatched_tx_hashes_rejected() {
        let v = AttributionValidator::new();
        let mut event = valid_event();
        event.tx_hash = TxHash::from([2u8; 32]);
        let err = v.validate_pair(&valid_trace(), &event).unwrap_err();
        assert_eq!(err.code, "TX_HASH_MISMATCH");
    }

    #[test]
    fn invalid_trace_short_circuits() {
        let v = AttributionValidator::new();
        let mut trace = valid_trace();
        trace.block_number = 0;
        let err = v.validate_pair(&trace, &valid_event()).unwrap_err();
        assert_eq!(err.code, "INVALID_BLOCK",
            "trace validation must short-circuit before event validation");
    }
}