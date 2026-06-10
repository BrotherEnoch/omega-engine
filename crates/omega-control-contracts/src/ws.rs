use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const WS_CHANNEL_CAPACITY: usize = 512;
pub const AUTHED_LIMIT_PER_MINUTE: u32 = 300;
pub const ANON_LIMIT_PER_MINUTE: u32 = 100;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WsEvent {
    HealthTransition {
        layer: String,
        from: String,
        to: String,
        reason: String,
        timestamp: DateTime<Utc>,
    },
    ModelPauseChanged {
        paused: bool,
        timestamp: DateTime<Utc>,
    },
    BlacklistReloaded {
        entry_count: usize,
        timestamp: DateTime<Utc>,
    },
    ConfigReloaded {
        timestamp: DateTime<Utc>,
    },
    CeilingEscalation {
        consecutive_hits: u64,
        paused: bool,
        timestamp: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsClientFrame {
    Auth { token: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsAuthFrame {
    AuthOk {
        rate_limit: u32,
        window_secs: u64,
    },
    AuthFailed {
        rate_limit: u32,
        window_secs: u64,
    },
}
