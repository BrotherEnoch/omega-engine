// omega-frontend-arch/crates/omega-control-contracts/src/error.rs
//! Typed API error envelope for OmegaEngine v12 control plane.
//!
//! The backend returns a JSON error body on 4xx/5xx responses:
//! ```json
//! {"code": "UNAUTHORIZED", "message": "Bearer token required"}
//! ```
//! This module provides the typed representation and maps it to a
//! [`thiserror`]-derived error hierarchy for ergonomic handling.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// API error codes returned by the control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApiErrorCode {
    // 4xx
    Unauthorized,
    Forbidden,
    NotFound,
    BadRequest,
    Conflict,
    // 5xx
    InternalError,
    ServiceUnavailable,
    // Domain-specific
    CheckpointNotFound,
    ModelPaused,
    GovernanceTimelockActive,
    BlacklistUpdateRejected,
}

/// Typed API error body. Deserialised from the backend error JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ApiErrorCode,
    pub message: String,
    /// Optional request correlation ID for log tracing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// Frontend-side error hierarchy, including transport and parse failures.
#[derive(Debug, Error)]
pub enum FrontendError {
    #[error("API error {code:?}: {message}")]
    Api { code: ApiErrorCode, message: String },

    #[error("WebSocket connection failed: {0}")]
    WsConnection(String),

    #[error("Deserialisation failed: {0}")]
    Deserialise(#[from] serde_json::Error),

    #[error("WebSocket endpoint unavailable — backend has not mounted /ws/events")]
    WsEndpointNotMounted,

    #[error("Authentication required")]
    Unauthenticated,

    #[error("Rate limit exceeded ({limit}/min)")]
    RateLimit { limit: u32 },
}

impl From<ApiError> for FrontendError {
    fn from(e: ApiError) -> Self {
        FrontendError::Api { code: e.code, message: e.message }
    }
}