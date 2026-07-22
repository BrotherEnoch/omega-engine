// crates/omega-execution/src/error.rs
//
// Error type for the execution pipeline stage (see
// ExecutionPipelineSpecification.md §9's failure taxonomy).
//
// Deliberately its own type, not folded into omega_core::errors::DropCode:
// DropCode is defined in omega-core, which this crate cannot modify, and
// several failure modes here (kill-switch trip, missing transaction signer,
// unresolvable flashloan provider) have no corresponding DropCode variant
// and shouldn't be forced into one just to fit an existing enum — see the
// spec's §9 table for which failures DO map onto an existing DropCode
// (and use it directly, via `RiskCheckFailed`) versus which don't.

use omega_core::errors::DropCode;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("blueprint failed integrity check (verify_hash/verify_idempotency_key)")]
    IntegrityFailure,

    #[error("bytecode integrity registry check failed: {0}")]
    IntegrityRegistryCheckFailed(#[from] omega_security::SecurityError),

    #[error("kill switch tripped for scope {scope}: {reason}")]
    KillSwitchTripped { scope: String, reason: String },

    #[error("pre-trade check failed: {0:?}")]
    RiskCheckFailed(DropCode),

    #[error("duplicate idempotency key — blueprint already submitted")]
    DuplicateIdempotencyKey,

    #[error(
        "no flashloan provider->protocol-name mapping available for address {address} — \
         no such table exists anywhere in the omega-engine workspace as of this pipeline's \
         implementation. Failing closed rather than submitting a flashloan blueprint through \
         a no-self-flash check that could never actually fire against a fabricated or \
         unmatchable placeholder value."
    )]
    UnknownFlashloanProvider { address: String },

    #[error(
        "no TransactionSigner configured — this pipeline cannot produce a signed transaction \
         for relay submission. See signer.rs's TransactionSigner trait doc comment: no \
         implementation of this trait exists anywhere in the omega-engine workspace as of \
         ExecutionPipelineSpecification.md. This is not a bug in this pipeline — it is a \
         genuinely unimplemented dependency that must be supplied before active_phase >= 1 \
         can submit real bundles."
    )]
    NoTransactionSigner,

    #[error("transaction signing failed: {detail}")]
    SigningFailed { detail: String },

    #[error("relay submission failed: {0}")]
    RelaySubmissionFailed(#[from] omega_relay::RelayError),
}