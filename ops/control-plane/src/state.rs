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
//     - The obs_bridge task (reads EventRingBuffer, publishes WsEvent)
//
// ## WsEvent broadcast
//
//   `ws_tx` is a tokio broadcast sender (capacity WS_CHANNEL_CAPACITY).
//   Producers call `state.publish(event)`.  Each WebSocket connection
//   subscribes via `state.subscribe_ws()`.
//
//   The `WsEvent` enum is serialised with `#[serde(tag = "type",
//   content = "payload")]` so that the frontend's
//   `omega-control-contracts::ws::WsEvent` (which uses the identical
//   attribute) can deserialise every variant without a custom handler.
//   Wire format: `{"type":"profit_split","payload":{…}}`.
//
// ## Observability bridge
//
//   `obs_buffer` is an `Arc<EventRingBuffer>` shared with the
//   `omega-observability` exporter.  The `obs_bridge` task (spawned in
//   `main`) drains this buffer on a 50ms tick, converts each `OmegaEvent`
//   to the corresponding `WsEvent` variant, and publishes it.
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
use omega_observability::EventRingBuffer;

// ── WsEvent ───────────────────────────────────────────────────────────────────

/// Structured event streamed to WebSocket clients (§17.1).
///
/// Serialised as `{"type":"<snake_case_variant>","payload":{…}}` to match
/// the frontend's `omega-control-contracts::ws::WsEvent` which uses the
/// identical `#[serde(tag = "type", content = "payload", rename_all =
/// "snake_case")]` attribute.  Both sides must agree on this shape for
/// `serde_json::from_str::<WsEvent>` in `ws_client.rs`'s `other =>`
/// branch to succeed.
///
/// Field types must also match the frontend contracts exactly:
///   - `emergency_fee_gwei: f64`  — obs_bridge casts the u64 OmegaEvent
///     field to f64 before constructing this variant; the frontend's
///     `EmergencyBundleSkippedEvent` declares it as u64, so the JSON
///     value must be an integer-valued f64 (e.g. `9999.0` serialises as
///     `9999` in serde_json, which deserialises cleanly into u64).
///
/// Variants not present in the frontend's WsEvent enum
/// (`HealthTransition`, `ModelPauseChanged`, `BlacklistReloaded`,
/// `ConfigReloaded`, `CeilingEscalation`) arrive in `ws_client.rs`'s
/// `other =>` branch, fail `from_str::<WsEvent>`, and are logged at WARN
/// then silently dropped — this is intentional; they are control-plane
/// governance events, not trading telemetry, and the frontend has no
/// display path for them.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum WsEvent {
    // ── Control-plane governance events ──────────────────────────────────────
    // These do not appear in the frontend's WsEvent enum. They arrive in
    // ws_client.rs's `other =>` branch and are dropped after a WARN log.
    // That is correct behaviour — the dashboard has no panel for them.

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

    // ── Trading engine telemetry ──────────────────────────────────────────────
    // These variants match the frontend's WsEvent enum exactly (same variant
    // names in snake_case, same field names, same field types). The frontend's
    // `record_obs_event` routes each one into the ObservabilityLog ring buffer
    // which drives the telemetry panel counters and event stream.

    /// Gas model reverted to a checkpoint after holdout degradation (§13, §16).
    GasModelReverted {
        checkpoint_version: u64,
        win_rate:           f64,
        sample_count:       u64,
        timestamp:          DateTime<Utc>,
    },
    /// Gas model ceiling escalation — model paused (§13.3, §16).
    GasModelCeilingEscalation {
        feature_key:       String,
        ceiling_hit_count: u64,
        timestamp:         DateTime<Utc>,
    },
    /// Emergency bundle skipped — profit check failed (§12.1, §16).
    ///
    /// `emergency_fee_gwei` is f64 here because `obs_bridge` converts the
    /// u64 `OmegaEvent` field via `as f64`.  serde_json serialises integer-
    /// valued f64s without a decimal point (e.g. `9999.0` → `9999`), so the
    /// frontend's `EmergencyBundleSkippedEvent { emergency_fee_gwei: u64 }`
    /// deserialises it correctly.
    EmergencyBundleSkipped {
        blueprint_hash:     String,
        emergency_fee_gwei: f64,
        reason:             String,
        timestamp:          DateTime<Utc>,
    },
    /// Profit released from Vault with DAO fee split (§15.1, §16).
    ProfitSplit {
        blueprint_hash: String,
        pil_share_wei:  String,
        dao_fee_wei:    String,
        timestamp:      DateTime<Utc>,
    },
    /// Sequencer reorg risk detected on a submitted blueprint (§11.4, §16).
    LaReorgRisk {
        tx_hash:        String,
        orphaned_block: u64,
        reorg_depth:    u32,
        timestamp:      DateTime<Utc>,
    },
    /// Simulation discrepancy detected (§16).
    SimulationError {
        blueprint_hash: String,
        sub_code:       String,
        timestamp:      DateTime<Utc>,
    },
    /// Blueprint confirmed on-chain with profit (§13, §16).
    ///
    /// Not yet present in the frontend's WsEvent enum; arrives in
    /// `ws_client.rs`'s `other =>` branch and is silently dropped.
    /// Add to `omega-control-contracts::ws::WsEvent` and
    /// `observability.rs`'s no-op arm when a display path is needed.
    BlueprintConfirmed {
        blueprint_hash: String,
        strategy_id:    String,
        block_number:   u64,
        profit_net_eth: f64,
        timestamp:      DateTime<Utc>,
    },
}

/// Broadcast channel capacity for WebSocket events.
pub const WS_CHANNEL_CAPACITY: usize = 512;

// ── ALL_LAYER_IDS ─────────────────────────────────────────────────────────────

/// All 16 canonical layer IDs in L0–L15 order.
pub const ALL_LAYER_IDS: &[LayerId] = &[
    LayerId::Health,
    LayerId::Rpc,
    LayerId::Oracle,
    LayerId::Security,
    LayerId::Compliance,
    LayerId::Risk,
    LayerId::Dag,
    LayerId::Zk,
    LayerId::FlashLoan,
    LayerId::Relay,
    LayerId::GasWar,
    LayerId::LossAttribution,
    LayerId::AddressRotation,
    LayerId::Strategies,
    LayerId::HotPath,
    LayerId::Observability,
];

// ── AppState ──────────────────────────────────────────────────────────────────

pub struct AppState {
    // ── Config ────────────────────────────────────────────────────────────────
    pub config:          RwLock<OmegaConfig>,
    pub config_path:     PathBuf,

    // ── Gas model ─────────────────────────────────────────────────────────────
    pub checkpoint_dir:  PathBuf,
    pub model_paused:    AtomicBool,
    pub ceiling_tracker: RwLock<CeilingEscalationTracker>,

    // ── Builder blacklist ─────────────────────────────────────────────────────
    pub blacklist:       Arc<BuilderBlacklist>,

    // ── Health ────────────────────────────────────────────────────────────────
    pub health_layers:   Vec<Arc<LayerHealthImpl>>,

    // ── WebSocket broadcast ───────────────────────────────────────────────────
    pub ws_tx:           broadcast::Sender<WsEvent>,

    // ── Observability bridge ──────────────────────────────────────────────────
    /// Shared ring buffer — written by all engine layers via OmegaEvent::emit_*,
    /// drained by obs_bridge task which converts to WsEvent and publishes.
    pub obs_buffer:      Arc<EventRingBuffer>,

    // ── Authentication ────────────────────────────────────────────────────────
    pub api_token:       String,
}

impl AppState {
    pub fn new(
        config:         OmegaConfig,
        config_path:    PathBuf,
        checkpoint_dir: PathBuf,
        blacklist_path: PathBuf,
        api_token:      String,
        obs_buffer:     Arc<EventRingBuffer>,
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
            obs_buffer,
            api_token,
        }))
    }

    pub fn layer(&self, id: LayerId) -> Option<&Arc<LayerHealthImpl>> {
        self.health_layers.iter().find(|l| l.layer_id() == id)
    }

    pub fn subscribe_ws(&self) -> broadcast::Receiver<WsEvent> {
        self.ws_tx.subscribe()
    }

    pub fn publish(&self, event: WsEvent) {
        let _ = self.ws_tx.send(event);
    }
}

// ── Config helpers ────────────────────────────────────────────────────────────

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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;
    use omega_core::LayerId;

    #[test]
    fn all_layer_ids_has_exactly_16_entries() {
        assert_eq!(ALL_LAYER_IDS.len(), 16);
    }

    #[test]
    fn all_layer_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for &id in ALL_LAYER_IDS {
            assert!(seen.insert(id), "duplicate LayerId in ALL_LAYER_IDS: {id:?}");
        }
    }

    #[test]
    fn all_layer_ids_covers_every_canonical_variant() {
        let canonical: std::collections::HashSet<LayerId> = LayerId::iter().collect();
        let listed:    std::collections::HashSet<LayerId> = ALL_LAYER_IDS.iter().copied().collect();
        assert_eq!(
            canonical, listed,
            "ALL_LAYER_IDS does not match the full set of LayerId variants.\n\
             Missing: {:?}\n\
             Extra:   {:?}",
            canonical.difference(&listed).collect::<Vec<_>>(),
            listed.difference(&canonical).collect::<Vec<_>>(),
        );
    }

    // ── Serialisation contract tests ──────────────────────────────────────────
    //
    // Every assertion checks the "type" + "payload" shape that
    // `ws_client.rs`'s `other =>` branch hands to
    // `serde_json::from_str::<omega_control_contracts::ws::WsEvent>`.
    // If these strings change, the frontend silently stops recording events.

    #[test]
    fn profit_split_serialises_with_type_and_payload() {
        let ev = WsEvent::ProfitSplit {
            blueprint_hash: "0xabc".into(),
            pil_share_wei:  "1000000000000000000".into(),
            dao_fee_wei:    "50000000000000000".into(),
            timestamp:      chrono::Utc::now(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"type\":\"profit_split\""),
            "wrong type tag: {json}");
        assert!(json.contains("\"payload\":{"),
            "missing payload wrapper: {json}");
        assert!(json.contains("\"blueprint_hash\":\"0xabc\""),
            "blueprint_hash missing from payload: {json}");
        assert!(json.contains("\"pil_share_wei\":\"1000000000000000000\""),
            "pil_share_wei missing: {json}");
    }

    #[test]
    fn gas_model_reverted_serialises_with_type_and_payload() {
        let ev = WsEvent::GasModelReverted {
            checkpoint_version: 7,
            win_rate:           0.72,
            sample_count:       7000,
            timestamp:          chrono::Utc::now(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"type\":\"gas_model_reverted\""),
            "wrong type tag: {json}");
        assert!(json.contains("\"payload\":{"),
            "missing payload wrapper: {json}");
        assert!(json.contains("\"checkpoint_version\":7"),
            "checkpoint_version missing: {json}");
        assert!(json.contains("\"win_rate\":0.72"),
            "win_rate missing: {json}");
    }

    #[test]
    fn gas_model_ceiling_escalation_serialises_with_type_and_payload() {
        let ev = WsEvent::GasModelCeilingEscalation {
            feature_key:       "ARBITRUM_LA".into(),
            ceiling_hit_count: 101,
            timestamp:         chrono::Utc::now(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"type\":\"gas_model_ceiling_escalation\""),
            "wrong type tag: {json}");
        assert!(json.contains("\"payload\":{"), "missing payload wrapper: {json}");
        assert!(json.contains("\"ceiling_hit_count\":101"), "hit count missing: {json}");
    }

    #[test]
    fn emergency_bundle_skipped_serialises_fee_as_number() {
        // emergency_fee_gwei is f64 on the wire; serde_json serialises
        // integer-valued f64s without decimal point so the frontend's
        // u64 field deserialises cleanly.
        let ev = WsEvent::EmergencyBundleSkipped {
            blueprint_hash:     "0xdeadbeef".into(),
            emergency_fee_gwei: 9999.0,
            reason:             "fee_cap_exceeded".into(),
            timestamp:          chrono::Utc::now(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"type\":\"emergency_bundle_skipped\""),
            "wrong type tag: {json}");
        assert!(json.contains("\"payload\":{"), "missing payload wrapper: {json}");
        // Must not have "9999.0" — serde_json drops the trailing zero for
        // integer-valued f64, producing "9999", which u64 accepts.
        assert!(json.contains("\"emergency_fee_gwei\":9999"),
            "fee gwei wrong format: {json}");
        assert!(!json.contains("9999."),
            "f64 serialised with decimal point — u64 deserialization on frontend will fail: {json}");
    }

    #[test]
    fn la_reorg_risk_serialises_with_type_and_payload() {
        let ev = WsEvent::LaReorgRisk {
            tx_hash:        "0x1234".into(),
            orphaned_block: 19_000_000,
            reorg_depth:    2,
            timestamp:      chrono::Utc::now(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"type\":\"la_reorg_risk\""),
            "wrong type tag: {json}");
        assert!(json.contains("\"payload\":{"), "missing payload wrapper: {json}");
    }

    #[test]
    fn simulation_error_serialises_with_type_and_payload() {
        let ev = WsEvent::SimulationError {
            blueprint_hash: "0x9999".into(),
            sub_code:       "STATE_MISMATCH".into(),
            timestamp:      chrono::Utc::now(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"type\":\"simulation_error\""),
            "wrong type tag: {json}");
        assert!(json.contains("\"payload\":{"), "missing payload wrapper: {json}");
        assert!(json.contains("\"sub_code\":\"STATE_MISMATCH\""),
            "sub_code missing: {json}");
    }

    #[test]
    fn blueprint_confirmed_serialises_with_type_and_payload() {
        let ev = WsEvent::BlueprintConfirmed {
            blueprint_hash: "0x1234".into(),
            strategy_id:    "LA".into(),
            block_number:   19_000_000,
            profit_net_eth: 0.042,
            timestamp:      chrono::Utc::now(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"type\":\"blueprint_confirmed\""),
            "wrong type tag: {json}");
        assert!(json.contains("\"payload\":{"), "missing payload wrapper: {json}");
    }

    #[test]
    fn governance_events_serialise_with_type_and_payload() {
        // Governance events are not consumed by the frontend's WsEvent
        // deserialiser, but they must still serialise correctly so the
        // WebSocket handler doesn't drop them before sending.
        let config_reloaded = WsEvent::ConfigReloaded { timestamp: chrono::Utc::now() };
        let json = serde_json::to_string(&config_reloaded).unwrap();
        assert!(json.contains("\"type\":\"config_reloaded\""),
            "config_reloaded wrong type tag: {json}");
        assert!(json.contains("\"payload\":{"),
            "config_reloaded missing payload wrapper: {json}");

        let model_pause = WsEvent::ModelPauseChanged { paused: true, timestamp: chrono::Utc::now() };
        let json = serde_json::to_string(&model_pause).unwrap();
        assert!(json.contains("\"type\":\"model_pause_changed\""),
            "model_pause_changed wrong type tag: {json}");
        assert!(json.contains("\"paused\":true"), "paused field missing: {json}");

        let blacklist = WsEvent::BlacklistReloaded { entry_count: 42, timestamp: chrono::Utc::now() };
        let json = serde_json::to_string(&blacklist).unwrap();
        assert!(json.contains("\"type\":\"blacklist_reloaded\""),
            "blacklist_reloaded wrong type tag: {json}");
        assert!(json.contains("\"entry_count\":42"), "entry_count missing: {json}");

        let health = WsEvent::HealthTransition {
            layer: "relay".into(), from: "HEALTHY".into(),
            to: "DEGRADED".into(), reason: "test".into(),
            timestamp: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&health).unwrap();
        assert!(json.contains("\"type\":\"health_transition\""),
            "health_transition wrong type tag: {json}");
        assert!(json.contains("\"layer\":\"relay\""), "layer field missing: {json}");
    }
}