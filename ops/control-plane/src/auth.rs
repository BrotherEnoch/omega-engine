// ops/control-plane/src/auth.rs
//
// Bearer token authentication used by every authenticated HTTP handler.
//
// ## Governance tiers (§5)
//
//   L1 (hot-reload config)   — Bearer token sufficient.
//   L2 (model revert, unpause, blacklist update) — Bearer token + the
//     caller must supply a multisig signature verified off-band at the
//     API gateway.  The control-plane enforces the Bearer token; the
//     gateway enforces the multisig.
//
// ## Usage
//
//   ```rust
//   async fn my_handler(
//       headers:      axum::http::HeaderMap,
//       State(state): State<Arc<AppState>>,
//   ) -> impl IntoResponse {
//       if let Err(e) = check_auth(&headers, &state.api_token) {
//           return e.into_response();
//       }
//       // ... handler body
//   }
//   ```

use axum::{http::StatusCode, Json};
use serde::Serialize;

// ─────────────────────────────────────────────────────────────────────────────
// Response types — used by all handlers
// ─────────────────────────────────────────────────────────────────────────────

/// JSON error body returned for 4xx and 5xx responses.
#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error:   String,
    pub message: String,
}

impl ApiError {
    pub fn new(error: impl Into<String>, message: impl Into<String>) -> Self {
        Self { error: error.into(), message: message.into() }
    }
}

/// JSON success body for write operations that have no meaningful payload.
#[derive(Debug, Serialize)]
pub struct ApiOk {
    pub status: &'static str,
}

pub const OK: ApiOk = ApiOk { status: "ok" };

// ─────────────────────────────────────────────────────────────────────────────
// check_auth
// ─────────────────────────────────────────────────────────────────────────────

/// Validate the `Authorization: Bearer <token>` header.
///
/// Returns `Ok(())` when the token matches.
/// Returns `Err((401, Json<ApiError>))` when the token is absent or wrong.
///
/// The return type is designed for direct use with `?` in axum handlers
/// that return `impl IntoResponse`.
pub fn check_auth(
    headers:   &axum::http::HeaderMap,
    api_token: &str,
) -> Result<(), (StatusCode, Json<ApiError>)> {
    let provided = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    if provided == api_token {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiError::new("UNAUTHORIZED", "Invalid or missing Bearer token")),
        ))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue, header::AUTHORIZATION};

    fn headers_with(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_str(value).unwrap());
        h
    }

    #[test]
    fn valid_token_passes() {
        assert!(check_auth(&headers_with("Bearer secret"), "secret").is_ok());
    }

    #[test]
    fn wrong_token_rejected() {
        assert!(check_auth(&headers_with("Bearer wrong"), "secret").is_err());
    }

    #[test]
    fn missing_header_rejected() {
        assert!(check_auth(&HeaderMap::new(), "secret").is_err());
    }

    #[test]
    fn no_bearer_prefix_rejected() {
        // Token present but without "Bearer " prefix
        assert!(check_auth(&headers_with("secret"), "secret").is_err());
    }

    #[test]
    fn error_response_is_401() {
        let (status, _) = check_auth(&HeaderMap::new(), "secret").unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}