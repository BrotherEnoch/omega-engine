// crates/omega-relay/src/error.rs
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("relay {relay} HTTP error: {status} — {body}")]
    HttpError { relay: String, status: u16, body: String },

    #[error("relay {relay} request failed: {source}")]
    RequestFailed { relay: String, #[source] source: reqwest::Error },

    #[error("all relays rejected bundle {bundle_hash}: no inclusion")]
    AllRelaysFailed { bundle_hash: String },

    #[error("relay {relay} rate-limited (429)")]
    RateLimited { relay: String },

    #[error("builder blacklist load failed from {path}: {source}")]
    BlacklistLoadFailed { path: String, #[source] source: std::io::Error },

    #[error("builder blacklist parse error: {0}")]
    BlacklistParseFailed(String),

    #[error("no relay metrics available for address rotation")]
    NoRelayMetrics,

    #[error("blueprint {tx_hash} entered Reorg-Risk state at orphaned block {block}")]
    ReorgRisk { tx_hash: String, block: u64 },

    #[error("position {position_key} already submitted in restart window (block {submitted_block})")]
    DuplicateSubmission { position_key: String, submitted_block: u64 },

    #[error("relay config invalid: {0}")]
    ConfigInvalid(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("inclusion confirmation RPC failed: {0}")]
    ConfirmationRpcFailed(String),
}

pub type RelayResult<T> = Result<T, RelayError>;