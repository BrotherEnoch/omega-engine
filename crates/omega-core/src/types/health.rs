// crates/omega-core/src/types/health.rs
//
// Shared health enums — single source of truth for LayerId, HealthStatus,
// HealthState alias, and LayerHealth trait.
//
// ## Serialisation contract
// HealthStatus serialises with SCREAMING_SNAKE_CASE:
//   Ok          → "OK"
//   Degraded    → "DEGRADED"
//   Halted      → "HALTED"
//   Recovering  → "RECOVERING"
//   Unknown     → "UNKNOWN"
//
// Display (used by tracing % fields and .to_string()) matches the wire format:
//   Ok::fmt    → "OK"
//   Halted::fmt → "HALTED"
//   etc.
//
// This makes the backend handler's `l.state().to_string()` produce "OK" which
// matches both the serde wire format and the frontend contracts parser.
//
// ## LayerId — 16 canonical variants
// Variant names follow the v12 architecture. Back-compat associated constants
// map old pre-v12 names to their canonical equivalents so existing callers
// (omega-health, omega-chaos) continue to compile without modification.

use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter, EnumString};

// ---------------------------------------------------------------------------
// HealthStatus
// ---------------------------------------------------------------------------

/// Operational health of a single layer.
///
/// Serialises as SCREAMING_SNAKE_CASE ("OK", "DEGRADED", "HALTED", …).
/// Display produces the same strings so tracing % fields and .to_string()
/// are consistent with the wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumIter)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[strum(serialize_all = "SCREAMING_SNAKE_CASE")]
pub enum HealthStatus {
    /// Layer is operating normally. Wire: "OK".
    Ok,
    /// Layer has detected degradation but has not halted. Wire: "DEGRADED".
    Degraded,
    /// Layer has halted; halt propagation may be active. Wire: "HALTED".
    Halted,
    /// Layer is recovering from a halt. Wire: "RECOVERING".
    Recovering,
    /// Layer status is unknown (e.g. control plane just started). Wire: "UNKNOWN".
    Unknown,
}

#[allow(non_upper_case_globals)]
impl HealthStatus {
    /// Back-compat: pre-v12 code used HealthState::Healthy.
    pub const Healthy: Self = Self::Ok;

    /// Returns true when the layer is fully healthy.
    pub fn is_healthy(self) -> bool { matches!(self, Self::Ok) }

    /// Returns true when the layer can still serve traffic (not Halted).
    pub fn is_operational(self) -> bool { !matches!(self, Self::Halted) }
}

/// Back-compat type alias — pre-v12 code used HealthState everywhere.
pub type HealthState = HealthStatus;

// ---------------------------------------------------------------------------
// LayerId — 16 canonical variants (v12)
// ---------------------------------------------------------------------------

/// Identifies a layer in the 16-layer OmegaEngine architecture.
///
/// Serialises as SCREAMING_SNAKE_CASE. Display produces the same strings.
/// Back-compat associated constants map pre-v12 names to canonical equivalents.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash,
    Serialize, Deserialize,
    Display, EnumIter, EnumString,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[strum(serialize_all = "SCREAMING_SNAKE_CASE")]
pub enum LayerId {
    /// L0 — Health FSM, persistence, halt propagation
    Health,
    /// L1 — RPC connectivity (Ethereum + Arbitrum nodes)
    Rpc,
    /// L2 — Oracle price feeds and staleness detection
    Oracle,
    /// L3 — Security policy enforcement
    Security,
    /// L4 — OFA compliance (versioned rules)
    Compliance,
    /// L5 — Risk engine (dual-component gas model)
    Risk,
    /// L6 — DAG execution planner
    Dag,
    /// L7 — ZK proof subsystem (StarkVerifier)
    Zk,
    /// L8 — Flash loan coordinator
    FlashLoan,
    /// L9 — Relay client (MEV-Boost + Arbitrum sequencer)
    Relay,
    /// L10 — Gas War Engine
    GasWar,
    /// L11 — Loss Attribution Engine (ML online learner)
    LossAttribution,
    /// L12 — Address rotation & relay reputation
    AddressRotation,
    /// L13 — Strategy orchestrator (SA / MSA / LA / MEV)
    Strategies,
    /// L14 — Hot-path executor (ZK + relay fast lane)
    HotPath,
    /// L15 — Observability (metrics, events, ELK)
    Observability,
}

/// Back-compat associated constants mapping pre-v12 names to v12 variants.
/// These allow omega-health, omega-chaos, and ops/control-plane to compile
/// unmodified while the canonical names are being adopted crate by crate.
#[allow(non_upper_case_globals)]
impl LayerId {
    pub const SystemHealth:   Self = Self::Health;
    pub const ExternalData:   Self = Self::Rpc;
    pub const Eil:            Self = Self::Compliance;
    pub const ChaosGuard:     Self = Self::Security;
    pub const Strategy:       Self = Self::Strategies;
    pub const Flashloan:      Self = Self::FlashLoan;
    pub const Orchestrator:   Self = Self::GasWar;
    pub const Vault:          Self = Self::AddressRotation;
}

// ---------------------------------------------------------------------------
// LayerHealth trait
// ---------------------------------------------------------------------------

/// Mutable health-controller interface implemented by `omega-health`.
pub trait LayerHealth: Send + Sync {
    fn state(&self)    -> HealthState;
    fn layer_id(&self) -> LayerId;
    fn set_state(&self, new_state: HealthState, reason: &str);

    fn is_healthy(&self)     -> bool { self.state().is_healthy() }
    fn is_operational(&self) -> bool { self.state().is_operational() }
}

// ---------------------------------------------------------------------------
// LayerHealthReport — dual-format REST/proto snapshot
// ---------------------------------------------------------------------------

/// Health report for a single layer. Accepts both REST ("layer") and
/// proto ("layer_id") field names via serde alias.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerHealthReport {
    #[serde(alias = "layer_id")]
    pub layer:     LayerId,
    pub status:    HealthStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message:   Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<u64>,
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
        assert_eq!(LayerId::iter().count(), 16);
    }

    #[test]
    fn health_status_display_is_screaming_snake() {
        assert_eq!(HealthStatus::Ok.to_string(),        "OK");
        assert_eq!(HealthStatus::Degraded.to_string(),  "DEGRADED");
        assert_eq!(HealthStatus::Halted.to_string(),    "HALTED");
        assert_eq!(HealthStatus::Recovering.to_string(),"RECOVERING");
        assert_eq!(HealthStatus::Unknown.to_string(),   "UNKNOWN");
    }

    #[test]
    fn health_status_serde_roundtrip() {
        for s in HealthStatus::iter() {
            let json  = serde_json::to_string(&s).unwrap();
            let back: HealthStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn health_status_serde_wire_values() {
        assert_eq!(serde_json::to_string(&HealthStatus::Ok).unwrap(),        r#""OK""#);
        assert_eq!(serde_json::to_string(&HealthStatus::Halted).unwrap(),    r#""HALTED""#);
        assert_eq!(serde_json::to_string(&HealthStatus::Degraded).unwrap(),  r#""DEGRADED""#);
    }

    #[test]
    fn layer_id_display_is_screaming_snake() {
        assert_eq!(LayerId::Health.to_string(),          "HEALTH");
        assert_eq!(LayerId::LossAttribution.to_string(), "LOSS_ATTRIBUTION");
        assert_eq!(LayerId::FlashLoan.to_string(),       "FLASH_LOAN");
    }

    #[test]
    fn back_compat_aliases_resolve() {
        assert_eq!(LayerId::SystemHealth, LayerId::Health);
        assert_eq!(LayerId::ExternalData, LayerId::Rpc);
        assert_eq!(LayerId::Strategy,     LayerId::Strategies);
        assert_eq!(LayerId::Flashloan,    LayerId::FlashLoan);
        assert_eq!(LayerId::Orchestrator, LayerId::GasWar);
        assert_eq!(LayerId::Vault,        LayerId::AddressRotation);
    }

    #[test]
    fn healthy_alias_resolves() {
        assert_eq!(HealthStatus::Healthy, HealthStatus::Ok);
    }

    #[test]
    fn is_operational_halted_is_false() {
        assert!(!HealthStatus::Halted.is_operational());
        assert!(HealthStatus::Ok.is_operational());
        assert!(HealthStatus::Degraded.is_operational());
    }
}