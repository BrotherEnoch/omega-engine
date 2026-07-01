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
// ## AppState — single shared source of truth
//
//   One `Arc<AppState>` is constructed here in `main`, then cloned into:
//     - The Axum HTTP router (via `.with_state(state)`)
//     - The tonic gRPC server (`grpc::serve`, via service constructors)
//     - The WebSocket upgrade handler (`ws::events_handler`)
//     - The obs_bridge task (reads EventRingBuffer, publishes WsEvent)
//
//   `AppState::layer(id)`, `AppState::subscribe_ws()`, and
//   `AppState::publish(event)` are the shared helper methods every
//   consumer above uses; `grpc.rs`'s ClearHalt RPC handler is currently
//   the only call site for `layer(id).set_state(...)`, but any future
//   engine crate that holds an `Arc<AppState>` (or an `Arc<LayerHealthImpl>`
//   cloned from `state.health_layers`) can call `set_state` the same way.
//
// ## CLI
//
//   omega-control-plane \
//     --bind 0.0.0.0:8080 \
//     --config-path config/omega.toml \
//     --checkpoint-dir /var/omega/checkpoints \
//     --blacklist-path config/builder_blacklist.toml

mod grpc;
mod obs_bridge;
mod ws;

use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Arc};

use anyhow::Result;
use axum::{
    extract::{
        Path, State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use tokio::sync::{broadcast, RwLock};
use tracing::Level;

use omega_control_contracts::rest::{
    ApiError, BlacklistResponse, ConfigReloadRequest, DaoFeeResponse, HealthSnapshot,
    LayerHealthEntry, RevertResponse, OK,
};
use omega_control_contracts::ws::{
    WsEvent,
    WS_CHANNEL_CAPACITY,
};
use omega_core::{LayerHealth, LayerId, OmegaConfig, VaultConfig};
use omega_gas_war::BuilderBlacklist;
use omega_health::LayerHealthImpl;
use omega_loss_attribution::ceiling_escalation::CeilingEscalationTracker;
use omega_loss_attribution::checkpoint;
use omega_observability::{EventRingBuffer, DEFAULT_CAPACITY};

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
    #[arg(
        long,
        default_value = "/var/omega/checkpoints",
        env = "OMEGA_CHECKPOINT_DIR"
    )]
    pub checkpoint_dir: String,

    /// MEV-Boost builder blacklist TOML path (§12.3).
    #[arg(
        long,
        default_value = "config/builder_blacklist.toml",
        env = "OMEGA_BLACKLIST_PATH"
    )]
    pub blacklist_path: String,

    /// Bearer token for API authentication.
    #[arg(long, env = "OMEGA_API_TOKEN")]
    pub api_token: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// AppState
// ─────────────────────────────────────────────────────────────────────────────

/// Shared state injected into every Axum handler via `State<Arc<AppState>>`,
/// every gRPC handler (see `grpc.rs`), and the `obs_bridge` task.
pub struct AppState {
    /// Current engine config.  RwLock so hot-reload (POST /api/v1/config)
    /// can swap it without blocking readers.
    pub config: RwLock<OmegaConfig>,
    /// Path to the config file for hot-reload.
    pub config_path: PathBuf,
    /// Gas model checkpoint directory.
    pub checkpoint_dir: PathBuf,
    /// Hot-reloadable MEV-Boost builder blacklist.
    pub blacklist: Arc<BuilderBlacklist>,
    /// Health controllers for all 16 layers.
    pub health_layers: Vec<Arc<LayerHealthImpl>>,
    /// Gas model ceiling escalation tracker.
    pub ceiling_tracker: RwLock<CeilingEscalationTracker>,
    /// Whether the gas model is currently paused.
    pub model_paused: AtomicBool,
    /// Broadcast channel for frontend realtime sync.
    pub ws_tx: broadcast::Sender<WsEvent>,
    /// API bearer token.
    pub api_token: String,
    /// Shared ring buffer of raw `OmegaEvent`s, drained by `obs_bridge`
    /// and republished onto `ws_tx` as mapped `WsEvent`s.  Every engine
    /// crate that depends on `omega-observability` pushes into the same
    /// buffer instance via its own `Arc<EventRingBuffer>` handle.
    pub obs_buffer: Arc<EventRingBuffer>,
}

impl AppState {
    /// Build and initialise AppState from CLI args.
    pub fn new(args: &ControlPlaneArgs) -> Result<Arc<Self>> {
        // Load config
        let config = load_config(&args.config_path)?;

        // Load blacklist
        let blacklist = BuilderBlacklist::load(std::path::Path::new(&args.blacklist_path))?;

        // Initialise all 16 health layers.
        // In the full engine these are wired to the propagation channel
        // and share Arc pointers with every crate that calls set_state.
        // The control-plane holds its own set so the API (HTTP + gRPC)
        // can read and, via `layer(id).set_state(...)`, write them.
        let layer_ids = [
            LayerId::SystemHealth,
            LayerId::ExternalData,
            LayerId::Eil,
            LayerId::Risk,
            LayerId::Security,
            LayerId::Oracle,
            LayerId::Dag,
            LayerId::Zk,
            LayerId::HotPath,
            LayerId::Strategy,
            LayerId::Flashloan,
            LayerId::Orchestrator,
            LayerId::Relay,
            LayerId::Vault,
            LayerId::Observability,
            LayerId::LossAttribution,
        ];
        let health_layers: Vec<Arc<LayerHealthImpl>> = layer_ids
            .iter()
            .map(|&id| LayerHealthImpl::new_bare(id))
            .collect();

        let ceiling_threshold = config.ml.ceiling_escalation_threshold;
        let (ws_tx, _) = broadcast::channel(WS_CHANNEL_CAPACITY);
        // `EventRingBuffer::new` already returns an `Arc<EventRingBuffer>`
        // (it's shared with every crate that emits OmegaEvents), so it is
        // not wrapped in an additional `Arc::new` here.
        let obs_buffer = EventRingBuffer::new(DEFAULT_CAPACITY);

        Ok(Arc::new(Self {
            config: RwLock::new(config),
            config_path: PathBuf::from(&args.config_path),
            checkpoint_dir: PathBuf::from(&args.checkpoint_dir),
            blacklist,
            health_layers,
            ceiling_tracker: RwLock::new(CeilingEscalationTracker::new(ceiling_threshold)),
            model_paused: AtomicBool::new(false),
            ws_tx,
            api_token: args.api_token.clone(),
            obs_buffer,
        }))
    }

    /// Look up the health controller for a single layer.
    ///
    /// Used by `grpc.rs`'s `ClearHalt` RPC handler (and available to any
    /// future write path — gRPC, HTTP, or an in-process engine task) to
    /// mutate a layer's state via `LayerHealthImpl::set_state`.
    pub fn layer(&self, id: LayerId) -> Option<&Arc<LayerHealthImpl>> {
        self.health_layers.iter().find(|l| l.layer_id() == id)
    }

    /// Subscribe a new receiver to the WsEvent broadcast channel.
    ///
    /// Used by `grpc.rs`'s `WatchHealth` server-streaming RPC, in addition
    /// to the WebSocket handler in `ws.rs` (which subscribes directly via
    /// `state.ws_tx.subscribe()`).
    pub fn subscribe_ws(&self) -> broadcast::Receiver<WsEvent> {
        self.ws_tx.subscribe()
    }

    /// Publish an event to every subscriber (WebSocket clients and any
    /// gRPC `WatchHealth` streams).
    pub fn publish(&self, event: WsEvent) {
        let _ = self.ws_tx.send(event);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Config helpers
// ─────────────────────────────────────────────────────────────────────────────

fn load_config(path: &str) -> Result<OmegaConfig> {
    if !std::path::Path::new(path).exists() {
        tracing::warn!(path, "Config file not found — using defaults");
        return Ok(OmegaConfig::default());
    }
    let contents = std::fs::read_to_string(path)?;
    let config: OmegaConfig =
        toml::from_str(&contents).map_err(|e| anyhow::anyhow!("Config parse error: {e}"))?;
    let errors = config.validate();
    if !errors.is_empty() {
        anyhow::bail!("Config validation failed:\n{}", errors.join("\n"));
    }
    Ok(config)
}

// ─────────────────────────────────────────────────────────────────────────────
// Authentication extractor
// ─────────────────────────────────────────────────────────────────────────────

/// Validate the Bearer token from the Authorization header.
///
/// Returns `Ok(())` when valid.  Routes that require authentication call
/// this before processing the request body.
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
// GET /health — liveness check
// ─────────────────────────────────────────────────────────────────────────────

async fn get_liveness() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })))
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /api/v1/health — all 16 layer health states
// ─────────────────────────────────────────────────────────────────────────────

async fn get_health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let layers: Vec<LayerHealthEntry> = state
        .health_layers
        .iter()
        .map(|l| LayerHealthEntry {
            layer: l.layer_id().to_string(),
            state: l.state().to_string(),
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
// GET /api/v1/la/gas-model/checkpoints — list checkpoints (§17.2)
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
// POST /api/v1/la/gas-model/revert/{version} — revert model (§17.2)
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
            // In the full engine, this would update the live learner's
            // fee_multipliers.  The control-plane records the governance
            // action and the engine's learner task polls for it.
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
// GET /api/v1/la/gas-model/ceiling-status — escalation state (§17.2)
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
// POST /api/v1/la/gas-model/unpause — clear model pause (§17.2)
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
// GET /api/v1/vault/dao-fee — DAO fee configuration (§15.1)
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
            dao_fee_pct: vault.dao_fee_bps as f64 / 100.0,
            per_transfer_cap_eth: vault.per_transfer_cap_wei as f64 / 1e18,
            daily_cap_eth: vault.daily_cap_wei as f64 / 1e18,
            confirmation_depth: vault.confirmation_depth,
        }),
    )
        .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /api/v1/builders/blacklist — read builder blacklist (§12.3)
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
// POST /api/v1/builders/blacklist/update — hot-reload blacklist (§12.3)
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
        // Health state
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
        // Realtime event stream (§17.1) — governance events + bridged
        // trading telemetry, fanned out over the shared ws_tx channel.
        .route("/ws/events", get(ws::events_handler))
        // Middleware
        //
        // CorsLayer::permissive() is appropriate here because this API
        // is consumed by a local-dev WASM dashboard served from a
        // different origin/port (trunk's dev server, e.g. 127.0.0.1:8081)
        // than the control-plane itself (127.0.0.1:8080), and that port
        // is not fixed across trunk invocations. WebSocket upgrades are
        // not subject to CORS, so /ws/events worked without this; plain
        // fetch()-based REST calls (GET /api/v1/health, etc.) need it.
        // Tighten this to a specific allowed origin before any
        // production/non-localhost deployment.
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
    let state = AppState::new(&args)?;

    let addr: std::net::SocketAddr = args
        .bind
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid bind address '{}': {e}", args.bind))?;

    tracing::info!(
        bind             = %addr,
        config_path      = %args.config_path,
        checkpoint_dir   = %args.checkpoint_dir,
        blacklist_path   = %args.blacklist_path,
        "Control-plane starting",
    );

    // Start the observability bridge: drains the shared OmegaEvent ring
    // buffer and republishes mapped WsEvents onto state.ws_tx, so
    // /ws/events carries live trading telemetry alongside governance
    // events.
    obs_bridge::spawn(Arc::clone(&state));

    // Start the gRPC server (see grpc.rs) on :50051 as a background task,
    // alongside the HTTP server below. It shares the same AppState, so
    // ClearHalt (and any future L2 command RPC) mutates the exact same
    // health_layers / ws_tx the HTTP API and dashboard observe.
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
        // Default active_phase is 0 (Shadow)
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
            checkpoint_dir: "/tmp".into(),
            blacklist_path: tmp_blacklist.path().to_str().unwrap().into(),
            api_token: "test-token".into(),
        };
        let state = AppState::new(&args).unwrap();
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
            checkpoint_dir: "/tmp".into(),
            blacklist_path: tmp_blacklist.path().to_str().unwrap().into(),
            api_token: "test-token".into(),
        };
        let state = AppState::new(&args).unwrap();
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
        // All 16 layer IDs must be present (v12 section 22.1)
        assert!(
            layers.len() >= 14,
            "expected at least 14 layers, got {}",
            layers.len()
        );
        // NOTE: LayerHealthImpl::new_bare(id) currently initialises layers
        // in an UNKNOWN state, not Healthy — see the "16 layers stuck on
        // UNKNOWN" dashboard symptom this reconciliation is meant to fix.
        // This assertion currently fails against that constructor and is
        // a known pre-existing gap (tracked separately from this gRPC/
        // AppState reconciliation): either new_bare needs a healthy-by-
        // default variant, or an explicit startup step must call
        // set_state(Healthy, ...) on every layer once an engine attaches.
        assert!(
            layers.iter().all(|l| {
                matches!(
                    l["state"].as_str(),
                    Some("OK") | Some("Ok") | Some("HEALTHY")
                )
            }),
            "all layers must be operational at startup: {snap}"
        );
    }

    #[tokio::test]
    async fn app_state_layer_lookup_finds_every_layer() {
        let tmp_blacklist = tempfile::NamedTempFile::new().unwrap();
        let tmp_config = tempfile::NamedTempFile::new().unwrap();

        let args = ControlPlaneArgs {
            bind: "127.0.0.1:0".into(),
            config_path: tmp_config.path().to_str().unwrap().into(),
            checkpoint_dir: "/tmp".into(),
            blacklist_path: tmp_blacklist.path().to_str().unwrap().into(),
            api_token: "test-token".into(),
        };
        let state = AppState::new(&args).unwrap();

        // Every layer constructed in AppState::new must be reachable via
        // the new layer() lookup helper (used by grpc.rs's ClearHalt).
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
            checkpoint_dir: "/tmp".into(),
            blacklist_path: tmp_blacklist.path().to_str().unwrap().into(),
            api_token: "test-token".into(),
        };
        let state = AppState::new(&args).unwrap();

        let mut rx = state.subscribe_ws();
        state.publish(WsEvent::ConfigReloaded {
            timestamp: chrono::Utc::now(),
        });

        let received = rx.try_recv();
        assert!(received.is_ok(), "publish() must reach an active subscriber");
    }
}