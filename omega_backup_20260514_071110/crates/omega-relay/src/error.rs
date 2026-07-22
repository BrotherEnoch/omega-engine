// crates/omega-relay/src/error.rs
//! Exhaustive error type for `omega-relay`.
//!
//! All public functions return `Result<_, RelayError>`. `anyhow` is used only
//! inside private implementation details and converted at the boundary.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelayError {
    // â”€â”€ submission errors â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    #[error("relay {relay} HTTP error: {status} â€” {body}")]
    HttpError {
        relay: String,
        status: u16,
        body: String,
    },

    #[error("relay {relay} request failed: {source}")]
    RequestFailed {
        relay: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("all relays rejected bundle {bundle_hash}: no inclusion")]
    AllRelaysFailed { bundle_hash: String },

    #[error("relay {relay} rate-limited (429)")]
    RateLimited { relay: String },

    // â”€â”€ blacklist errors â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    #[error("builder blacklist load failed from {path}: {source}")]
    BlacklistLoadFailed {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("builder blacklist parse error: {0}")]
    BlacklistParseFailed(String),

    // â”€â”€ reputation errors â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    #[error("no relay metrics available for address rotation")]
    NoRelayMetrics,

    // â”€â”€ reorg errors â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    #[error("blueprint {tx_hash} entered Reorg-Risk state at orphaned block {block}")]
    ReorgRisk { tx_hash: String, block: u64 },

    // â”€â”€ dedup errors â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    #[error("position {position_key} already submitted in restart window (block {submitted_block})")]
    DuplicateSubmission {
        position_key: String,
        submitted_block: u64,
    },

    // â”€â”€ config errors â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    #[error("relay config invalid: {0}")]
    ConfigInvalid(String),

    // â”€â”€ I/O â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type RelayResult<T> = Result<T, RelayError>;