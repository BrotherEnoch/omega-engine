// crates/omega-control-contracts/src/health.rs
//
// Frontend-side health type aliases and parsing helpers.
//
// The backend serialises health as plain strings (LayerId::to_string(),
// HealthState::to_string()). The frontend receives LayerHealthEntry from
// rest.rs and uses these helpers to interpret the string values.
//
// ## Backend LayerId string values (omega-core, 16 layers v12)
// SYSTEM_HEALTH | EXTERNAL_DATA | EIL | RISK | SECURITY | CHAOS_GUARD |
// DAG | ZK | HOT_PATH | STRATEGY | FLASHLOAN | ORCHESTRATOR | RELAY |
// VAULT | OBSERVABILITY | LOSS_ATTRIBUTION
//
// ## Backend HealthState string values
// HEALTHY | DEGRADED | HALTED | RECOVERING | UNKNOWN

use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter, EnumString};

// ---------------------------------------------------------------------------
// HealthStatus — frontend representation of backend HealthState strings
// ---------------------------------------------------------------------------

/// Frontend health status, parsed from backend state strings.
///
/// Maps: "HEALTHY" → Ok, "DEGRADED" → Degraded, "HALTED" → Halted,
///       "RECOVERING" → Recovering, anything else → Unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display)]
pub enum HealthStatus {
    Ok,
    Degraded,
    Halted,
    Recovering,
    Unknown,
}

impl HealthStatus {
    /// Parse a backend state string into a HealthStatus.
    pub fn from_backend_str(s: &str) -> Self {
        match s {
            "HEALTHY"    => Self::Ok,
            "DEGRADED"   => Self::Degraded,
            "HALTED"     => Self::Halted,
            "RECOVERING" => Self::Recovering,
            _            => Self::Unknown,
        }
    }
}

// ---------------------------------------------------------------------------
// LayerId — frontend layer identifier enum
//
// These map to the backend omega-core LayerId string values.
// The frontend uses these for display, ordering, and array indexing.
// ---------------------------------------------------------------------------

/// Frontend layer identifier. Ordered to match the backend layer array.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash,
    Serialize, Deserialize,
    Display, EnumIter, EnumString,
)]
pub enum LayerId {
    /// L00 — SystemHealth (backend: "SYSTEM_HEALTH")
    SystemHealth,
    /// L01 — ExternalData / RPC feeds (backend: "EXTERNAL_DATA")
    ExternalData,
    /// L02 — EIL execution cache (backend: "EIL")
    Eil,
    /// L03 — Risk engine (backend: "RISK")
    Risk,
    /// L04 — Security policy (backend: "SECURITY")
    Security,
    /// L05 — ChaosGuard (backend: "CHAOS_GUARD")
    ChaosGuard,
    /// L06 — DAG planner (backend: "DAG")
    Dag,
    /// L07 — ZK prover (backend: "ZK")
    Zk,
    /// L08 — Hot path executor (backend: "HOT_PATH")
    HotPath,
    /// L09 — Strategy orchestrator (backend: "STRATEGY")
    Strategy,
    /// L10 — Flash loan coordinator (backend: "FLASHLOAN")
    Flashloan,
    /// L11 — Orchestrator (backend: "ORCHESTRATOR")
    Orchestrator,
    /// L12 — Relay client (backend: "RELAY")
    Relay,
    /// L13 — Vault (backend: "VAULT")
    Vault,
    /// L14 — Observability (backend: "OBSERVABILITY")
    Observability,
    /// L15 — Loss Attribution (backend: "LOSS_ATTRIBUTION")
    LossAttribution,
}

impl LayerId {
    /// The backend string representation for this layer.
    pub fn backend_str(&self) -> &'static str {
        match self {
            Self::SystemHealth   => "SYSTEM_HEALTH",
            Self::ExternalData   => "EXTERNAL_DATA",
            Self::Eil            => "EIL",
            Self::Risk           => "RISK",
            Self::Security       => "SECURITY",
            Self::ChaosGuard     => "CHAOS_GUARD",
            Self::Dag            => "DAG",
            Self::Zk             => "ZK",
            Self::HotPath        => "HOT_PATH",
            Self::Strategy       => "STRATEGY",
            Self::Flashloan      => "FLASHLOAN",
            Self::Orchestrator   => "ORCHESTRATOR",
            Self::Relay          => "RELAY",
            Self::Vault          => "VAULT",
            Self::Observability  => "OBSERVABILITY",
            Self::LossAttribution => "LOSS_ATTRIBUTION",
        }
    }

    /// Parse a backend layer string. Returns None for unknown strings.
    pub fn from_backend_str(s: &str) -> Option<Self> {
        match s {
            "SYSTEM_HEALTH"    => Some(Self::SystemHealth),
            "EXTERNAL_DATA"    => Some(Self::ExternalData),
            "EIL"              => Some(Self::Eil),
            "RISK"             => Some(Self::Risk),
            "SECURITY"         => Some(Self::Security),
            "CHAOS_GUARD"      => Some(Self::ChaosGuard),
            "DAG"              => Some(Self::Dag),
            "ZK"               => Some(Self::Zk),
            "HOT_PATH"         => Some(Self::HotPath),
            "STRATEGY"         => Some(Self::Strategy),
            "FLASHLOAN"        => Some(Self::Flashloan),
            "ORCHESTRATOR"     => Some(Self::Orchestrator),
            "RELAY"            => Some(Self::Relay),
            "VAULT"            => Some(Self::Vault),
            "OBSERVABILITY"    => Some(Self::Observability),
            "LOSS_ATTRIBUTION" => Some(Self::LossAttribution),
            _                  => None,
        }
    }
}

// ---------------------------------------------------------------------------
// LayerHealth — parsed frontend view of a LayerHealthEntry
// ---------------------------------------------------------------------------

/// Parsed layer health — converted from the backend's string-typed
/// `LayerHealthEntry` into typed enums for frontend consumption.
#[derive(Debug, Clone)]
pub struct LayerHealth {
    pub layer:         LayerId,
    pub status:        HealthStatus,
    pub is_operational: bool,
    pub message:       Option<String>,
    pub updated_at_ms: Option<u64>,
}

impl LayerHealth {
    /// Parse from a backend `LayerHealthEntry`.
    /// Returns `None` if the layer string is unrecognised.
    pub fn from_entry(entry: &crate::rest::LayerHealthEntry) -> Option<Self> {
        let layer  = LayerId::from_backend_str(&entry.layer)?;
        let status = HealthStatus::from_backend_str(&entry.state);
        Some(Self {
            layer,
            status,
            is_operational: entry.is_operational,
            message:        None,
            updated_at_ms:  None,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;

    #[test]
    fn all_16_layers_present() {
        assert_eq!(LayerId::iter().count(), 16,
            "Layer count must be 16 per v12 spec");
    }

    #[test]
    fn backend_str_round_trips() {
        for layer in LayerId::iter() {
            let s     = layer.backend_str();
            let back  = LayerId::from_backend_str(s).unwrap();
            assert_eq!(back, layer, "round-trip failed for {layer:?}");
        }
    }

    #[test]
    fn health_status_parsing() {
        assert_eq!(HealthStatus::from_backend_str("HEALTHY"),    HealthStatus::Ok);
        assert_eq!(HealthStatus::from_backend_str("DEGRADED"),   HealthStatus::Degraded);
        assert_eq!(HealthStatus::from_backend_str("HALTED"),     HealthStatus::Halted);
        assert_eq!(HealthStatus::from_backend_str("RECOVERING"), HealthStatus::Recovering);
        assert_eq!(HealthStatus::from_backend_str("UNKNOWN"),    HealthStatus::Unknown);
        assert_eq!(HealthStatus::from_backend_str("garbage"),    HealthStatus::Unknown);
    }

    #[test]
    fn layer_health_from_entry() {
        let entry = crate::rest::LayerHealthEntry {
            layer: "RELAY".into(),
            state: "HALTED".into(),
            is_operational: false,
        };
        let lh = LayerHealth::from_entry(&entry).unwrap();
        assert_eq!(lh.layer,  LayerId::Relay);
        assert_eq!(lh.status, HealthStatus::Halted);
        assert!(!lh.is_operational);
    }

    #[test]
    fn unknown_layer_returns_none() {
        let entry = crate::rest::LayerHealthEntry {
            layer: "NONEXISTENT_LAYER".into(),
            state: "HEALTHY".into(),
            is_operational: true,
        };
        assert!(LayerHealth::from_entry(&entry).is_none());
    }
}