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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerHealthEntry {
    pub layer: String,
    pub state: String,
    pub is_operational: bool,
}
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaoFeeResponse {
    pub dao_fee_bps: u16,
    pub dao_fee_pct: f64,
    pub per_transfer_cap_eth: f64,
    pub daily_cap_eth: f64,
    pub confirmation_depth: u8,
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
}