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
