// crates/omega-control-contracts/src/rest.rs
//
// REST DTOs — field shapes match the backend wire format exactly.
// Backend (ops/control-plane/src/main.rs) is the authoritative source.
// Do not rename fields; the backend serialises with serde defaults (snake_case).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Generic
// ---------------------------------------------------------------------------

/// Generic success body returned by mutation endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiOk {
    pub status: &'static str,
}

pub const OK: ApiOk = ApiOk { status: "ok" };

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub error:   String,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Health  — matches GET /api/v1/health backend handler exactly
// ---------------------------------------------------------------------------

/// A single layer entry as returned by the backend REST endpoint and
/// kept live by WebSocket `layer_event` messages.
///
/// Field names: `layer` (string id), `state` (string status), `is_operational`.
/// The backend serialises LayerId and HealthState as strings via `.to_string()`.
///
/// ## State string values (backend)
///   "HEALTHY" | "DEGRADED" | "HALTED" | "RECOVERING" | "UNKNOWN"
///
/// ## Layer string values (backend omega-core LayerId)
///   "SYSTEM_HEALTH" | "EXTERNAL_DATA" | "EIL" | "RISK" | "SECURITY" |
///   "CHAOS_GUARD" | "DAG" | "ZK" | "HOT_PATH" | "STRATEGY" | "FLASHLOAN" |
///   "ORCHESTRATOR" | "RELAY" | "VAULT" | "OBSERVABILITY" | "LOSS_ATTRIBUTION"
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerHealthEntry {
    /// Layer identifier string (backend LayerId::to_string()).
    pub layer: String,
    /// Health state string (backend HealthState::to_string()).
    pub state: String,
    /// True when the layer is not halted or degraded.
    pub is_operational: bool,
    /// Optional diagnostic message from the layer.
    /// Absent in REST snapshots; populated from WS `layer_event` messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl LayerHealthEntry {
    /// Returns true if this layer is currently halted.
    pub fn is_halted(&self) -> bool { self.state == "HALTED" }

    /// Returns true if this layer is healthy.
    pub fn is_healthy(&self) -> bool { self.state == "HEALTHY" }
}

/// Full health snapshot from GET /api/v1/health.
///
/// Field `generated_at` matches backend (not `snapshot_at`).
/// Field `system_halted` matches backend (not `overall`).
/// No `revision` field — backend does not emit one on this endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthSnapshot {
    /// UTC timestamp when this snapshot was generated.
    pub generated_at:  DateTime<Utc>,
    /// Per-layer health entries (16 layers in v12).
    pub layers:        Vec<LayerHealthEntry>,
    /// True if any layer is in the HALTED state.
    pub system_halted: bool,
}

impl HealthSnapshot {
    /// Compute the overall worst status across all layers as a string.
    /// Returns "HALTED" > "DEGRADED" > "HEALTHY".
    pub fn overall_state(&self) -> &'static str {
        if self.layers.iter().any(|l| l.state == "HALTED")   { return "HALTED"; }
        if self.layers.iter().any(|l| l.state == "DEGRADED") { return "DEGRADED"; }
        "HEALTHY"
    }
}

// ---------------------------------------------------------------------------
// Config  — matches POST /api/v1/config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigReloadRequest {
    pub from_disk: bool,
    pub body:      Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Gas model checkpoints  — matches GET /api/v1/la/gas-model/checkpoints
// ---------------------------------------------------------------------------

/// A single checkpoint entry as returned by `checkpoint::list_checkpoints`.
///
/// `PartialEq` uses bitwise f64 equality, which is correct here because these
/// values are wire-round-tripped and never computed from floating-point arithmetic
/// on the frontend side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GasModelCheckpoint {
    pub version:           u64,
    pub win_rate:          f64,
    pub sample_count:      u64,
    pub baseline_win_rate: f64,
    pub saved_at:          DateTime<Utc>,
}

/// Response for POST /api/v1/la/gas-model/revert/{version}.
/// Field name `reverted_to_version` matches backend handler exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevertResponse {
    pub reverted_to_version: u64,
    pub win_rate:            f64,
    pub sample_count:        u64,
}

// ---------------------------------------------------------------------------
// Ceiling status  — matches GET /api/v1/la/gas-model/ceiling-status
//
// This is the JSON actually emitted by CeilingEscalationTracker::snapshot()
// in ops/control-plane (confirmed live via curl against a running server):
//
//   {"paused":false,"consecutive_ceiling_hits":0,"escalation_threshold":100,
//    "trigger_key":null,"last_hit_at":null,"paused_at":null}
//
// An earlier `features: Vec<FeatureCeilingStatus>` / `any_paused` shape
// here never matched what the handler returns (it calls Json(snapshot)
// directly on the tracker's own type, not this struct), which broke the
// frontend's poll with "missing field `features`". This struct now
// mirrors the tracker's real output exactly.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CeilingStatusResponse {
    /// True when the gas model is currently paused due to ceiling escalation.
    pub paused: bool,
    /// Current consecutive-hit count toward the escalation threshold.
    pub consecutive_ceiling_hits: u64,
    /// Configured threshold (ml.ceiling_escalation_threshold) that triggers a pause.
    pub escalation_threshold: u64,
    /// Feature/key that triggered the most recent ceiling hit, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_key: Option<String>,
    /// Timestamp of the most recent ceiling hit, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_hit_at: Option<DateTime<Utc>>,
    /// Timestamp the model was paused, if currently paused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Vault / DAO fee  — matches GET /api/v1/vault/dao-fee backend handler exactly
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaoFeeResponse {
    /// Fee in basis points (0–1000).
    pub dao_fee_bps:          u16,
    /// Fee as a percentage (dao_fee_bps / 100.0).
    pub dao_fee_pct:          f64,
    /// Per-transfer cap in ETH (converted from wei by backend).
    pub per_transfer_cap_eth: f64,
    /// Daily cap in ETH (converted from wei by backend).
    pub daily_cap_eth:        f64,
    /// Required on-chain confirmation depth (12 for Ethereum).
    pub confirmation_depth:   u8,
}

// ---------------------------------------------------------------------------
// Builder blacklist  — matches GET /api/v1/builders/blacklist backend exactly
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlacklistResponse {
    /// Number of entries in the blacklist.
    pub entry_count: usize,
    /// Filesystem path of the blacklist file.
    pub path:        String,
    /// True when the blacklist contains no entries.
    pub is_empty:    bool,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_ok_round_trip() {
        let s = serde_json::to_string(&OK).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["status"], "ok");
    }

    #[test]
    fn health_snapshot_round_trip() {
        let snap = HealthSnapshot {
            generated_at:  Utc::now(),
            layers: vec![
                LayerHealthEntry {
                    layer: "RELAY".into(), state: "HEALTHY".into(),
                    is_operational: true,  message: None,
                },
                LayerHealthEntry {
                    layer: "LOSS_ATTRIBUTION".into(), state: "HALTED".into(),
                    is_operational: false, message: None,
                },
            ],
            system_halted: true,
        };
        let s   = serde_json::to_string(&snap).unwrap();
        let s2: HealthSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(s2.layers.len(), 2);
        assert!(s2.system_halted);
        assert_eq!(s2.overall_state(), "HALTED");
    }

    #[test]
    fn layer_health_entry_helpers() {
        let halted = LayerHealthEntry {
            layer: "HOT_PATH".into(), state: "HALTED".into(),
            is_operational: false, message: None,
        };
        assert!(halted.is_halted());
        assert!(!halted.is_healthy());

        let healthy = LayerHealthEntry {
            layer: "RELAY".into(), state: "HEALTHY".into(),
            is_operational: true, message: None,
        };
        assert!(healthy.is_healthy());
        assert!(!healthy.is_halted());
    }

    #[test]
    fn layer_health_entry_message_round_trip() {
        let with_msg = LayerHealthEntry {
            layer: "HOT_PATH".into(), state: "DEGRADED".into(),
            is_operational: true, message: Some("tick=1569787 interval=200ms".into()),
        };
        let s  = serde_json::to_string(&with_msg).unwrap();
        let e2: LayerHealthEntry = serde_json::from_str(&s).unwrap();
        assert_eq!(e2.message.as_deref(), Some("tick=1569787 interval=200ms"));

        let no_msg_json = r#"{"layer":"RELAY","state":"HEALTHY","is_operational":true}"#;
        let e3: LayerHealthEntry = serde_json::from_str(no_msg_json).unwrap();
        assert!(e3.message.is_none());
    }

    #[test]
    fn dao_fee_bps_max_1000() {
        let r = DaoFeeResponse {
            dao_fee_bps: 500, dao_fee_pct: 5.0,
            per_transfer_cap_eth: 50.0, daily_cap_eth: 500.0, confirmation_depth: 12,
        };
        assert!(r.dao_fee_bps <= 1000);
        assert!((r.dao_fee_pct - r.dao_fee_bps as f64 / 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn blacklist_response_round_trip() {
        let b  = BlacklistResponse { entry_count: 3, path: "config/blacklist.toml".into(), is_empty: false };
        let s  = serde_json::to_string(&b).unwrap();
        let b2: BlacklistResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(b, b2);
    }

    #[test]
    fn revert_response_field_name() {
        let r = RevertResponse { reverted_to_version: 7, win_rate: 0.72, sample_count: 7000 };
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert!(v.get("reverted_to_version").is_some(), "field name must be reverted_to_version");
    }

    #[test]
    fn gas_model_checkpoint_partial_eq() {
        use chrono::TimeZone;
        let c = GasModelCheckpoint {
            version: 3, win_rate: 0.71, sample_count: 5000,
            baseline_win_rate: 0.65,
            saved_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        };
        assert_eq!(c.clone(), c);
    }

    /// Confirms CeilingStatusResponse deserialises the exact payload the
    /// live backend sends (captured via curl against a running
    /// ops/control-plane instance) — this is the regression test for the
    /// "missing field `features`" bug.
    #[test]
    fn ceiling_status_matches_live_backend_payload() {
        let json = r#"{"paused":false,"consecutive_ceiling_hits":0,"escalation_threshold":100,"trigger_key":null,"last_hit_at":null,"paused_at":null}"#;
        let cs: CeilingStatusResponse = serde_json::from_str(json).unwrap();
        assert!(!cs.paused);
        assert_eq!(cs.consecutive_ceiling_hits, 0);
        assert_eq!(cs.escalation_threshold, 100);
        assert!(cs.trigger_key.is_none());
        assert!(cs.last_hit_at.is_none());
        assert!(cs.paused_at.is_none());
    }

    #[test]
    fn ceiling_status_round_trip_with_values() {
        let cs = CeilingStatusResponse {
            paused: true,
            consecutive_ceiling_hits: 4,
            escalation_threshold: 100,
            trigger_key: Some("ARBITRUM_LA".into()),
            last_hit_at: Some(Utc::now()),
            paused_at: Some(Utc::now()),
        };
        let s   = serde_json::to_string(&cs).unwrap();
        let cs2: CeilingStatusResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(cs, cs2);
    }
}

