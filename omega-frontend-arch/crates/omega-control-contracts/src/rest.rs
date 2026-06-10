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

/// Generic error body returned on 4xx/5xx.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub error:   String,
    pub message: String,
}

impl ApiError {
    pub fn new(error: impl Into<String>, message: impl Into<String>) -> Self {
        Self { error: error.into(), message: message.into() }
    }
}

/// Generic success body returned by mutation endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiOk {
    pub status: &'static str,
}

pub const OK: ApiOk = ApiOk { status: "ok" };

// ---------------------------------------------------------------------------
// Health  — matches GET /api/v1/health backend handler exactly
// ---------------------------------------------------------------------------

/// A single layer entry as returned by the backend.
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
    pub generated_at:   DateTime<Utc>,
    /// Per-layer health entries (16 layers in v12).
    pub layers:         Vec<LayerHealthEntry>,
    /// True if any layer is in the HALTED state.
    pub system_halted:  bool,
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
    pub body: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Gas model checkpoints  — matches GET /api/v1/la/gas-model/checkpoints
// ---------------------------------------------------------------------------

/// A single checkpoint entry as returned by `checkpoint::list_checkpoints`.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureCeilingStatus {
    pub feature_key:       String,
    pub multiplier:        f64,
    pub ceiling_hit_count: u64,
    pub paused:            bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CeilingStatusResponse {
    pub features:   Vec<FeatureCeilingStatus>,
    pub any_paused: bool,
}

// ---------------------------------------------------------------------------
// Vault / DAO fee  — matches GET /api/v1/vault/dao-fee backend handler exactly
//
// Backend builds this from VaultConfig:
//   dao_fee_bps, dao_fee_pct, per_transfer_cap_eth, daily_cap_eth, confirmation_depth
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
//
// Backend builds this directly from BuilderBlacklist:
//   entry_count, path (display string), is_empty
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
    fn api_error_round_trip() {
        let e = ApiError::new("NOT_FOUND", "checkpoint 99 does not exist");
        let s = serde_json::to_string(&e).unwrap();
        let e2: ApiError = serde_json::from_str(&s).unwrap();
        assert_eq!(e, e2);
    }

    #[test]
    fn health_snapshot_round_trip() {
        let snap = HealthSnapshot {
            generated_at:  Utc::now(),
            layers: vec![
                LayerHealthEntry { layer: "RELAY".into(), state: "HEALTHY".into(), is_operational: true },
                LayerHealthEntry { layer: "LOSS_ATTRIBUTION".into(), state: "HALTED".into(), is_operational: false },
            ],
            system_halted: true,
        };
        let s  = serde_json::to_string(&snap).unwrap();
        let s2: HealthSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(s2.layers.len(), 2);
        assert!(s2.system_halted);
        assert_eq!(s2.overall_state(), "HALTED");
    }

    #[test]
    fn layer_health_entry_helpers() {
        let halted = LayerHealthEntry { layer: "HOT_PATH".into(), state: "HALTED".into(), is_operational: false };
        assert!(halted.is_halted());
        assert!(!halted.is_healthy());
        let healthy = LayerHealthEntry { layer: "RELAY".into(), state: "HEALTHY".into(), is_operational: true };
        assert!(healthy.is_healthy());
        assert!(!healthy.is_halted());
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
        let b = BlacklistResponse { entry_count: 3, path: "config/blacklist.toml".into(), is_empty: false };
        let s = serde_json::to_string(&b).unwrap();
        let b2: BlacklistResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(b, b2);
    }

    #[test]
    fn revert_response_field_name() {
        // Field must be reverted_to_version not reverted_to or version
        let r = RevertResponse { reverted_to_version: 7, win_rate: 0.72, sample_count: 7000 };
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert!(v.get("reverted_to_version").is_some(), "field name must be reverted_to_version");
    }
}