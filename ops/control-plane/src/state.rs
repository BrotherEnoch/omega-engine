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
// ## FIX (this revision): WsEvent is now the real, shared crate type
//
//   This file previously defined its OWN local `WsEvent` enum and its
//   own `WS_CHANNEL_CAPACITY` const, under the assumption that it was
//   serialised as `#[serde(tag = "type", content = "payload", rename_all
//   = "snake_case")]` — i.e. every event wrapped as
//   `{"type":"...","payload":{...}}`.
//
//   That assumption was wrong, and it broke `grpc.rs` and `ws.rs`, both
//   of which import `WsEvent` directly from
//   `omega_control_contracts::ws` (the actual type shared with the
//   frontend dashboard) rather than from this module. `grpc.rs`'s own
//   comment confirms the real wire format against that crate's own test
//   suite: `#[serde(tag = "kind", rename_all = "snake_case")]` — NO
//   `content` wrapper, every field flattened alongside `"kind"` at the
//   top level. Since `AppState.ws_tx` was typed as
//   `broadcast::Sender<state::WsEvent>` (the local, wrong-shaped type),
//   `grpc.rs`'s `watch_health` (`Ok(WsEvent::HealthTransition { .. }) =>
//   ...` matched against the stream from `state.subscribe_ws()`) and
//   `clear_halt` (`state.publish(WsEvent::HealthTransition { .. })`)
//   both failed to typecheck — two genuinely different `WsEvent` types
//   with the same name, not just a naming collision.
//
//   Fixed by dropping the local `WsEvent` enum and local
//   `WS_CHANNEL_CAPACITY` const entirely and importing both from
//   `omega_control_contracts::ws` instead, so `AppState.ws_tx` broadcasts
//   the one real, frontend-shared type everywhere. The variant field
//   shapes this file previously used (`HealthTransition { layer, from,
//   to, reason, timestamp }`, `ConfigReloaded { timestamp }`,
//   `ModelPauseChanged { paused, timestamp }`, `BlacklistReloaded
//   { entry_count, timestamp }`, `ProfitSplit { .. }`, `GasModelReverted
//   { .. }`) match exactly what `grpc.rs` and `ws.rs` already construct
//   against the real crate type, so no field-shape changes were needed —
//   only the import and the serialisation-format tests, which asserted
//   the wrong (local) shape and have been removed; `ws.rs`'s own test
//   module already carries the corrected assertions against the real
//   `"kind"`-tagged, flattened format.
//
//   NOT INDEPENDENTLY CONFIRMED: whether
//   `omega_control_contracts::ws::WsEvent` also has the
//   `GasModelCeilingEscalation`, `EmergencyBundleSkipped`, `LaReorgRisk`,
//   `SimulationError`, and `BlueprintConfirmed` variants that
//   `obs_bridge.rs` constructs — those were only ever exercised against
//   this file's old, local enum. If `cargo build` reports one of those
//   as missing from the real crate, `obs_bridge.rs`'s `map_omega_event`
//   needs adjusting (or that event simply has no display path yet, same
//   as this file's old comment already noted for `BlueprintConfirmed`).
//
// ## FIX (this revision, 2): ALL_LAYER_IDS removed
//
//   `grpc.rs`'s own comment notes it stopped depending on this module's
//   `ALL_LAYER_IDS` constant, iterating `omega_core::LayerId` via
//   `strum::IntoEnumIterator` instead — "the old module-local
//   ALL_LAYER_IDS constant... only ever lived in the dead state.rs".
//   Since nothing else in this crate depends on `ALL_LAYER_IDS` either,
//   it's removed here too, and `AppState::new`'s own health-layer
//   construction now uses the same `LayerId::iter()` approach for
//   consistency with `grpc.rs` rather than keeping two different ways to
//   enumerate the same 16 layers.
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

use tokio::sync::{broadcast, RwLock};

use strum::IntoEnumIterator;

use omega_control_contracts::ws::{WsEvent, WS_CHANNEL_CAPACITY};
use omega_core::{LayerHealth, LayerId, OmegaConfig};
use omega_gas_war::BuilderBlacklist;
use omega_health::LayerHealthImpl;
use omega_loss_attribution::ceiling_escalation::CeilingEscalationTracker;
use omega_observability::EventRingBuffer;

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
    /// Broadcasts `omega_control_contracts::ws::WsEvent` — the real type
    /// shared with the frontend dashboard (see this file's module-level
    /// FIX note). NOT a locally-defined type.
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

        // Same enumeration approach grpc.rs already uses — see this
        // file's module-level FIX note, 2.
        let health_layers: Vec<Arc<LayerHealthImpl>> = LayerId::iter()
            .map(LayerHealthImpl::new_bare)
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

    /// Builds a real AppState against tempfiles, same pattern main.rs's
    /// own test module uses — kept local to this module so state.rs's
    /// tests don't depend on main.rs's test helpers or vice versa.
    fn test_app_state() -> Arc<AppState> {
        let tmp_blacklist = tempfile::NamedTempFile::new().unwrap();
        let tmp_config = tempfile::NamedTempFile::new().unwrap();
        let obs_buffer = EventRingBuffer::new(omega_observability::DEFAULT_CAPACITY);

        AppState::new(
            OmegaConfig::default(),
            tmp_config.path().to_path_buf(),
            std::env::temp_dir(),
            tmp_blacklist.path().to_path_buf(),
            "test-token".into(),
            obs_buffer,
        )
        .unwrap()
    }

    #[test]
    fn health_layers_covers_all_16_canonical_layer_ids() {
        // Replaces the old ALL_LAYER_IDS-specific tests (see this file's
        // module-level FIX note, 2) — the same guarantee (every
        // LayerId variant present exactly once), now checked directly
        // against what AppState::new actually builds.
        let state = test_app_state();
        assert_eq!(state.health_layers.len(), 16);

        let canonical: std::collections::HashSet<LayerId> = LayerId::iter().collect();
        let built: std::collections::HashSet<LayerId> =
            state.health_layers.iter().map(|l| l.layer_id()).collect();
        assert_eq!(canonical, built);
    }

    #[test]
    fn layer_lookup_finds_every_layer() {
        let state = test_app_state();
        for l in &state.health_layers {
            assert!(state.layer(l.layer_id()).is_some());
        }
    }

    #[test]
    fn layer_lookup_returns_none_for_nothing_missing() {
        // There is no "unregistered" LayerId to test against directly
        // (every canonical variant is always registered by
        // AppState::new), so this instead confirms layer() doesn't
        // panic and returns a real, matching entry for a lookup done
        // twice — a basic sanity check on the Option/find plumbing.
        let state = test_app_state();
        let first = state.health_layers[0].layer_id();
        assert_eq!(state.layer(first).unwrap().layer_id(), first);
    }

    #[tokio::test]
    async fn publish_reaches_subscriber_with_real_wsevent_type() {
        // Confirms `ws_tx` really is `broadcast::Sender<WsEvent>` where
        // `WsEvent` is `omega_control_contracts::ws::WsEvent` — this
        // would fail to compile at all if the import in this file
        // regressed back to a local, incompatible type (see this file's
        // module-level FIX note).
        let state = test_app_state();
        let mut rx = state.subscribe_ws();

        state.publish(WsEvent::ConfigReloaded {
            timestamp: chrono::Utc::now(),
        });

        let received = rx.try_recv();
        assert!(received.is_ok(), "publish() must reach an active subscriber");
    }

    // ── Coverage for the 5 variants obs_bridge.rs constructs but that
    // were never independently confirmed against the real crate (see
    // this file's and obs_bridge.rs's module-level FIX notes) ──────────
    //
    // These don't re-guess the wire format — `omega_control_contracts::ws::WsEvent`
    // is a single enum with one `#[serde(tag = "kind", rename_all =
    // "snake_case")]` attribute covering every variant, and that
    // attribute is already confirmed (via grpc.rs/ws.rs, cross-checked
    // against the crate's own test suite) for ConfigReloaded,
    // ModelPauseChanged, BlacklistReloaded, HealthTransition,
    // ProfitSplit, and GasModelReverted. An enum-level serde attribute
    // applies uniformly to every variant, so the same "kind"-tagged,
    // flattened shape necessarily applies here too — what these tests
    // actually establish is narrower and more useful than re-deriving
    // the format: that GasModelCeilingEscalation, EmergencyBundleSkipped,
    // LaReorgRisk, SimulationError, and BlueprintConfirmed genuinely
    // EXIST on the real enum with the field names obs_bridge.rs assumes.
    // If any of them don't, this file fails to COMPILE — turning a
    // documented assumption into a build-time guarantee instead of a
    // runtime surprise.

    #[test]
    fn gas_model_ceiling_escalation_exists_and_serialises_flat() {
        let event = WsEvent::GasModelCeilingEscalation {
            feature_key: "ARBITRUM_LA".into(),
            ceiling_hit_count: 101,
            timestamp: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.contains("\"kind\":\"gas_model_ceiling_escalation\""),
            "wrong or missing kind tag: {json}"
        );
        assert!(
            json.contains("\"ceiling_hit_count\":101"),
            "ceiling_hit_count missing (should be flattened at top level): {json}"
        );
    }

    #[test]
    fn emergency_bundle_skipped_exists_and_serialises_flat() {
        let event = WsEvent::EmergencyBundleSkipped {
            blueprint_hash: "0xdeadbeef".into(),
            emergency_fee_gwei: 9999.0,
            reason: "fee_cap_exceeded".into(),
            timestamp: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.contains("\"kind\":\"emergency_bundle_skipped\""),
            "wrong or missing kind tag: {json}"
        );
        assert!(
            json.contains("\"blueprint_hash\":\"0xdeadbeef\""),
            "blueprint_hash missing (should be flattened at top level): {json}"
        );
        // FIX (confirmed by a real test run, not guessed): this crate's
        // serde_json ALWAYS serialises f64 with a decimal point, including
        // integer-valued ones — `emergency_fee_gwei: 9999.0` serialises as
        // `9999.0`, never bare `9999`. The previous version of this test
        // asserted the opposite ("integer-valued f64 drops the decimal
        // point") — an assumption inherited from obs_bridge.rs's own
        // comment, itself never verified against a real run until this
        // test actually failed with the JSON shown right here.
        //
        // REAL OPEN QUESTION, NOT FIXABLE FROM THIS CRATE: if the
        // frontend's `EmergencyBundleSkippedEvent.emergency_fee_gwei` is
        // typed as `u64` (as obs_bridge.rs's original comment assumed),
        // serde's default u64 deserialisation rejects a JSON float token
        // like `9999.0` outright — "invalid type: floating point number,
        // expected u64". That's a real cross-crate risk this test can't
        // resolve: either the frontend field needs to be f64 (matching
        // what's actually sent), or obs_bridge.rs needs to send a true
        // integer (drop the `as f64` cast) rather than a float. Neither
        // change belongs in this crate's own serialisation test — this
        // test's job is only to confirm what THIS crate actually puts on
        // the wire, which is now correctly asserted below.
        assert!(
            json.contains("\"emergency_fee_gwei\":9999.0"),
            "fee gwei wrong format: {json}"
        );
    }

    #[test]
    fn la_reorg_risk_exists_and_serialises_flat() {
        let event = WsEvent::LaReorgRisk {
            tx_hash: "0x1234".into(),
            orphaned_block: 19_000_000,
            reorg_depth: 0,
            timestamp: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.contains("\"kind\":\"la_reorg_risk\""),
            "wrong or missing kind tag: {json}"
        );
        assert!(
            json.contains("\"orphaned_block\":19000000"),
            "orphaned_block missing (should be flattened at top level): {json}"
        );
    }

    // NOTE: there is no `simulation_error_exists_and_serialises_flat` test
    // here. `SimulationError` was in the original, wrong local `WsEvent`
    // enum this file used to define, but rustc has now confirmed the real
    // `omega_control_contracts::ws::WsEvent` has no such variant — the
    // dashboard genuinely has no display path for that event. Removed
    // rather than "fixed", since there's nothing real to test against.

    #[test]
    fn blueprint_confirmed_exists_and_serialises_flat() {
        // FIX (confirmed by rustc): the real field is `profit_net_wei`, not
        // `profit_net_eth` — see obs_bridge.rs's module-level FIX note for
        // the same correction and the type caveat (assumed String, matching
        // this enum's ProfitSplit precedent; NOT independently confirmed).
        let event = WsEvent::BlueprintConfirmed {
            blueprint_hash: "0x1234".into(),
            strategy_id: "LA".into(),
            block_number: 19_000_000,
            profit_net_wei: "42000000000000000".into(),
            timestamp: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.contains("\"kind\":\"blueprint_confirmed\""),
            "wrong or missing kind tag: {json}"
        );
        assert!(
            json.contains("\"strategy_id\":\"LA\""),
            "strategy_id missing (should be flattened at top level): {json}"
        );
        assert!(
            json.contains("\"profit_net_wei\":\"42000000000000000\""),
            "profit_net_wei missing or wrong shape (should be flattened at top level): {json}"
        );
    }
}