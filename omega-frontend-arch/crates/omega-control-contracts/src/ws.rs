// crates/omega-control-contracts/src/ws.rs
use serde::{Deserialize, Serialize};

use crate::health::{HealthStatus, LayerId};

// ---------------------------------------------------------------------------
// Rate limit constants (v12 §17.1)
// ---------------------------------------------------------------------------

pub const WS_CHANNEL_CAPACITY:    usize = 512;
pub const AUTHED_LIMIT_PER_MINUTE: u32  = 300;
pub const ANON_LIMIT_PER_MINUTE:   u32  = 100;

// Struct alias kept for compatibility with omega-frontend imports.
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
// WsEvent — tagged union
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WsEvent {
    HealthUpdate(HealthUpdateEvent),
    HaltPropagation(HaltEvent),
    GasModelReverted(GasModelRevertedEvent),
    GasModelCeilingEscalation(GasModelCeilingEvent),
    EmergencyBundleSkipped(EmergencyBundleSkippedEvent),
    LaReorgRisk(LaReorgRiskEvent),
    ProfitSplit(ProfitSplitEvent),
    SimulationError(SimulationErrorEvent),
    Ping { nonce: u64 },
}

// ---------------------------------------------------------------------------
// Event payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthUpdateEvent {
    pub changes:  Vec<LayerStatusChange>,
    pub overall:  HealthStatus,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerStatusChange {
    pub layer:    LayerId,
    pub previous: HealthStatus,
    pub current:  HealthStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message:  Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HaltEvent {
    pub source_layer:    LayerId,
    pub affected_layers: Vec<LayerId>,
    pub reason:          String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GasModelRevertedEvent {
    pub checkpoint_version: u64,
    pub win_rate:           f64,
    pub degraded_win_rate:  f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GasModelCeilingEvent {
    pub feature_key:       String,
    pub ceiling_hit_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmergencyBundleSkippedEvent {
    pub blueprint_hash:     String,
    pub reason:             String,
    pub emergency_fee_gwei: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaReorgRiskEvent {
    pub tx_hash:          String,
    pub orphaned_block:   u64,
    pub rescore_at_block: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfitSplitEvent {
    pub blueprint_hash:  String,
    pub pil_share_wei:   String,
    pub dao_fee_wei:     String,
    pub dao_fee_address: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulationErrorEvent {
    pub blueprint_hash: String,
    pub sub_code:       SimulationErrorSubCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail:         Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SimulationErrorSubCode {
    StateMismatch,
    ExecutionRevert,
    GasMiscalc,
}

// ---------------------------------------------------------------------------
// Auth frames
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
    fn health_update_round_trip() {
        let event = WsEvent::HealthUpdate(HealthUpdateEvent {
            changes:  vec![LayerStatusChange {
                layer:    LayerId::Relay,
                previous: HealthStatus::Ok,
                current:  HealthStatus::Degraded,
                message:  Some("timeout".into()),
            }],
            overall:  HealthStatus::Degraded,
            revision: 10,
        });
        let s  = serde_json::to_string(&event).unwrap();
        let e2: WsEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(event, e2);
    }

    #[test]
    fn ping_round_trip() {
        let e = WsEvent::Ping { nonce: 42 };
        let s = serde_json::to_string(&e).unwrap();
        let e2: WsEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(e, e2);
    }
}


