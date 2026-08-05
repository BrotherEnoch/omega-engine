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
//
// ## Fix (this revision): BlueprintFieldOverflow
//
// `pipeline.rs::blueprint_to_check_fields` previously mapped
// `bp.expected_profit_net.try_into().unwrap_or(u128::MAX)` — coercing a
// U256-to-u128 overflow to `u128::MAX` rather than rejecting it. That
// direction fails OPEN in `omega_risk::checks::check_dynamic_profit`
// (`MAX < dynamic_min_profit_wei` is false for any realistic threshold),
// meaning an overflowed profit figure would silently pass the exact
// check meant to catch an unprofitable blueprint. Confirmed against the
// real check body, not inferred — `MAX < x` is false whenever `x <
// u128::MAX`, which is every realistic `dynamic_min_profit_wei`.
//
// `dynamic_min_profit` and `flashloan_amount` are NOT changed here: an
// overflow on either of those maps to `u128::MAX` and fails CLOSED
// (checks 5, 10, and 14 all reject when the compared-against value is
// astronomically large), confirmed the same way against
// `check_dynamic_profit`, `check_flashloan_liquidity`, and
// `check_account_exposure`'s real bodies. Only `expected_profit_net`
// needed this new error variant.

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

    /// `expected_profit_net` (U256, on `ExecutionBlueprint`) does not fit
    /// in `u128` (the type `omega_risk::checks::BlueprintFields` uses).
    ///
    /// Mapping this overflow to `u128::MAX` would fail OPEN in
    /// `check_dynamic_profit` — see this file's module-level "Fix (this
    /// revision)" note for the confirmed reasoning. Stage 1
    /// (`verify_hash`/`verify_idempotency_key`) only checks that the
    /// blueprint is internally self-consistent with its own claimed
    /// hash, not that its economic fields are sane — so an overflowed
    /// profit value is not caught anywhere before this mapping step
    /// unless this variant exists and is used.
    #[error(
        "blueprint field `{field}` does not fit in u128 — failing closed rather than \
         coercing to u128::MAX, which would fail open in check_dynamic_profit"
    )]
    BlueprintFieldOverflow { field: &'static str },
}
