// crates/omega-relay/src/error.rs
//! Exhaustive error type for `omega-relay`.
//!
//! All public functions return `Result<_, RelayError>`. `anyhow` is used only
//! inside private implementation details and converted at the boundary.

use thiserror::Error;

/// Exhaustive relay-layer error taxonomy.
#[derive(Debug, Error)]
pub enum RelayError {
    // ── submission errors ────────────────────────────────────────────────────
    /// Relay responded with a non-success HTTP status and body.
    #[error("relay {relay} HTTP error: {status} — {body}")]
    HttpError {
        /// Relay name that returned the HTTP error.
        relay: String,
        /// HTTP status code returned by the relay.
        status: u16,
        /// Response body returned by the relay.
        body: String,
    },

    /// Relay request failed before a valid HTTP response was obtained.
    #[error("relay {relay} request failed: {source}")]
    RequestFailed {
        /// Relay name whose request failed.
        relay: String,
        #[source]
        /// Underlying transport or protocol error from `reqwest`.
        source: reqwest::Error,
    },

    /// All configured relays rejected or failed a bundle submission.
    #[error("all relays rejected bundle {bundle_hash}: no inclusion")]
    AllRelaysFailed {
        /// Bundle hash that failed across all relays.
        bundle_hash: String,
    },

    /// Relay rejected a submission due to rate limiting.
    #[error("relay {relay} rate-limited (429)")]
    RateLimited {
        /// Relay name that rate-limited the submission.
        relay: String,
    },

    // ── blacklist errors ─────────────────────────────────────────────────────
    /// Builder blacklist file could not be read from disk.
    #[error("builder blacklist load failed from {path}: {source}")]
    BlacklistLoadFailed {
        /// Filesystem path that failed to load.
        path: String,
        #[source]
        /// Underlying I/O error while reading the blacklist file.
        source: std::io::Error,
    },

    /// Builder blacklist contents could not be parsed.
    #[error("builder blacklist parse error: {0}")]
    BlacklistParseFailed(String),

    // ── reputation errors ────────────────────────────────────────────────────
    /// No relay metrics were available for the requested operation.
    #[error("no relay metrics available for address rotation")]
    NoRelayMetrics,

    // ── reorg errors ─────────────────────────────────────────────────────────
    /// A submitted blueprint entered reorg-risk state.
    #[error("blueprint {tx_hash} entered Reorg-Risk state at orphaned block {block}")]
    ReorgRisk {
        /// Blueprint transaction hash affected by the reorg.
        tx_hash: String,
        /// Orphaned block that invalidated the submission.
        block: u64,
    },

    // ── dedup errors ─────────────────────────────────────────────────────────
    /// A position was submitted twice inside the same restart window.
    #[error(
        "position {position_key} already submitted in restart window (block {submitted_block})"
    )]
    DuplicateSubmission {
        /// Stable key of the duplicated position.
        position_key: String,
        /// First block at which the position was submitted.
        submitted_block: u64,
    },

    // ── config errors ────────────────────────────────────────────────────────
    /// Relay configuration failed validation.
    #[error("relay config invalid: {0}")]
    ConfigInvalid(String),

    // ── I/O ──────────────────────────────────────────────────────────────────
    /// Generic I/O error surfaced by the relay layer.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization or deserialization error.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Standard result type for public relay-layer operations.
pub type RelayResult<T> = Result<T, RelayError>;
