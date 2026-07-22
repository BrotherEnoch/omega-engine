ï»¿// ops/control-plane/src/handlers/health.rs
//
// Health, config, and vault endpoints (Â§17, Â§17.2).
//
// Routes handled here:
//   GET  /health                                â€” liveness probe (no auth)
//   GET  /api/v1/health                         â€” all 16 layer states
//   GET  /api/v1/config                         â€” config snapshot (auth L1)
//   POST /api/v1/config                         â€” hot-reload config (auth L1)
//   POST /api/v1/health/clear-halt/{layer}      â€” clear halt (auth L2)
//   GET  /api/v1/vault/dao-fee                  â€” DAO fee config (auth L1)
//   GET  /api/v1/builders/blacklist             â€” blacklist info (auth L1)
//   POST /api/v1/builders/blacklist/update      â€” hot-reload blacklist (auth L2)

pub use crate::state::load_config;

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

use omega_core::{HealthState, LayerHealth, VaultConfig};

use crate::auth::{check_auth, ApiError, ApiOk, OK};
use crate::state::{AppState, WsEvent, ALL_LAYER_IDS};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// GET /health â€” liveness probe (no auth)
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub async fn liveness() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })))
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// GET /api/v1/health â€” all 16 layer states
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[derive(Serialize)]
pub struct HealthSnapshot {
    pub generated_at:  chrono::DateTime<chrono::Utc>,
    pub layers:        Vec<LayerHealthEntry>,
    pub system_halted: bool,
}

#[derive(Serialize)]
pub struct LayerHealthEntry {
    pub layer:          String,
    pub state:          String,
    pub is_operational: bool,
}

pub async fn get_health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let layers: Vec<LayerHealthEntry> = state
        .health_layers
        .iter()
        .map(|l| LayerHealthEntry {
            layer:          l.layer_id().to_string(),
            state:          l.state().to_string(),
            is_operational: l.is_operational(),
        })
        .collect();

    let system_halted = layers.iter().any(|l| l.state == "HALTED");

    (
        StatusCode::OK,
        Json(HealthSnapshot {
            generated_at: chrono::Utc::now(),
            layers,
            system_halted,
        }),
    )
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// GET /api/v1/config â€” config snapshot
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub async fn get_config(
    headers:      axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }
    let config = state.config.read().await.clone();
    (StatusCode::OK, Json(config)).into_response()
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// POST /api/v1/config â€” hot-reload (L1)
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[derive(Deserialize)]
pub struct ConfigReloadRequest {
    pub from_disk: bool,
    pub body:      Option<serde_json::Value>,
}

pub async fn post_config(
    headers:      axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(req):    Json<ConfigReloadRequest>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }

    let new_config = if req.from_disk {
        match load_config(state.config_path.to_str().unwrap_or("")) {
            Ok(c)  => c,
            Err(e) => return (
                StatusCode::BAD_REQUEST,
                Json(ApiError::new("CONFIG_PARSE_ERROR", e.to_string())),
            ).into_response(),
        }
    } else {
        let body = match req.body {
            Some(b) => b,
            None => return (
                StatusCode::BAD_REQUEST,
                Json(ApiError::new("MISSING_BODY", "from_disk=false requires a config body")),
            ).into_response(),
        };
        match serde_json::from_value(body) {
            Ok(c)  => c,
            Err(e) => return (
                StatusCode::BAD_REQUEST,
                Json(ApiError::new("CONFIG_PARSE_ERROR", e.to_string())),
            ).into_response(),
        }
    };

    let errors = new_config.validate();
    if !errors.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError::new("CONFIG_VALIDATION_FAILED", errors.join("; "))),
        ).into_response();
    }

    *state.config.write().await = new_config;
    state.publish(WsEvent::ConfigReloaded { timestamp: chrono::Utc::now() });
    tracing::info!("Config hot-reloaded (L1)");

    (StatusCode::OK, Json(OK)).into_response()
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// POST /api/v1/health/clear-halt/{layer} â€” clear halt (L2 governance)
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub async fn clear_halt(
    headers:      axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
    Path(layer):  Path<String>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }

    let layer_id = ALL_LAYER_IDS.iter().find(|&&id| id.to_string() == layer);
    let Some(&layer_id) = layer_id else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError::new("UNKNOWN_LAYER", format!("Layer '{layer}' not found"))),
        ).into_response();
    };

    let Some(ctrl) = state.layer(layer_id) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("LAYER_CTRL_MISSING", "Health controller not wired")),
        ).into_response();
    };

    ctrl.set_state(HealthState::Healthy, "cleared by governance (L2 fast-approve)");

    state.publish(WsEvent::HealthTransition {
        layer:     layer.clone(),
        from:      "HALTED".into(),
        to:        "HEALTHY".into(),
        reason:    "governance clear-halt (L2)".into(),
        timestamp: chrono::Utc::now(),
    });

    tracing::warn!(layer = %layer, "Halt cleared by governance (L2 fast-approve)");

    (StatusCode::OK, Json(OK)).into_response()
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// GET /api/v1/vault/dao-fee â€” DAO fee configuration (Â§15.1)
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[derive(Serialize)]
pub struct DaoFeeResponse {
    pub dao_fee_bps:          u16,
    pub dao_fee_pct:          f64,
    pub per_transfer_cap_eth: f64,
    pub daily_cap_eth:        f64,
    pub confirmation_depth:   u8,
}

pub async fn get_dao_fee(
    headers:      axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }

    let vault: VaultConfig = state.config.read().await.vault.clone();

    (
        StatusCode::OK,
        Json(DaoFeeResponse {
            dao_fee_bps:          vault.dao_fee_bps,
            dao_fee_pct:          vault.dao_fee_bps as f64 / 100.0,
            per_transfer_cap_eth: vault.per_transfer_cap_wei as f64 / 1e18,
            daily_cap_eth:        vault.daily_cap_wei as f64 / 1e18,
            confirmation_depth:   vault.confirmation_depth,
        }),
    ).into_response()
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// GET /api/v1/builders/blacklist â€” read blacklist metadata (Â§12.3)
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[derive(Serialize)]
pub struct BlacklistResponse {
    pub entry_count: usize,
    pub path:        String,
    pub is_empty:    bool,
}

pub async fn get_blacklist(
    headers:      axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }

    (
        StatusCode::OK,
        Json(BlacklistResponse {
            entry_count: state.blacklist.len(),
            path:        state.blacklist.path().display().to_string(),
            is_empty:    state.blacklist.is_empty(),
        }),
    ).into_response()
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// POST /api/v1/builders/blacklist/update â€” hot-reload blacklist (Â§12.3, L2)
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub async fn post_blacklist_update(
    headers:      axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }

    match state.blacklist.reload() {
        Ok(()) => {
            let count = state.blacklist.len();
            state.publish(WsEvent::BlacklistReloaded {
                entry_count: count,
                timestamp:   chrono::Utc::now(),
            });
            tracing::info!(entry_count = count, "Builder blacklist hot-reloaded (L2)");
            (StatusCode::OK, Json(OK)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Builder blacklist reload failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("BLACKLIST_RELOAD_ERROR", e.to_string())),
            ).into_response()
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_config_missing_file_gives_defaults() {
        let cfg = load_config("/tmp/omega_nonexistent_xyz_987.toml").unwrap();
        assert_eq!(cfg.active_phase, 0);
    }

    #[test]
    fn load_config_valid_toml_phase_1() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "active_phase = 1\n").unwrap();
        let cfg = load_config(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(cfg.active_phase, 1);
    }

    #[test]
    fn load_config_invalid_phase_fails_validation() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "active_phase = 99\n").unwrap();
        assert!(load_config(tmp.path().to_str().unwrap()).is_err());
    }

    #[test]
    fn load_config_bad_toml_returns_error() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "not valid {{{{ toml").unwrap();
        assert!(load_config(tmp.path().to_str().unwrap()).is_err());
    }
}