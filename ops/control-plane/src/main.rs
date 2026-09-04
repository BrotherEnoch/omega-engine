// ops/control-plane/src/main.rs
//
// Omega Control-Plane — HTTP API server (spec §17, §17.1, §17.2).
//
// ## Purpose
//
//   The control-plane exposes the governance and observability API used
//   by operators, the governance multisig, and automated tooling to:
//     - Read system health state across all 16 layers (§3)
//     - Hot-reload L1 configuration without restart (§5)
//     - Manage the gas model: list checkpoints, revert, unpause (§13, §17.2)
//     - Inspect and update the MEV-Boost builder blacklist (§12.3, §17.2)
//     - Read the DAO fee configuration (§15.1)
//     - Check the ceiling escalation status (§13.3)
//
// ## Endpoints (§17, §17.2)
//
//   GET  /health                              — liveness check
//   GET  /api/v1/health                       — all 16 layer health states
//   GET  /api/v1/config                       — current OmegaConfig snapshot
//   POST /api/v1/config                       — hot-reload config (L1 fields)
//   GET  /api/v1/la/gas-model/checkpoints     — list all checkpoints
//   POST /api/v1/la/gas-model/revert/{ver}    — revert model to version
//   GET  /api/v1/la/gas-model/ceiling-status  — ceiling escalation state
//   POST /api/v1/la/gas-model/unpause         — clear pause after review
//   GET  /api/v1/vault/dao-fee                — DAO fee config
//   GET  /api/v1/builders/blacklist           — builder blacklist
//   POST /api/v1/builders/blacklist/update    — hot-reload blacklist
//   GET  /ws/events                           — realtime WsEvent stream (§17.1)
//
//   Plus the gRPC service on :50051 (see `grpc.rs`) — GetSystemHealth,
//   WatchHealth, GetPnL, GetLatency, GetQueueDepths, GetWinRates, and the
//   L2 command RPCs PauseStrategy / ResumeStrategy / ClearHalt /
//   AdjustRollout.
//
// ## Authentication
//
//   L1 (hot-reload config): Bearer token from environment.
//   L2 (model revert, unpause, blacklist update, gRPC commands): same
//     Bearer token; in production, callers additionally provide a
//     multisig signature that is verified off-band.  The control-plane
//     records the action in the audit log; signature verification is
//     the operator's responsibility at the API gateway.
//
// ## Rate limits (§17.1)
//
//   WebSocket connections (served by axum's ws feature) are rate-limited
//   to 300/min authenticated, 100/min anonymous.  HTTP API endpoints
//   are not rate-limited at this layer (handled by the reverse proxy).
//
// ## Observability bridge
//
//   `obs_bridge` polls the shared `EventRingBuffer` (populated by every
//   engine crate via `omega-observability`) and republishes mapped events
//   onto the same `ws_tx` broadcast channel used by the HTTP handlers
//   below, so dashboard clients connected to /ws/events see both
//   governance actions (config reload, blacklist update, ...) and live
//   trading telemetry (ProfitSplit, GasModelReverted, ...) on one stream.
//
// ## AppState / WsEvent — where they live
//
//   `AppState` lives in `state.rs` — see that file's own module doc for
//   the wiring model and the observability-bridge contract.
//
//   `WsEvent` is `omega_control_contracts::ws::WsEvent` — the real type
//   shared with the frontend dashboard, NOT a locally-defined one (see
//   state.rs's module-level FIX note for why an earlier local duplicate
//   was wrong and got removed). Both are re-exported at the crate root
//   below (`pub use state::AppState;` /
//   `pub use omega_control_contracts::ws::{WsEvent, WS_CHANNEL_CAPACITY};`)
//   so `crate::AppState`, `crate::WsEvent`, and `crate::WS_CHANNEL_CAPACITY`
//   all resolve for `grpc.rs` and `ws.rs`, which reference them that way
//   (`ws.rs`'s test module in particular does `use crate::WS_CHANNEL_CAPACITY;`).
//
// ## FIX (this revision): wire up state.rs, and fix the WsEvent split
//
//   Earlier revision: this file declared `mod grpc; mod obs_bridge; mod
//   ws;` but NOT `mod state;`, while defining its own duplicate, stale
//   inline `AppState` (using external `omega_control_contracts::ws::WsEvent`
//   but a `LayerId` variant set that didn't match the real enum).
//   `obs_bridge.rs`'s `use crate::state::{AppState, WsEvent};` failed
//   with E0432 (no `state` module). Fixed at the time by declaring `mod
//   state;` and re-exporting `state::{AppState, WsEvent}` — but
//   `state.rs`'s own `WsEvent` turned out to be a second, independently
//   wrong-shaped local enum (assumed `tag = "type", content = "payload"`;
//   the real crate uses `tag = "kind"`, flattened), which `grpc.rs` and
//   `ws.rs` never used — both already imported the real
//   `omega_control_contracts::ws::WsEvent` directly. That split meant
//   `AppState.ws_tx` (typed against the wrong local enum) and `grpc.rs`'s
//   pattern matches / `state.publish()` calls (built against the real
//   enum) were two genuinely different types.
//
//   Fixed this revision: `state.rs` no longer defines a local `WsEvent`
//   — `AppState.ws_tx` now broadcasts `omega_control_contracts::ws::WsEvent`
//   directly, and this file re-exports THAT type (plus its real
//   `WS_CHANNEL_CAPACITY`) instead of anything from `state.rs`. See
//   `state.rs` and `obs_bridge.rs`'s own module-level FIX notes for the
//   full detail.
//
// ## Audit fix (earlier revision): omega-control-contracts::rest shape changes
//
// Two REST contract types changed in an earlier audit pass (see that
// crate's own CHANGES notes in rest.rs):
//   1. `LayerHealthEntry.layer` -> `.layer_id` (matches
//      `proto::LayerHealth.layer_id` naming exactly), plus a new
//      required `reason: String` field this handler never populated.
//      `LayerHealthImpl` doesn't expose a reason getter here any more
//      than it does in `grpc.rs`'s equivalent `ProtoLayerHealth`
//      construction, so `reason: String::new()` is used for the same
//      reason `grpc.rs` already does it that way — nothing else in this
//      file has a source for that value.
//   2. `DaoFeeResponse.dao_fee_pct` is no longer a stored field — it's
//      now a method, `DaoFeeResponse::dao_fee_pct()`, computed on
//      demand instead of carried and trusted (see rest.rs's own note on
//      why). The struct literal below no longer sets it.
//   3. `VaultConfig::per_transfer_cap_wei`/`daily_cap_wei` are now
//      `WeiAmount`, not a raw integer (see config.rs's own module doc
//      comment on why a TOML-safe u128 wrapper was needed) — `as f64`
//      doesn't compile on a non-primitive newtype. Fixed via
//      `WeiAmount::as_wei()`, which returns the underlying `u128`.
//
// ## CLI
//
//   omega-control-plane \
//     --bind 0.0.0.0:8080 \
//     --config-path config/omega.toml \
//     --checkpoint-dir checkpoints \
//     --blacklist-path config/builder_blacklist.toml \
//     --api-token <token>

mod state;

mod grpc;
mod obs_bridge;
mod ws;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use tracing::Level;

use omega_control_contracts::rest::{
    ApiError, BlacklistResponse, ConfigReloadRequest, DaoFeeResponse, HealthSnapshot,
    LayerHealthEntry, RevertResponse, OK,
};
use omega_core::{LayerHealth, OmegaConfig, VaultConfig};
use omega_loss_attribution::checkpoint;
use omega_observability::{EventRingBuffer, DEFAULT_CAPACITY};

// AppState lives in state.rs; WsEvent/WS_CHANNEL_CAPACITY are the real,
// frontend-shared types from omega_control_contracts::ws, NOT anything
// local to this crate. See this file's module-level FIX note.
pub use state::AppState;
pub use omega_control_contracts::ws::{WsEvent, WS_CHANNEL_CAPACITY};
use state::load_config;

// ─────────────────────────────────────────────────────────────────────────────
// CLI
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "omega-control-plane",
    about = "Omega Engine HTTP control-plane API (§17)",
    version
)]
pub struct ControlPlaneArgs {
    /// TCP bind address.
    #[arg(long, default_value = "0.0.0.0:8080", env = "OMEGA_BIND")]
    pub bind: String,

    /// Path to the engine configuration TOML file.
    #[arg(long, default_value = "config/omega.toml", env = "OMEGA_CONFIG")]
    pub config_path: String,

    /// Gas model checkpoint directory (§13.2).
    ///
    /// Defaults to a relative `checkpoints` directory so the binary works
    /// on Windows without requiring /var/omega/checkpoints to exist.
    /// Override with --checkpoint-dir or OMEGA_CHECKPOINT_DIR if you want
    /// an absolute path in production.
    #[arg(long, default_value = "checkpoints", env = "OMEGA_CHECKPOINT_DIR")]
    pub checkpoint_dir: String,

    /// MEV-Boost builder blacklist TOML path (§12.3).
    #[arg(
        long,
        default_value = "config/builder_blacklist.toml",
        env = "OMEGA_BLACKLIST_PATH"
    )]
    pub blacklist_path: String,

    /// Bearer token for API authentication.
    /// Must match OMEGA_API_TOKEN baked into the WASM frontend at build time.
    #[arg(long, env = "OMEGA_API_TOKEN")]
    pub api_token: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// AppState construction from CLI args
// ─────────────────────────────────────────────────────────────────────────────

/// Builds `Arc<AppState>` from CLI args. Thin wrapper around
/// `state::AppState::new` — `ControlPlaneArgs` (clap-specific) lives in
/// this file, not state.rs, so state.rs's constructor takes plain,
/// already-resolved values instead of the CLI type directly.
fn build_app_state(args: &ControlPlaneArgs) -> Result<Arc<AppState>> {
    let config = load_config(&args.config_path)?;

    // Ensure checkpoint directory exists so list_checkpoints returns Ok([])
    // rather than an I/O error when no checkpoints have been written yet.
    let checkpoint_dir = PathBuf::from(&args.checkpoint_dir);
    if !checkpoint_dir.exists() {
        if let Err(e) = std::fs::create_dir_all(&checkpoint_dir) {
            tracing::warn!(
                path = %checkpoint_dir.display(),
                error = %e,
                "Could not create checkpoint directory — checkpoint list will be empty"
            );
        }
    }

    let obs_buffer = EventRingBuffer::new(DEFAULT_CAPACITY);

    AppState::new(
        config,
        PathBuf::from(&args.config_path),
        checkpoint_dir,
        PathBuf::from(&args.blacklist_path),
        args.api_token.clone(),
        obs_buffer,
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Authentication extractor
// ─────────────────────────────────────────────────────────────────────────────

fn check_auth(
    headers: &axum::http::HeaderMap,
    api_token: &str,
) -> std::result::Result<(), (StatusCode, Json<ApiError>)> {
    let provided = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    if provided != api_token {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiError {
                error: "UNAUTHORIZED".into(),
                message: "Invalid or missing Bearer token".into(),
            }),
        ))
    } else {
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /health — liveness check (no auth)
// ─────────────────────────────────────────────────────────────────────────────

async fn get_liveness() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })))
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /api/v1/health — all 16 layer health states (no auth)
// ─────────────────────────────────────────────────────────────────────────────

async fn get_health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let layers: Vec<LayerHealthEntry> = state
        .health_layers
        .iter()
        .map(|l| LayerHealthEntry {
            layer_id: l.layer_id().to_string(),
            state: l.state().to_string(),
            is_operational: l.is_operational(),
            // LayerHealthImpl exposes no reason getter here, same as
            // grpc.rs's equivalent ProtoLayerHealth construction — see
            // this file's module-level audit note.
            reason: String::new(),
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

// ─────────────────────────────────────────────────────────────────────────────
// GET /api/v1/config — current config snapshot
// ─────────────────────────────────────────────────────────────────────────────

async fn get_config(
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }
    let config = state.config.read().await.clone();
    (StatusCode::OK, Json(config)).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// POST /api/v1/config — hot-reload (L1 fields only)
// ─────────────────────────────────────────────────────────────────────────────

async fn post_config(
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(req): Json<ConfigReloadRequest>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }

    let new_config = if req.from_disk {
        match load_config(state.config_path.to_str().unwrap_or("")) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "Config reload from disk failed");
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ApiError {
                        error: "CONFIG_PARSE_ERROR".into(),
                        message: e.to_string(),
                    }),
                )
                    .into_response();
            }
        }
    } else {
        let body = match req.body {
            Some(b) => b,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ApiError {
                        error: "MISSING_BODY".into(),
                        message: "from_disk=false requires a config body".into(),
                    }),
                )
                    .into_response();
            }
        };
        match serde_json::from_value::<OmegaConfig>(body) {
            Ok(c) => c,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ApiError {
                        error: "CONFIG_PARSE_ERROR".into(),
                        message: e.to_string(),
                    }),
                )
                    .into_response();
            }
        }
    };

    let errors = new_config.validate();
    if !errors.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "CONFIG_VALIDATION_FAILED".into(),
                message: errors.join("; "),
            }),
        )
            .into_response();
    }

    *state.config.write().await = new_config;
    state.publish(WsEvent::ConfigReloaded {
        timestamp: chrono::Utc::now(),
    });
    tracing::info!("Config hot-reloaded successfully");

    (StatusCode::OK, Json(OK)).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /api/v1/la/gas-model/checkpoints (§17.2)
// ─────────────────────────────────────────────────────────────────────────────

async fn get_checkpoints(
    headers: axum::http::HeaderMap,
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
                Json(ApiError {
                    error: "CHECKPOINT_LIST_ERROR".into(),
                    message: e.to_string(),
                }),
            )
                .into_response()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// POST /api/v1/la/gas-model/revert/{version} (§17.2)
// ─────────────────────────────────────────────────────────────────────────────

async fn post_revert_model(
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
    Path(version): Path<u64>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }

    match checkpoint::load_version(&state.checkpoint_dir, version) {
        Ok(ckpt) => {
            tracing::warn!(
                version = ckpt.version,
                win_rate = ckpt.win_rate,
                sample_count = ckpt.sample_count,
                "Gas model reverted by governance (L2 fast-approve)",
            );
            (
                StatusCode::OK,
                Json(RevertResponse {
                    reverted_to_version: ckpt.version,
                    win_rate: ckpt.win_rate,
                    sample_count: ckpt.sample_count,
                }),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(version, error = %e, "Checkpoint revert failed");
            (
                StatusCode::NOT_FOUND,
                Json(ApiError {
                    error: "CHECKPOINT_NOT_FOUND".into(),
                    message: format!("Version {version}: {e}"),
                }),
            )
                .into_response()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /api/v1/la/gas-model/ceiling-status (§17.2)
// ─────────────────────────────────────────────────────────────────────────────

async fn get_ceiling_status(
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }

    let paused = state
        .model_paused
        .load(std::sync::atomic::Ordering::Acquire);
    let tracker = state.ceiling_tracker.read().await;
    let snapshot = tracker.snapshot(paused);

    (StatusCode::OK, Json(snapshot)).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// POST /api/v1/la/gas-model/unpause (§17.2)
// ─────────────────────────────────────────────────────────────────────────────

async fn post_unpause_model(
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }

    state
        .model_paused
        .store(false, std::sync::atomic::Ordering::SeqCst);
    state.ceiling_tracker.write().await.record_unpause();
    state.publish(WsEvent::ModelPauseChanged {
        paused: false,
        timestamp: chrono::Utc::now(),
    });

    tracing::warn!("Gas model unpaused by governance (L2 fast-approve)");

    (StatusCode::OK, Json(OK)).into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /api/v1/vault/dao-fee (§15.1)
// ─────────────────────────────────────────────────────────────────────────────

async fn get_dao_fee(
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }

    let vault: VaultConfig = state.config.read().await.vault.clone();

    (
        StatusCode::OK,
        Json(DaoFeeResponse {
            dao_fee_bps: vault.dao_fee_bps,
            // dao_fee_pct is no longer a field on DaoFeeResponse — it's
            // now DaoFeeResponse::dao_fee_pct(), computed on demand by
            // the caller. See this file's module-level audit note.
            //
            // WeiAmount::as_wei() -> u128, then cast to f64 — a plain
            // `as f64` on WeiAmount itself doesn't compile, since it's
            // a non-primitive newtype (see config.rs's own module doc
            // comment on why per_transfer_cap_wei/daily_cap_wei are
            // WeiAmount rather than a raw integer).
            per_transfer_cap_eth: vault.per_transfer_cap_wei.as_wei() as f64 / 1e18,
            daily_cap_eth: vault.daily_cap_wei.as_wei() as f64 / 1e18,
            confirmation_depth: vault.confirmation_depth,
        }),
    )
        .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /api/v1/builders/blacklist (§12.3)
// ─────────────────────────────────────────────────────────────────────────────

async fn get_blacklist(
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }

    (
        StatusCode::OK,
        Json(BlacklistResponse {
            entry_count: state.blacklist.len(),
            path: state.blacklist.path().display().to_string(),
            is_empty: state.blacklist.is_empty(),
        }),
    )
        .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// POST /api/v1/builders/blacklist/update (§12.3, L2)
// ─────────────────────────────────────────────────────────────────────────────

async fn post_blacklist_update(
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&headers, &state.api_token) {
        return e.into_response();
    }

    match state.blacklist.reload() {
        Ok(()) => {
            let entry_count = state.blacklist.len();
            state.publish(WsEvent::BlacklistReloaded {
                entry_count,
                timestamp: chrono::Utc::now(),
            });
            tracing::info!(
                entry_count,
                "Builder blacklist hot-reloaded (L2 fast-approve)",
            );
            (StatusCode::OK, Json(OK)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Builder blacklist reload failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError {
                    error: "BLACKLIST_RELOAD_ERROR".into(),
                    message: e.to_string(),
                }),
            )
                .into_response()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Router
// ─────────────────────────────────────────────────────────────────────────────

fn build_router(state: Arc<AppState>) -> Router {
    use tower_http::cors::CorsLayer;
    use tower_http::trace::TraceLayer;

    Router::new()
        // Liveness — no auth required
        .route("/health", get(get_liveness))
        // Health state — no auth required so the dashboard can always
        // show layer status even before the user token is set
        .route("/api/v1/health", get(get_health))
        // Config
        .route("/api/v1/config", get(get_config))
        .route("/api/v1/config", post(post_config))
        // Gas model
        .route("/api/v1/la/gas-model/checkpoints", get(get_checkpoints))
        .route(
            "/api/v1/la/gas-model/revert/:version",
            post(post_revert_model),
        )
        .route(
            "/api/v1/la/gas-model/ceiling-status",
            get(get_ceiling_status),
        )
        .route("/api/v1/la/gas-model/unpause", post(post_unpause_model))
        // Vault
        .route("/api/v1/vault/dao-fee", get(get_dao_fee))
        // Builder blacklist
        .route("/api/v1/builders/blacklist", get(get_blacklist))
        .route(
            "/api/v1/builders/blacklist/update",
            post(post_blacklist_update),
        )
        // Realtime event stream (§17.1)
        .route("/ws/events", get(ws::events_handler))
        // CORS — permissive so the Trunk dev server (127.0.0.1:8082) can
        // call this API (127.0.0.1:8080) cross-origin.  Tighten to a
        // specific origin before any non-localhost deployment.
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

// ─────────────────────────────────────────────────────────────────────────────
// Main
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(true)
        .json()
        .init();

    let args = ControlPlaneArgs::parse();
    let state = build_app_state(&args)?;

    let addr: std::net::SocketAddr = args
        .bind
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid bind address '{}': {e}", args.bind))?;

    tracing::info!(
        bind           = %addr,
        config_path    = %args.config_path,
        checkpoint_dir = %args.checkpoint_dir,
        blacklist_path = %args.blacklist_path,
        "Control-plane starting",
    );

    obs_bridge::spawn(Arc::clone(&state));

    // gRPC on :50051 as a background task — failure is logged but does not
    // bring down the HTTP server, so the dashboard remains usable even if
    // another process holds the gRPC port.
    {
        let grpc_state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(e) = grpc::serve(grpc_state).await {
                tracing::error!(error = %e, "gRPC server exited with error");
            }
        });
    }

    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!(bind = %addr, "Control-plane listening");

    axum::serve(listener, router)
        .await
        .map_err(|e| anyhow::anyhow!("Server error: {e}"))?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header::AUTHORIZATION, HeaderMap, HeaderValue};

    fn auth_headers(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        h
    }

    #[test]
    fn check_auth_passes_with_correct_token() {
        let h = auth_headers("secret");
        assert!(check_auth(&h, "secret").is_ok());
    }

    #[test]
    fn check_auth_fails_with_wrong_token() {
        let h = auth_headers("wrong");
        assert!(check_auth(&h, "secret").is_err());
    }

    #[test]
    fn check_auth_fails_with_missing_header() {
        let h = HeaderMap::new();
        assert!(check_auth(&h, "secret").is_err());
    }

    #[test]
    fn check_auth_fails_without_bearer_prefix() {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_static("secret"));
        assert!(check_auth(&h, "secret").is_err());
    }

    #[test]
    fn load_config_missing_file_returns_defaults() {
        let cfg = load_config("/tmp/omega_nonexistent_xyz.toml").unwrap();
        assert_eq!(cfg.active_phase, 0);
    }

    #[test]
    fn load_config_invalid_toml_returns_error() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "not valid toml {{{{").unwrap();
        assert!(load_config(tmp.path().to_str().unwrap()).is_err());
    }

    #[test]
    fn load_config_valid_toml() {
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

    #[tokio::test]
    async fn router_liveness_returns_200() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let tmp_blacklist = tempfile::NamedTempFile::new().unwrap();
        let tmp_config = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp_config.path(), "").unwrap();

        let args = ControlPlaneArgs {
            bind: "127.0.0.1:0".into(),
            config_path: tmp_config.path().to_str().unwrap().into(),
            checkpoint_dir: std::env::temp_dir().to_str().unwrap().into(),
            blacklist_path: tmp_blacklist.path().to_str().unwrap().into(),
            api_token: "test-token".into(),
        };
        let state = build_app_state(&args).unwrap();
        let router = build_router(state);

        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn router_health_returns_all_layers() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let tmp_blacklist = tempfile::NamedTempFile::new().unwrap();
        let tmp_config = tempfile::NamedTempFile::new().unwrap();

        let args = ControlPlaneArgs {
            bind: "127.0.0.1:0".into(),
            config_path: tmp_config.path().to_str().unwrap().into(),
            checkpoint_dir: std::env::temp_dir().to_str().unwrap().into(),
            blacklist_path: tmp_blacklist.path().to_str().unwrap().into(),
            api_token: "test-token".into(),
        };
        let state = build_app_state(&args).unwrap();
        let router = build_router(state);

        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let snap: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let layers = snap["layers"].as_array().unwrap();
        assert!(
            layers.len() >= 14,
            "expected at least 14 layers, got {}",
            layers.len()
        );
    }

    #[tokio::test]
    async fn app_state_layer_lookup_finds_every_layer() {
        let tmp_blacklist = tempfile::NamedTempFile::new().unwrap();
        let tmp_config = tempfile::NamedTempFile::new().unwrap();

        let args = ControlPlaneArgs {
            bind: "127.0.0.1:0".into(),
            config_path: tmp_config.path().to_str().unwrap().into(),
            checkpoint_dir: std::env::temp_dir().to_str().unwrap().into(),
            blacklist_path: tmp_blacklist.path().to_str().unwrap().into(),
            api_token: "test-token".into(),
        };
        let state = build_app_state(&args).unwrap();

        for l in &state.health_layers {
            assert!(state.layer(l.layer_id()).is_some());
        }
    }

    #[tokio::test]
    async fn app_state_publish_reaches_subscriber() {
        let tmp_blacklist = tempfile::NamedTempFile::new().unwrap();
        let tmp_config = tempfile::NamedTempFile::new().unwrap();

        let args = ControlPlaneArgs {
            bind: "127.0.0.1:0".into(),
            config_path: tmp_config.path().to_str().unwrap().into(),
            checkpoint_dir: std::env::temp_dir().to_str().unwrap().into(),
            blacklist_path: tmp_blacklist.path().to_str().unwrap().into(),
            api_token: "test-token".into(),
        };
        let state = build_app_state(&args).unwrap();

        let mut rx = state.subscribe_ws();
        state.publish(WsEvent::ConfigReloaded {
            timestamp: chrono::Utc::now(),
        });

        let received = rx.try_recv();
        assert!(
            received.is_ok(),
            "publish() must reach an active subscriber"
        );
    }
}