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
//
// ## Fix (this revision): context_assembly + async surface errors
//
// `context_assembly.rs` fails closed when gas or nonce sources error.
// Those variants were referenced but missing from this enum — added as
// `GasSourceUnavailable` / `NonceSourceUnavailable`. Journal and vault
// lookup failures used by Stage 7 / inflight recovery are also first-class
// so async tasks can log typed errors instead of string-only messages.

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
        "invalid flashloan identity: {detail} — Orchestrator reverts on flashloanToken==0; \
         refusing to submit a blueprint that cannot succeed on-chain"
    )]
    InvalidFlashloanIdentity { detail: String },

    #[error(
        "no flashloan provider->protocol-name mapping available for address {address} — \
         failing closed rather than submitting a flashloan blueprint through a no-self-flash \
         check that could never fire against an unmatchable placeholder"
    )]
    UnknownFlashloanProvider { address: String },

    #[error(
        "no TransactionSigner configured — this pipeline cannot produce a signed transaction \
         for relay submission. A real TransactionSigner must be supplied before \
         active_phase >= 1 can submit real bundles."
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
    /// revision)" note for the confirmed reasoning.
    #[error(
        "blueprint field `{field}` does not fit in u128 — failing closed rather than \
         coercing to u128::MAX, which would fail open in check_dynamic_profit"
    )]
    BlueprintFieldOverflow { field: &'static str },

    /// Live fee / gas snapshot source failed (context assembly Stage 2).
    #[error("gas / fee source unavailable: {0}")]
    GasSourceUnavailable(String),

    /// Nonce registry / latest-nonce source failed (context assembly Stage 2).
    /// Fails closed — never default to 0 (would defeat stale-blueprint check).
    #[error("nonce source unavailable: {0}")]
    NonceSourceUnavailable(String),

    /// Durable in-flight journal I/O failed.
    #[error("in-flight journal I/O failed: {0}")]
    JournalIo(String),

    /// Vault pending_profit eth_call failed or returned unusable data.
    /// Stage 7 falls back to provisional expected profit; this variant is
    /// for callers that treat vault lookup as mandatory.
    #[error("vault realized-profit lookup failed for {blueprint_hash}: {detail}")]
    VaultLookupFailed {
        blueprint_hash: String,
        detail: String,
    },

    /// Background async task joined with an unexpected error.
    #[error("async task failed: {0}")]
    AsyncTaskFailed(String),
}

impl From<std::io::Error> for ExecutionError {
    fn from(e: std::io::Error) -> Self {
        ExecutionError::JournalIo(e.to_string())
    }
}
