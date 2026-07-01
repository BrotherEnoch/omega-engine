// crates/omega-control-contracts/src/health.rs
//
// Frontend-side health type aliases and parsing helpers.
//
// The backend serialises health as plain strings (LayerId::to_string(),
// HealthState::to_string()). The frontend receives LayerHealthEntry from
// rest.rs and uses these helpers to interpret the string values.
//
// ## Backend LayerId string values (omega-core, 16 layers v12)
//
//   These are the CANONICAL v12 names — see
//   crates/omega-core/src/types/health.rs's `LayerId` enum, whose
//   `#[strum(serialize_all = "SCREAMING_SNAKE_CASE")]` Display impl is
//   what `ops/control-plane`'s `get_health` handler literally calls
//   (`l.layer_id().to_string()`) to build every `LayerHealthEntry.layer`
//   string. Confirmed against a live `GET /api/v1/health` response
//   during development:
//
//     HEALTH | RPC | ORACLE | SECURITY | COMPLIANCE | RISK | DAG | ZK |
//     FLASH_LOAN | RELAY | GAS_WAR | LOSS_ATTRIBUTION | ADDRESS_ROTATION |
//     STRATEGIES | HOT_PATH | OBSERVABILITY
//
//   PRE-V12 NOTE: an earlier version of this file (and of
//   ops/control-plane's own layer-construction code) used a different,
//   pre-v12 naming convention — SYSTEM_HEALTH, EXTERNAL_DATA, EIL,
//   CHAOS_GUARD, STRATEGY, FLASHLOAN, ORCHESTRATOR, VAULT. The backend's
//   `omega_core::LayerId` keeps those as back-compat associated
//   constants (e.g. `LayerId::SystemHealth` is a const alias for
//   `LayerId::Health`), but its *Display* output — the actual string
//   that travels over the wire — has always been the canonical name.
//   This frontend copy of `LayerId` previously hardcoded the pre-v12
//   strings as its `backend_str()` output, which silently broke every
//   layer lookup once the backend was confirmed to be sending canonical
//   names: every `HealthSnapshot` entry's `layer` field failed to match
//   any `LayerId::from_backend_str()` pattern, and (on the
//   `omega-frontend` consumer side) `layer_backend_key()`'s output
//   never matched a real entry either, so every layer fell back to
//   `HealthStatus::Unknown` regardless of what the backend reported.
//
//   This file's `LayerId` also previously had a `ChaosGuard` variant
//   with no canonical-backend counterpart at all — `ChaosGuard` was
//   only ever a *pre-v12 alias for `Security`* on the backend, never a
//   distinct sixteenth layer. Meanwhile `Oracle` (a genuinely distinct
//   real layer, L2 in the v12 architecture) had no representation here.
//   `ChaosGuard` has been renamed to `Oracle` below to correct this:
//   the enum's variant count and ordinal position are unchanged (still
//   16 variants, still position 5), only the name and its associated
//   backend string changed.
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
            "HEALTHY" | "OK" => Self::Ok,
            "DEGRADED"       => Self::Degraded,
            "HALTED"         => Self::Halted,
            "RECOVERING"     => Self::Recovering,
            _                => Self::Unknown,
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
    /// L00 — Health FSM, persistence, halt propagation (backend: "HEALTH")
    SystemHealth,
    /// L01 — RPC connectivity / external data feeds (backend: "RPC")
    ExternalData,
    /// L02 — Oracle price feeds and staleness detection (backend: "ORACLE")
    Oracle,
    /// L03 — Risk engine (backend: "RISK")
    Risk,
    /// L04 — Security policy enforcement (backend: "SECURITY")
    Security,
    /// L05 — OFA compliance / EIL execution cache (backend: "COMPLIANCE")
    Eil,
    /// L06 — DAG planner (backend: "DAG")
    Dag,
    /// L07 — ZK prover (backend: "ZK")
    Zk,
    /// L08 — Hot path executor (backend: "HOT_PATH")
    HotPath,
    /// L09 — Strategy orchestrator (backend: "STRATEGIES")
    Strategy,
    /// L10 — Flash loan coordinator (backend: "FLASH_LOAN")
    Flashloan,
    /// L11 — Gas War Engine / orchestrator (backend: "GAS_WAR")
    Orchestrator,
    /// L12 — Relay client (backend: "RELAY")
    Relay,
    /// L13 — Address rotation & relay reputation (backend: "ADDRESS_ROTATION")
    Vault,
    /// L14 — Observability (backend: "OBSERVABILITY")
    Observability,
    /// L15 — Loss Attribution Engine (backend: "LOSS_ATTRIBUTION")
    LossAttribution,
}

impl LayerId {
    /// The backend string representation for this layer.
    pub fn backend_str(&self) -> &'static str {
        match self {
            Self::SystemHealth    => "HEALTH",
            Self::ExternalData    => "RPC",
            Self::Oracle          => "ORACLE",
            Self::Risk            => "RISK",
            Self::Security        => "SECURITY",
            Self::Eil             => "COMPLIANCE",
            Self::Dag             => "DAG",
            Self::Zk              => "ZK",
            Self::HotPath         => "HOT_PATH",
            Self::Strategy        => "STRATEGIES",
            Self::Flashloan       => "FLASH_LOAN",
            Self::Orchestrator    => "GAS_WAR",
            Self::Relay           => "RELAY",
            Self::Vault           => "ADDRESS_ROTATION",
            Self::Observability   => "OBSERVABILITY",
            Self::LossAttribution => "LOSS_ATTRIBUTION",
        }
    }

    /// Parse a backend layer string. Returns None for unknown strings.
    pub fn from_backend_str(s: &str) -> Option<Self> {
        match s {
            "HEALTH"            => Some(Self::SystemHealth),
            "RPC"                => Some(Self::ExternalData),
            "ORACLE"             => Some(Self::Oracle),
            "RISK"               => Some(Self::Risk),
            "SECURITY"           => Some(Self::Security),
            "COMPLIANCE"         => Some(Self::Eil),
            "DAG"                => Some(Self::Dag),
            "ZK"                 => Some(Self::Zk),
            "HOT_PATH"           => Some(Self::HotPath),
            "STRATEGIES"         => Some(Self::Strategy),
            "FLASH_LOAN"         => Some(Self::Flashloan),
            "GAS_WAR"            => Some(Self::Orchestrator),
            "RELAY"              => Some(Self::Relay),
            "ADDRESS_ROTATION"   => Some(Self::Vault),
            "OBSERVABILITY"      => Some(Self::Observability),
            "LOSS_ATTRIBUTION"   => Some(Self::LossAttribution),
            _                    => None,
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
        assert_eq!(HealthStatus::from_backend_str("OK"),         HealthStatus::Ok);
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

    /// Pins backend_str()'s output to the EXACT canonical v12 strings
    /// confirmed via a live `GET /api/v1/health` against
    /// ops/control-plane during this session. This is the regression
    /// test for the "all 16 layers stuck UNKNOWN" bug: the previous
    /// version of this file passed `backend_str_round_trips` (a
    /// self-referential test) while still emitting the wrong,
    /// pre-v12 strings that never matched what the real backend sends.
    #[test]
    fn backend_str_matches_live_backend_strings() {
        assert_eq!(LayerId::SystemHealth.backend_str(),    "HEALTH");
        assert_eq!(LayerId::ExternalData.backend_str(),    "RPC");
        assert_eq!(LayerId::Oracle.backend_str(),          "ORACLE");
        assert_eq!(LayerId::Risk.backend_str(),            "RISK");
        assert_eq!(LayerId::Security.backend_str(),        "SECURITY");
        assert_eq!(LayerId::Eil.backend_str(),             "COMPLIANCE");
        assert_eq!(LayerId::Dag.backend_str(),             "DAG");
        assert_eq!(LayerId::Zk.backend_str(),              "ZK");
        assert_eq!(LayerId::HotPath.backend_str(),         "HOT_PATH");
        assert_eq!(LayerId::Strategy.backend_str(),        "STRATEGIES");
        assert_eq!(LayerId::Flashloan.backend_str(),       "FLASH_LOAN");
        assert_eq!(LayerId::Orchestrator.backend_str(),    "GAS_WAR");
        assert_eq!(LayerId::Relay.backend_str(),           "RELAY");
        assert_eq!(LayerId::Vault.backend_str(),           "ADDRESS_ROTATION");
        assert_eq!(LayerId::Observability.backend_str(),   "OBSERVABILITY");
        assert_eq!(LayerId::LossAttribution.backend_str(), "LOSS_ATTRIBUTION");
    }

    /// Every backend_str() output must be unique, or two LayerId
    /// variants would silently collapse onto the same HealthSnapshot
    /// entry (the prior bug: Security and the old ChaosGuard variant
    /// both effectively excluded Oracle from ever being represented).
    #[test]
    fn backend_str_produces_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for layer in LayerId::iter() {
            let s = layer.backend_str();
            assert!(seen.insert(s), "backend_str produced a duplicate value {s:?} for {layer:?}");
        }
    }
}