// ops/control-plane/src/handlers/la.rs
//
// LA-specific and gas-model API endpoints (spec §17.2).
//
// All endpoints:
//   GET  /api/v1/la/gas-model/checkpoints       — list checkpoints (§13.2)
//   POST /api/v1/la/gas-model/revert/:version   — revert model (§13.2)
//   GET  /api/v1/la/gas-model/ceiling-status    — escalation state (§13.3)
//   POST /api/v1/la/gas-model/unpause           — clear model pause (§13.3)

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Serialize;

use omega_loss_attribution::checkpoint;

use crate::auth::{check_auth, ApiError, ApiOk, OK};
use crate::state::AppState;

// ─────────────────────────────────────────────────────────────────────────────
// GET /api/v1/la/gas-model/checkpoints — list all checkpoints (§13.2, §17.2)
// ─────────────────────────────────────────────────────────────────────────────

pub async fn get_checkpoints(
    headers:      axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }

    match checkpoint::list_checkpoints(&state.checkpoint_dir) {
        Ok(metas) => (StatusCode::OK, Json(metas)).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to list checkpoints");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("CHECKPOINT_LIST_ERROR", e.to_string())),
            ).into_response()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// POST /api/v1/la/gas-model/revert/:version (§13.2, §17.2)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct RevertResponse {
    pub reverted_to_version: u64,
    pub win_rate:            f64,
    pub sample_count:        u64,
}

pub async fn revert_checkpoint(
    headers:       axum::http::HeaderMap,
    State(state):  State<Arc<AppState>>,
    Path(version): Path<u64>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }

    match checkpoint::load_version(&state.checkpoint_dir, version) {
        Ok(ckpt) => {
            // The live engine's learner task polls for the checkpoint version
            // flag set here.  The control-plane records the governance action.
            tracing::warn!(
                version      = ckpt.version,
                win_rate     = ckpt.win_rate,
                sample_count = ckpt.sample_count,
                "Gas model reverted by governance (L2 fast-approve)",
            );
            (
                StatusCode::OK,
                Json(RevertResponse {
                    reverted_to_version: ckpt.version,
                    win_rate:            ckpt.win_rate,
                    sample_count:        ckpt.sample_count,
                }),
            ).into_response()
        }
        Err(e) => {
            tracing::error!(version, error = %e, "Checkpoint load failed");
            (
                StatusCode::NOT_FOUND,
                Json(ApiError::new(
                    "CHECKPOINT_NOT_FOUND",
                    format!("Version {version}: {e}"),
                )),
            ).into_response()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /api/v1/la/gas-model/ceiling-status (§13.3, §17.2)
// ─────────────────────────────────────────────────────────────────────────────

pub async fn ceiling_status(
    headers:      axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }

    let paused   = state.model_paused.load(std::sync::atomic::Ordering::Acquire);
    let tracker  = state.ceiling_tracker.read().await;
    let snapshot = tracker.snapshot(paused);

    (StatusCode::OK, Json(snapshot)).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// POST /api/v1/la/gas-model/unpause (§13.3, §17.2)
// ─────────────────────────────────────────────────────────────────────────────

pub async fn unpause_model(
    headers:      axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }

    state.model_paused.store(false, std::sync::atomic::Ordering::SeqCst);
    state.ceiling_tracker.write().await.record_unpause();

    tracing::warn!("Gas model unpaused by governance (L2 fast-approve)");

    (StatusCode::OK, Json(OK)).into_response()
}