// crates\omega-control-contracts\src\ws.rs
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
    // ── Trading-engine telemetry (bridged from OmegaEvent via obs_bridge) ──
    GasModelReverted {
        checkpoint_version: u64,
        win_rate: f64,
        sample_count: u64,
        timestamp: DateTime<Utc>,
    },
    GasModelCeilingEscalation {
        feature_key: String,
        ceiling_hit_count: u64,
        timestamp: DateTime<Utc>,
    },
    EmergencyBundleSkipped {
        blueprint_hash: String,
        emergency_fee_gwei: f64,
        reason: String,
        timestamp: DateTime<Utc>,
    },
    ProfitSplit {
        blueprint_hash: String,
        pil_share_wei: String,
        dao_fee_wei: String,
        timestamp: DateTime<Utc>,
    },
    LaReorgRisk {
        tx_hash: String,
        orphaned_block: u64,
        reorg_depth: u64,
        timestamp: DateTime<Utc>,
    },
    BlueprintConfirmed {
        blueprint_hash: String,
        strategy_id: String,
        block_number: u64,
        profit_net_eth: f64,
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