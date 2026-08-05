// omega-engine\crates\omega-control-contracts\src\rest.rs
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
    pub message: String,
}
impl ApiError {
    pub fn new(error: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiOk {
    pub status: &'static str,
}
pub const OK: ApiOk = ApiOk { status: "ok" };

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub generated_at: DateTime<Utc>,
    pub layers: Vec<LayerHealthEntry>,
    pub system_halted: bool,
}

// CHANGES:
//   - `layer` renamed to `layer_id` — matches `proto::LayerHealth.layer_id` exactly.
//     The prior mismatched naming (proto: layer_id, rest: layer) for the same
//     identifier had no functional impact but was an unforced inconsistency
//     between the two contract layers in this crate. BREAKING for any existing
//     REST/JSON consumer expecting the `"layer"` key.
//   - `reason: String` added — proto's `LayerHealth` already carries this (why a
//     layer is in its current state); REST/JSON consumers (the WASM frontend)
//     previously had no way to see it, only a derived `is_operational` boolean.
//     Defaults to empty string when there's nothing to report, matching proto's
//     own non-optional string convention rather than introducing `Option`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerHealthEntry {
    pub layer_id: String,
    pub state: String,
    /// INVARIANT NOT ENFORCED BY THIS TYPE: whoever populates this struct must keep
    /// `is_operational` consistent with `state`. I didn't derive it automatically
    /// here because I don't have the actual set of valid `state` strings (the L0
    /// Health FSM's state names) — inventing a string-match mapping without that
    /// would be guessing at business logic, not fixing a bug. If you give me the
    /// valid state values and which count as operational, I can turn this into a
    /// computed method instead of a separately-set field, closing off the
    /// possibility of the two disagreeing.
    pub is_operational: bool,
    pub reason: String,
}

// CHANGE: `from_disk`/`body` are still an intentionally loose contract, not
// tightened here — the actual set of reloadable config fields is real
// backend-specific knowledge I don't have, and inventing a typed schema for it
// would mean guessing at fields that might not match what the backend actually
// accepts. If there IS a known, finite set of reloadable fields, replace `body`
// with a real struct; if arbitrary override maps are genuinely intentional,
// this comment at least makes that explicit instead of leaving it silent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigReloadRequest {
    pub from_disk: bool,
    pub body: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevertResponse {
    pub reverted_to_version: u64,
    pub win_rate: f64,
    pub sample_count: u64,
}

// CHANGE: `dao_fee_pct` is no longer a stored field — it was redundant with
// `dao_fee_bps` (presumably `dao_fee_bps as f64 / 100.0` computed somewhere in
// the backend) with nothing enforcing the two stayed in agreement if they were
// ever set independently. Converted to a method that can only ever be correct,
// since it's derived at the point of use rather than carried and trusted.
// BREAKING for JSON consumers: `dao_fee_pct` is no longer a field in the
// serialized output. Re-add a `#[serde(rename = "dao_fee_pct")]` computed
// value via a custom Serialize impl if wire compatibility with existing
// frontend code matters more than closing off the drift risk — that's a
// tradeoff call, not something to make silently.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaoFeeResponse {
    pub dao_fee_bps: u16,
    pub per_transfer_cap_eth: f64,
    pub daily_cap_eth: f64,
    pub confirmation_depth: u8,
}
impl DaoFeeResponse {
    /// `dao_fee_bps` expressed as a percentage, computed on demand.
    pub fn dao_fee_pct(&self) -> f64 {
        f64::from(self.dao_fee_bps) / 100.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlacklistResponse {
    pub entry_count: usize,
    pub path: String,
    pub is_empty: bool,
}

// Matches GET /api/v1/la/gas-model/ceiling-status exactly -- this is the
// JSON CeilingEscalationTracker::snapshot() actually emits in
// ops/control-plane (confirmed live via curl against a running server):
//   {"paused":false,"consecutive_ceiling_hits":0,"escalation_threshold":100,
//    "trigger_key":null,"last_hit_at":null,"paused_at":null}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CeilingStatusResponse {
    pub paused: bool,
    pub consecutive_ceiling_hits: u64,
    pub escalation_threshold: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_hit_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let s = serde_json::to_string(&cs).unwrap();
        let cs2: CeilingStatusResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(cs, cs2);
    }

    #[test]
    fn dao_fee_pct_is_computed_correctly() {
        let r = DaoFeeResponse {
            dao_fee_bps: 500,
            per_transfer_cap_eth: 50.0,
            daily_cap_eth: 500.0,
            confirmation_depth: 12,
        };
        assert!((r.dao_fee_pct() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn layer_health_entry_serializes_with_layer_id_key() {
        let entry = LayerHealthEntry {
            layer_id: "L0".into(),
            state: "healthy".into(),
            is_operational: true,
            reason: String::new(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"layer_id\":\"L0\""));
        assert!(!json.contains("\"layer\":"));
    }
}
