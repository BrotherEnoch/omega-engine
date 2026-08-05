// crates/omega-relay/src/error.rs
//! Error types for the relay submission layer.
//!
//! `RelayError` is the single error type returned across bundle
//! submission, builder-blacklist loading, sequencer-restart dedup, and
//! inclusion confirmation. `missing_docs` is denied crate-wide (see
//! `lib.rs`), and that lint applies to the module declaration itself
//! (`pub mod error;`) as well as to the items inside it — a doc comment
//! on `RelayError` alone isn't enough; this file-level `//!` doc is what
//! satisfies the module-level requirement.

use thiserror::Error;

/// Errors produced by the relay submission layer.
#[derive(Debug, Error)]
pub enum RelayError {
    /// A relay responded with a non-success HTTP status.
    #[error("relay {relay} HTTP error: {status} — {body}")]
    HttpError {
        /// Name of the relay that responded.
        relay: String,
        /// HTTP status code returned.
        status: u16,
        /// Response body, for diagnostics.
        body: String,
    },

    /// The HTTP request to a relay failed at the transport level.
    #[error("relay {relay} request failed: {source}")]
    RequestFailed {
        /// Name of the relay the request was sent to.
        relay: String,
        /// Underlying reqwest error.
        #[source]
        source: reqwest::Error,
    },

    /// Every configured relay rejected the bundle.
    #[error("all relays rejected bundle {bundle_hash}: no inclusion")]
    AllRelaysFailed {
        /// Hash of the bundle that failed everywhere.
        bundle_hash: String,
    },

    /// A relay returned HTTP 429.
    #[error("relay {relay} rate-limited (429)")]
    RateLimited {
        /// Name of the relay that rate-limited the request.
        relay: String,
    },

    /// The builder blacklist file could not be read from disk.
    #[error("builder blacklist load failed from {path}: {source}")]
    BlacklistLoadFailed {
        /// Path the blacklist was loaded from.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The builder blacklist file was read but failed to parse.
    #[error("builder blacklist parse error: {0}")]
    BlacklistParseFailed(String),

    /// Address rotation was requested but no relay metrics exist yet.
    #[error("no relay metrics available for address rotation")]
    NoRelayMetrics,

    /// A submitted blueprint's transaction was orphaned by a reorg.
    #[error("blueprint {tx_hash} entered Reorg-Risk state at orphaned block {block}")]
    ReorgRisk {
        /// Hash of the affected transaction.
        tx_hash: String,
        /// Block number that was orphaned.
        block: u64,
    },

    /// The same position was already claimed within the restart window.
    #[error(
        "position {position_key} already submitted in restart window (block {submitted_block})"
    )]
    DuplicateSubmission {
        /// Key identifying the position.
        position_key: String,
        /// Block at which the earlier submission was made.
        submitted_block: u64,
    },

    /// Relay configuration failed validation.
    #[error("relay config invalid: {0}")]
    ConfigInvalid(String),

    /// A wrapped I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A wrapped JSON (de)serialization error.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// The confirmation RPC call failed.
    #[error("inclusion confirmation RPC failed: {0}")]
    ConfirmationRpcFailed(String),
}

/// Convenience alias for results from the relay layer.
pub type RelayResult<T> = Result<T, RelayError>;
