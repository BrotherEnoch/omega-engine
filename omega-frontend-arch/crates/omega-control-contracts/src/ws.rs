// omega-frontend-arch/crates/omega-control-contracts/src/ws.rs

use serde::{Deserialize, Serialize};
use crate::health::{HealthStatus, LayerId};

// ---------------------------------------------------------------------------
// Rate limit constants (v12 §17.1)
// ---------------------------------------------------------------------------

pub const WS_CHANNEL_CAPACITY:     usize = 512;
pub const AUTHED_LIMIT_PER_MINUTE: u32   = 300;
pub const ANON_LIMIT_PER_MINUTE:   u32   = 100;

pub struct WsRateLimit;
impl WsRateLimit {
    pub const AUTHENTICATED_PER_MIN: u32 = AUTHED_LIMIT_PER_MINUTE;
    pub const ANONYMOUS_PER_MIN:     u32 = ANON_LIMIT_PER_MINUTE;
}

// ---------------------------------------------------------------------------
// WsConnectionStatus
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WsConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting { attempt: u32 },
    Unavailable,
    AuthError,
}

// ---------------------------------------------------------------------------
// Wire layer tag — "L00" … "L15"
// ---------------------------------------------------------------------------

/// Parse the wire layer tag (e.g. `"L08"`) into a `LayerId`.
/// Returns `None` for unrecognised tags so the client can skip them.
///
/// Tag-to-variant mapping matches the canonical v12 LayerId enum order in
/// `health.rs` exactly (L00=SystemHealth, L01=ExternalData, L02=Oracle,
/// L03=Risk, L04=Security, L05=Eil, L06=Dag, …). Previously L02 was mapped
/// to Eil and L05 to ChaosGuard — both wrong: ChaosGuard was removed from
/// the enum (it was only ever a pre-v12 alias for Security on the backend,
/// never a distinct layer) and Oracle was missing entirely.
pub fn layer_id_from_wire(tag: &str) -> Option<LayerId> {
    match tag {
        "L00" => Some(LayerId::SystemHealth),
        "L01" => Some(LayerId::ExternalData),
        "L02" => Some(LayerId::Oracle),
        "L03" => Some(LayerId::Risk),
        "L04" => Some(LayerId::Security),
        "L05" => Some(LayerId::Eil),
        "L06" => Some(LayerId::Dag),
        "L07" => Some(LayerId::Zk),
        "L08" => Some(LayerId::HotPath),
        "L09" => Some(LayerId::Strategy),
        "L10" => Some(LayerId::Flashloan),
        "L11" => Some(LayerId::Orchestrator),
        "L12" => Some(LayerId::Relay),
        "L13" => Some(LayerId::Vault),
        "L14" => Some(LayerId::Observability),
        "L15" => Some(LayerId::LossAttribution),
        _     => None,
    }
}

// ---------------------------------------------------------------------------
// SimulationErrorSubCode
// ---------------------------------------------------------------------------

/// Sub-classification for `SimulationError` events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SimulationErrorSubCode {
    /// Fork-state diverged between simulation and execution.
    StateMismatch,
    /// EVM reverted during simulation replay.
    ExecutionRevert,
    /// Gas estimate was materially wrong.
    GasMiscalc,
}

// ---------------------------------------------------------------------------
// Event payloads
// ---------------------------------------------------------------------------

/// Payload for `{"type":"layer_event", "payload":{…}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerEventPayload {
    /// Wire layer tag: "L00" … "L15".
    pub layer:      String,
    /// Health status string: "HEALTHY" | "DEGRADED" | "HALTED" | "RECOVERING".
    pub status:     String,
    /// Human-readable message from the layer (e.g. `"tick=62810"`).
    pub message:    String,
    /// Monotonically increasing version counter.
    pub version:    u64,
    /// Processing latency in nanoseconds.
    pub latency_ns: u64,
}

impl LayerEventPayload {
    /// Resolve the wire tag to a typed `LayerId`.
    pub fn layer_id(&self) -> Option<LayerId> {
        layer_id_from_wire(&self.layer)
    }

    /// Parse the status string into a typed `HealthStatus`.
    pub fn health_status(&self) -> HealthStatus {
        HealthStatus::from_backend_str(&self.status)
    }
}

/// Payload for `{"type":"ping", "payload":{…}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingPayload {
    pub nonce: u64,
}

/// Emitted when the loss-attribution gas model is reverted to a checkpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GasModelRevertedEvent {
    /// The checkpoint version that was restored.
    pub checkpoint_version: u64,
    /// Win rate of the restored checkpoint.
    pub win_rate:           f64,
    /// Number of samples the checkpoint was trained on.
    pub sample_count:       u64,
}

/// Emitted when a feature ceiling is escalated due to repeated hits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GasModelCeilingEvent {
    /// Feature key whose ceiling was hit (e.g. `"ARBITRUM_LA"`).
    pub feature_key:       String,
    /// Cumulative hit count at the time of escalation.
    pub ceiling_hit_count: u64,
}

/// Emitted when an emergency bundle is skipped because the fee exceeds the cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmergencyBundleSkippedEvent {
    /// Blueprint hash of the skipped bundle.
    pub blueprint_hash:     String,
    /// Human-readable skip reason.
    pub reason:             String,
    /// Emergency fee that triggered the skip, in gwei.
    ///
    /// The backend's `state::WsEvent::EmergencyBundleSkipped` carries this
    /// field as `f64` (the obs_bridge casts from u64 via `as f64`).
    /// serde_json serialises integer-valued f64s without a decimal point
    /// (e.g. `9999.0` → `9999`), so this u64 field deserialises cleanly.
    pub emergency_fee_gwei: u64,
}

/// Emitted after a successful profit split between PIL and the DAO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfitSplitEvent {
    /// Blueprint hash of the settled bundle.
    pub blueprint_hash: String,
    /// PIL's share in wei (string to avoid u128 JSON issues).
    pub pil_share_wei:  String,
    /// DAO fee in wei (string to avoid u128 JSON issues).
    pub dao_fee_wei:    String,
}

/// Emitted when a reorg risk is detected for a recently landed transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaReorgRiskEvent {
    /// Transaction hash at risk.
    pub tx_hash:        String,
    /// Block number that was orphaned.
    pub orphaned_block: u64,
    /// Depth of the reorg detected.
    pub reorg_depth:    u32,
}

/// Emitted when the simulation layer detects a discrepancy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulationErrorEvent {
    /// Blueprint hash of the bundle that failed simulation.
    pub blueprint_hash: String,
    /// Fine-grained error classification.
    pub sub_code:       SimulationErrorSubCode,
    /// Optional human-readable detail string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail:         Option<String>,
}

/// Emitted when a blueprint is confirmed on-chain with a positive profit.
///
/// The backend's `state::WsEvent::BlueprintConfirmed` carries a `timestamp`
/// field; this payload struct omits it because `obs_panel.rs` uses
/// `ObservabilityEntry::recorded_at` for display and the timestamp is not
/// part of the `observability.rs` ring buffer entry.  The `timestamp` field
/// in the wire JSON is simply ignored by serde (no `#[serde(deny_unknown_fields)]`
/// on this struct).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlueprintConfirmedEvent {
    /// Blueprint hash of the confirmed bundle.
    pub blueprint_hash: String,
    /// Strategy that produced this blueprint.
    pub strategy_id:    String,
    /// Block number in which the blueprint was confirmed.
    pub block_number:   u64,
    /// Net profit in ETH (after gas costs).
    pub profit_net_eth: f64,
}

// ---------------------------------------------------------------------------
// WsEvent — matches the actual wire format
//
// Every message has shape: { "type": "<tag>", "payload": { … } }
// ---------------------------------------------------------------------------

/// Incoming WebSocket event — deserialises from the actual wire JSON.
///
/// `#[serde(tag = "type", content = "payload", rename_all = "snake_case")]`
/// must match `ops/control-plane/src/state.rs`'s `WsEvent` attribute
/// exactly.  Both ends use the same shape; mismatching either attribute
/// silently breaks deserialisation for every trading event variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum WsEvent {
    /// Per-layer heartbeat / status update.
    LayerEvent(LayerEventPayload),

    /// Ping keepalive — payload is the nonce object.
    Ping(PingPayload),

    /// Gas model reverted to a prior checkpoint.
    GasModelReverted(GasModelRevertedEvent),

    /// A feature ceiling was escalated.
    GasModelCeilingEscalation(GasModelCeilingEvent),

    /// An emergency bundle was skipped because the fee exceeded the cap.
    EmergencyBundleSkipped(EmergencyBundleSkippedEvent),

    /// A profit split was completed between PIL and the DAO.
    ProfitSplit(ProfitSplitEvent),

    /// A reorg risk was detected for a recently landed transaction.
    LaReorgRisk(LaReorgRiskEvent),

    /// The simulation layer detected a discrepancy.
    SimulationError(SimulationErrorEvent),

    /// A blueprint was confirmed on-chain with positive profit (§13, §16).
    ///
    /// Handled in `ws_client.rs`'s `other =>` branch: routed to
    /// `record_obs_event` which hits the no-op arm in `observability.rs`
    /// (`WsEvent::LayerEvent(_) | WsEvent::Ping(_) | WsEvent::BlueprintConfirmed(_) => None`)
    /// so it does not produce a ring buffer entry.  Adding a display path
    /// requires: (1) a new `ObservabilityEventKind::BlueprintConfirmed`
    /// variant, (2) a counter in `ObservabilityMetrics`, (3) a new metrics
    /// row in `obs_panel.rs`.
    BlueprintConfirmed(BlueprintConfirmedEvent),
}

// ---------------------------------------------------------------------------
// Auth frames (client → server / server → client)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsClientFrame {
    Auth { token: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsAuthFrame {
    AuthOk     { rate_limit: u32, window_secs: u64 },
    AuthFailed { rate_limit: u32, window_secs: u64 },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_constants_match_spec() {
        assert_eq!(AUTHED_LIMIT_PER_MINUTE, 300);
        assert_eq!(ANON_LIMIT_PER_MINUTE,   100);
        assert_eq!(WsRateLimit::AUTHENTICATED_PER_MIN, 300);
        assert_eq!(WsRateLimit::ANONYMOUS_PER_MIN,     100);
    }

    #[test]
    fn layer_event_round_trip() {
        let raw = r#"{"type":"layer_event","payload":{"layer":"L15","status":"HEALTHY","message":"tick=62810","version":5505316,"latency_ns":1300}}"#;
        let ev: WsEvent = serde_json::from_str(raw).unwrap();
        match &ev {
            WsEvent::LayerEvent(p) => {
                assert_eq!(p.layer, "L15");
                assert_eq!(p.layer_id(), Some(LayerId::LossAttribution));
                assert_eq!(p.health_status(), HealthStatus::Ok);
                assert_eq!(p.version, 5505316);
                assert_eq!(p.message, "tick=62810");
            }
            _ => panic!("expected LayerEvent"),
        }
        let re  = serde_json::to_string(&ev).unwrap();
        let ev2: WsEvent = serde_json::from_str(&re).unwrap();
        assert_eq!(ev, ev2);
    }

    #[test]
    fn layer_event_l00_aggregator() {
        let raw = r#"{"type":"layer_event","payload":{"layer":"L00","status":"HEALTHY","message":"all 15 layers HEALTHY","version":5505341,"latency_ns":0}}"#;
        let ev: WsEvent = serde_json::from_str(raw).unwrap();
        match &ev {
            WsEvent::LayerEvent(p) => {
                assert_eq!(p.layer_id(), Some(LayerId::SystemHealth));
            }
            _ => panic!("expected LayerEvent"),
        }
    }

    #[test]
    fn layer_event_l02_is_oracle() {
        let raw = r#"{"type":"layer_event","payload":{"layer":"L02","status":"HEALTHY","message":"tick=1","version":1,"latency_ns":0}}"#;
        let ev: WsEvent = serde_json::from_str(raw).unwrap();
        match &ev {
            WsEvent::LayerEvent(p) => {
                assert_eq!(p.layer_id(), Some(LayerId::Oracle));
            }
            _ => panic!("expected LayerEvent"),
        }
    }

    #[test]
    fn layer_event_l05_is_eil() {
        let raw = r#"{"type":"layer_event","payload":{"layer":"L05","status":"HEALTHY","message":"tick=1","version":1,"latency_ns":0}}"#;
        let ev: WsEvent = serde_json::from_str(raw).unwrap();
        match &ev {
            WsEvent::LayerEvent(p) => {
                assert_eq!(p.layer_id(), Some(LayerId::Eil));
            }
            _ => panic!("expected LayerEvent"),
        }
    }

    #[test]
    fn ping_round_trip() {
        let raw = r#"{"type":"ping","payload":{"nonce":42}}"#;
        let ev: WsEvent = serde_json::from_str(raw).unwrap();
        match &ev {
            WsEvent::Ping(p) => assert_eq!(p.nonce, 42),
            _ => panic!("expected Ping"),
        }
        let re  = serde_json::to_string(&ev).unwrap();
        let ev2: WsEvent = serde_json::from_str(&re).unwrap();
        assert_eq!(ev, ev2);
    }

    #[test]
    fn all_l_tags_map_to_layer_id() {
        for i in 0u8..=15 {
            let tag = format!("L{:02}", i);
            assert!(layer_id_from_wire(&tag).is_some(), "no mapping for {tag}");
        }
        assert!(layer_id_from_wire("L16").is_none());
        assert!(layer_id_from_wire("RELAY").is_none());
    }

    #[test]
    fn unknown_layer_tag_returns_none() {
        assert!(layer_id_from_wire("L99").is_none());
        assert!(layer_id_from_wire("").is_none());
    }

    #[test]
    fn all_layer_ids_reachable_via_wire_tags() {
        use strum::IntoEnumIterator;
        let reachable: std::collections::HashSet<LayerId> =
            (0u8..=15).filter_map(|i| layer_id_from_wire(&format!("L{:02}", i))).collect();
        for id in LayerId::iter() {
            assert!(reachable.contains(&id), "LayerId::{id:?} has no L-tag mapping in layer_id_from_wire");
        }
    }

    #[test]
    fn gas_model_reverted_round_trip() {
        let raw = r#"{"type":"gas_model_reverted","payload":{"checkpoint_version":7,"win_rate":0.72,"sample_count":7000}}"#;
        let ev: WsEvent = serde_json::from_str(raw).unwrap();
        match &ev {
            WsEvent::GasModelReverted(p) => {
                assert_eq!(p.checkpoint_version, 7);
                assert!((p.win_rate - 0.72).abs() < f64::EPSILON);
                assert_eq!(p.sample_count, 7000);
            }
            _ => panic!("expected GasModelReverted"),
        }
        let re  = serde_json::to_string(&ev).unwrap();
        let ev2: WsEvent = serde_json::from_str(&re).unwrap();
        assert_eq!(ev, ev2);
    }

    #[test]
    fn gas_model_ceiling_escalation_round_trip() {
        let raw = r#"{"type":"gas_model_ceiling_escalation","payload":{"feature_key":"ARBITRUM_LA","ceiling_hit_count":101}}"#;
        let ev: WsEvent = serde_json::from_str(raw).unwrap();
        match &ev {
            WsEvent::GasModelCeilingEscalation(p) => {
                assert_eq!(p.feature_key, "ARBITRUM_LA");
                assert_eq!(p.ceiling_hit_count, 101);
            }
            _ => panic!("expected GasModelCeilingEscalation"),
        }
        let re  = serde_json::to_string(&ev).unwrap();
        let ev2: WsEvent = serde_json::from_str(&re).unwrap();
        assert_eq!(ev, ev2);
    }

    #[test]
    fn emergency_bundle_skipped_round_trip() {
        let raw = r#"{"type":"emergency_bundle_skipped","payload":{"blueprint_hash":"0xdeadbeef","reason":"fee_cap_exceeded","emergency_fee_gwei":9999}}"#;
        let ev: WsEvent = serde_json::from_str(raw).unwrap();
        match &ev {
            WsEvent::EmergencyBundleSkipped(p) => {
                assert_eq!(p.blueprint_hash, "0xdeadbeef");
                assert_eq!(p.emergency_fee_gwei, 9999);
            }
            _ => panic!("expected EmergencyBundleSkipped"),
        }
        let re  = serde_json::to_string(&ev).unwrap();
        let ev2: WsEvent = serde_json::from_str(&re).unwrap();
        assert_eq!(ev, ev2);
    }

    #[test]
    fn profit_split_round_trip() {
        let raw = r#"{"type":"profit_split","payload":{"blueprint_hash":"0xabc","pil_share_wei":"1000000000000000000","dao_fee_wei":"50000000000000000"}}"#;
        let ev: WsEvent = serde_json::from_str(raw).unwrap();
        match &ev {
            WsEvent::ProfitSplit(p) => {
                assert_eq!(p.blueprint_hash, "0xabc");
                assert_eq!(p.pil_share_wei, "1000000000000000000");
                assert_eq!(p.dao_fee_wei, "50000000000000000");
            }
            _ => panic!("expected ProfitSplit"),
        }
        let re  = serde_json::to_string(&ev).unwrap();
        let ev2: WsEvent = serde_json::from_str(&re).unwrap();
        assert_eq!(ev, ev2);
    }

    #[test]
    fn la_reorg_risk_round_trip() {
        let raw = r#"{"type":"la_reorg_risk","payload":{"tx_hash":"0x1234","orphaned_block":19000000,"reorg_depth":2}}"#;
        let ev: WsEvent = serde_json::from_str(raw).unwrap();
        match &ev {
            WsEvent::LaReorgRisk(p) => {
                assert_eq!(p.tx_hash, "0x1234");
                assert_eq!(p.orphaned_block, 19000000);
                assert_eq!(p.reorg_depth, 2);
            }
            _ => panic!("expected LaReorgRisk"),
        }
        let re  = serde_json::to_string(&ev).unwrap();
        let ev2: WsEvent = serde_json::from_str(&re).unwrap();
        assert_eq!(ev, ev2);
    }

    #[test]
    fn simulation_error_round_trip() {
        let raw = r#"{"type":"simulation_error","payload":{"blueprint_hash":"0x9999","sub_code":"STATE_MISMATCH","detail":"fork block 19000001"}}"#;
        let ev: WsEvent = serde_json::from_str(raw).unwrap();
        match &ev {
            WsEvent::SimulationError(p) => {
                assert_eq!(p.blueprint_hash, "0x9999");
                assert_eq!(p.sub_code, SimulationErrorSubCode::StateMismatch);
                assert_eq!(p.detail.as_deref(), Some("fork block 19000001"));
            }
            _ => panic!("expected SimulationError"),
        }
        let re  = serde_json::to_string(&ev).unwrap();
        let ev2: WsEvent = serde_json::from_str(&re).unwrap();
        assert_eq!(ev, ev2);
    }

    #[test]
    fn simulation_error_sub_codes_round_trip() {
        for (sub, s) in [
            (SimulationErrorSubCode::StateMismatch,   "STATE_MISMATCH"),
            (SimulationErrorSubCode::ExecutionRevert, "EXECUTION_REVERT"),
            (SimulationErrorSubCode::GasMiscalc,      "GAS_MISCALC"),
        ] {
            let serialised = serde_json::to_string(&sub).unwrap();
            assert_eq!(serialised, format!("\"{}\"", s));
            let back: SimulationErrorSubCode = serde_json::from_str(&serialised).unwrap();
            assert_eq!(back, sub);
        }
    }

    #[test]
    fn simulation_error_without_detail_round_trip() {
        let raw = r#"{"type":"simulation_error","payload":{"blueprint_hash":"0x1","sub_code":"GAS_MISCALC"}}"#;
        let ev: WsEvent = serde_json::from_str(raw).unwrap();
        match &ev {
            WsEvent::SimulationError(p) => {
                assert_eq!(p.sub_code, SimulationErrorSubCode::GasMiscalc);
                assert!(p.detail.is_none());
            }
            _ => panic!("expected SimulationError"),
        }
    }

    #[test]
    fn blueprint_confirmed_round_trip() {
        // The backend sends a timestamp field; serde ignores unknown fields
        // (no deny_unknown_fields on BlueprintConfirmedEvent), so this must
        // deserialise cleanly even with the extra field present.
        let raw = r#"{"type":"blueprint_confirmed","payload":{"blueprint_hash":"0x1234","strategy_id":"LA","block_number":19000000,"profit_net_eth":0.042,"timestamp":"2026-06-27T00:00:00Z"}}"#;
        let ev: WsEvent = serde_json::from_str(raw).unwrap();
        match &ev {
            WsEvent::BlueprintConfirmed(p) => {
                assert_eq!(p.blueprint_hash, "0x1234");
                assert_eq!(p.strategy_id,    "LA");
                assert_eq!(p.block_number,   19_000_000);
                assert!((p.profit_net_eth - 0.042).abs() < 1e-10);
            }
            _ => panic!("expected BlueprintConfirmed"),
        }
        // Round-trip without the extra timestamp field (frontend never adds it)
        let re  = serde_json::to_string(&ev).unwrap();
        let ev2: WsEvent = serde_json::from_str(&re).unwrap();
        assert_eq!(ev, ev2);
    }

    #[test]
    fn blueprint_confirmed_no_timestamp_field_deserialises() {
        // Backend may evolve; ensure we handle absence of optional fields.
        let raw = r#"{"type":"blueprint_confirmed","payload":{"blueprint_hash":"0xabc","strategy_id":"SA","block_number":1,"profit_net_eth":0.001}}"#;
        let ev: WsEvent = serde_json::from_str(raw).unwrap();
        assert!(matches!(ev, WsEvent::BlueprintConfirmed(_)));
    }
}