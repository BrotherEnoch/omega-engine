// ops/control-plane/src/state.rs
//
// AppState — single shared state for the HTTP server, gRPC server, and
// WebSocket broadcaster.
//
// ## Wiring model
//
//   One `Arc<AppState>` is constructed in `main`, then cloned into:
//     - The Axum HTTP router (via `.with_state(state)`)
//     - The tonic gRPC server (via service constructors)
//     - The WebSocket upgrade handler
//
// ## WsEvent broadcast
//
//   `ws_tx` is a tokio broadcast sender (capacity WS_CHANNEL_CAPACITY).
//   Producers (HTTP handlers on write paths, future health monitors) call
//   `state.publish(event)`.  Each WebSocket connection receives an
//   independent receiver via `state.subscribe_ws()` and fans events out
//   to its client with the appropriate rate limit.
//
// ## Config hot-reload
//
//   `load_config` is a free function so both `AppState::new` and the
//   `POST /api/v1/config` handler can call it without owning AppState.

use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Arc};

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::{broadcast, RwLock};

use omega_core::{LayerHealth, LayerId, OmegaConfig};
use omega_gas_war::BuilderBlacklist;
use omega_health::LayerHealthImpl;
use omega_loss_attribution::ceiling_escalation::CeilingEscalationTracker;

// ─────────────────────────────────────────────────────────────────────────────
// WsEvent
// ─────────────────────────────────────────────────────────────────────────────

/// Structured event streamed to WebSocket clients (§17.1).
///
/// Every variant is serialised with a `kind` discriminant field so
/// clients can dispatch without inspecting nested fields.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WsEvent {
    /// A layer's health state transitioned (§3).
    HealthTransition {
        layer:     String,
        from:      String,
        to:        String,
        reason:    String,
        timestamp: DateTime<Utc>,
    },
    /// The gas model pause state changed (§13.3).
    ModelPauseChanged {
        paused:    bool,
        timestamp: DateTime<Utc>,
    },
    /// The builder blacklist was hot-reloaded (§12.3).
    BlacklistReloaded {
        entry_count: usize,
        timestamp:   DateTime<Utc>,
    },
    /// The config was hot-reloaded (§5, L1).
    ConfigReloaded {
        timestamp: DateTime<Utc>,
    },
    /// Snapshot of the gas model ceiling escalation state (§13.3).
    CeilingEscalation {
        consecutive_hits: u64,
        paused:           bool,
        timestamp:        DateTime<Utc>,
    },
}

/// Broadcast channel capacity for WebSocket events.
///
/// Slow clients that fall more than `WS_CHANNEL_CAPACITY` events behind
/// receive a `RecvError::Lagged` and a lag-detected error frame.
pub const WS_CHANNEL_CAPACITY: usize = 512;

// ─────────────────────────────────────────────────────────────────────────────
// All LayerIds in canonical report order
// ─────────────────────────────────────────────────────────────────────────────

pub const ALL_LAYER_IDS: &[LayerId] = &[
    LayerId::SystemHealth, LayerId::ExternalData, LayerId::Eil,
    LayerId::Risk,         LayerId::Security,     LayerId::ChaosGuard,
    LayerId::Dag,          LayerId::Zk,           LayerId::HotPath,
    LayerId::Strategy,     LayerId::Flashloan,    LayerId::Orchestrator,
    LayerId::Relay,        LayerId::Vault,        LayerId::Observability,
    LayerId::LossAttribution,
];

// ─────────────────────────────────────────────────────────────────────────────
// AppState
// ─────────────────────────────────────────────────────────────────────────────

pub struct AppState {
    // ── Config ────────────────────────────────────────────────────────────
    /// Live engine config; hot-reloaded by `POST /api/v1/config`.
    pub config:          RwLock<OmegaConfig>,
    /// Path to the config TOML file for disk hot-reload.
    pub config_path:     PathBuf,

    // ── Gas model ─────────────────────────────────────────────────────────
    /// Gas model checkpoint directory (§13.2).
    pub checkpoint_dir:  PathBuf,
    /// Whether the gas model is paused due to ceiling escalation (§13.3).
    pub model_paused:    AtomicBool,
    /// Ceiling escalation state for the ceiling-status API (§17.2).
    pub ceiling_tracker: RwLock<CeilingEscalationTracker>,

    // ── Builder blacklist ─────────────────────────────────────────────────
    /// Hot-reloadable MEV-Boost builder blacklist (§12.3).
    pub blacklist:       Arc<BuilderBlacklist>,

    // ── Health ────────────────────────────────────────────────────────────
    /// Health controllers for all 16 layer variants.
    /// Shared with the propagation orchestrator — reads reflect live state.
    pub health_layers:   Vec<Arc<LayerHealthImpl>>,

    // ── WebSocket broadcast ────────────────────────────────────────────────
    /// Sender half of the WsEvent broadcast channel.
    /// Handlers call `state.publish(event)`.
    /// WebSocket connections subscribe via `state.subscribe_ws()`.
    pub ws_tx:           broadcast::Sender<WsEvent>,

    // ── Authentication ────────────────────────────────────────────────────
    /// Bearer token required for all authenticated endpoints.
    pub api_token:       String,
}

impl AppState {
    /// Construct `AppState` from already-loaded config and path arguments.
    pub fn new(
        config:         OmegaConfig,
        config_path:    PathBuf,
        checkpoint_dir: PathBuf,
        blacklist_path: PathBuf,
        api_token:      String,
    ) -> anyhow::Result<Arc<Self>> {
        let blacklist = BuilderBlacklist::load(&blacklist_path)?;

        let health_layers: Vec<Arc<LayerHealthImpl>> = ALL_LAYER_IDS
            .iter()
            .map(|&id| LayerHealthImpl::new_bare(id))
            .collect();

        let ceiling_threshold = config.ml.ceiling_escalation_threshold;
        let (ws_tx, _)        = broadcast::channel(WS_CHANNEL_CAPACITY);

        Ok(Arc::new(Self {
            config:          RwLock::new(config),
            config_path,
            checkpoint_dir,
            model_paused:    AtomicBool::new(false),
            ceiling_tracker: RwLock::new(CeilingEscalationTracker::new(ceiling_threshold)),
            blacklist,
            health_layers,
            ws_tx,
            api_token,
        }))
    }

    /// Find the health controller for a specific layer ID.
    pub fn layer(&self, id: LayerId) -> Option<&Arc<LayerHealthImpl>> {
        self.health_layers.iter().find(|l| l.layer_id() == id)
    }

    /// Subscribe to WebSocket events.  Used by `ws::events_handler`.
    pub fn subscribe_ws(&self) -> broadcast::Receiver<WsEvent> {
        self.ws_tx.subscribe()
    }

    /// Publish a WebSocket event.
    ///
    /// `send` returns the number of active receivers.  Zero receivers
    /// is not an error — the system may have no connected WS clients.
    pub fn publish(&self, event: WsEvent) {
        let _ = self.ws_tx.send(event);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Config helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Load and validate `OmegaConfig` from a TOML file.
///
/// Returns `OmegaConfig::default()` when the file does not exist —
/// valid for development deployments.  Returns `Err` on parse or
/// validation failure.
pub fn load_config(path: &str) -> anyhow::Result<OmegaConfig> {
    if !std::path::Path::new(path).exists() {
        tracing::warn!(path, "Config file not found — using defaults");
        return Ok(OmegaConfig::default());
    }
    let contents = std::fs::read_to_string(path)?;
    let config: OmegaConfig = toml::from_str(&contents)
        .map_err(|e| anyhow::anyhow!("Config parse error in '{path}': {e}"))?;
    let errors = config.validate();
    if !errors.is_empty() {
        anyhow::bail!("Config validation failed:\n{}", errors.join("\n"));
    }
    Ok(config)
}
