ï»¿// crates/omega-core/src/types/health.rs
//
// Health FSM primitives shared across every layer (Â§3).
//
// The 14-layer health model (Â§2, Â§3) requires every layer to expose a
// uniform interface so the SystemHealth orchestrator can propagate halts
// top-down and aggregate Degraded states.  This module owns:
//
//   HealthState  â€” the three FSM states (Healthy â†’ Degraded â†’ Halted)
//   LayerId      â€” canonical identifier for each of the 14 architecture
//                  layers; used in metrics labels and halt-propagation
//                  routing (Â§3)
//   LayerHealth  â€” the trait every layer-local health controller must
//                  implement

use serde::{Deserialize, Serialize};

/// Three-state FSM for a single architecture layer (Â§3).
///
/// Transitions:
///   Healthy  â†’ Degraded  (non-fatal anomaly; system keeps running with
///                          reduced capacity or elevated latency)
///   Degraded â†’ Halted    (threshold crossed or manual halt; layer stops
///                          processing)
///   Halted   â†’ Healthy   (recovery â€” requires governance clearance for
///                          critical layers, automatic for transient faults)
///
/// A layer at Halted MUST NOT produce ExecutionBlueprints or submit
/// bundles.  The DAG (Â§9) enforces this via dependency propagation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Healthy,
    Degraded,
    Halted,
}

impl HealthState {
    /// Returns `true` when the layer may continue normal operation.
    /// Both Healthy and Degraded are operational; Halted is not.
    #[inline]
    pub fn is_operational(self) -> bool {
        self != HealthState::Halted
    }

    /// Returns `true` only when fully healthy â€” no degradation.
    #[inline]
    pub fn is_healthy(self) -> bool {
        self == HealthState::Healthy
    }
}

impl std::fmt::Display for HealthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HealthState::Healthy  => f.write_str("HEALTHY"),
            HealthState::Degraded => f.write_str("DEGRADED"),
            HealthState::Halted   => f.write_str("HALTED"),
        }
    }
}

/// Canonical identifiers for each of the 14 architecture layers (Â§2).
///
/// Used in:
///   - Prometheus metric labels (`layer="relay"`)
///   - Halt-propagation routing in the SystemHealth orchestrator (Â§3)
///   - Loss Attribution sub-classification (Â§13.4)
///   - Observability event payloads (Â§16)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerId {
    SystemHealth,
    ExternalData,
    Eil,
    Risk,
    Security,
    ChaosGuard,
    Dag,
    Zk,
    HotPath,
    Strategy,
    Flashloan,
    Orchestrator,
    Relay,
    Vault,
    Observability,
    /// Loss Attribution Engine (Â§13) â€” treated as a sub-layer of
    /// Strategy for halt propagation but tracked independently in
    /// metrics because ceiling escalation (Â§13.3) can trigger an
    /// independent DEGRADED state.
    LossAttribution,
}

impl std::fmt::Display for LayerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Produces the canonical snake_case label used in Prometheus
        // and ELK payloads.  Matches serde rename_all = "snake_case".
        let s = match self {
            LayerId::SystemHealth    => "system_health",
            LayerId::ExternalData    => "external_data",
            LayerId::Eil             => "eil",
            LayerId::Risk            => "risk",
            LayerId::Security        => "security",
            LayerId::ChaosGuard      => "chaos_guard",
            LayerId::Dag             => "dag",
            LayerId::Zk              => "zk",
            LayerId::HotPath         => "hot_path",
            LayerId::Strategy        => "strategy",
            LayerId::Flashloan       => "flashloan",
            LayerId::Orchestrator    => "orchestrator",
            LayerId::Relay           => "relay",
            LayerId::Vault           => "vault",
            LayerId::Observability   => "observability",
            LayerId::LossAttribution => "loss_attribution",
        };
        f.write_str(s)
    }
}

/// Uniform health interface every layer-local controller must implement.
///
/// Implementors live in their respective crates (omega-health,
/// omega-relay, omega-strategies â€¦).  omega-core owns the trait so
/// every crate in the dependency graph can hold `Arc<dyn LayerHealth>`
/// without pulling in the concrete implementations.
///
/// Thread safety: `Send + Sync` required â€” all layer controllers are
/// shared across the Tokio runtime via Arc.
pub trait LayerHealth: Send + Sync {
    /// Current FSM state of this layer.
    fn state(&self) -> HealthState;

    /// Transition this layer to `new_state`, recording `reason` in the
    /// telemetry stream.  Implementations MUST emit a tracing event at
    /// the appropriate level (WARN for Degraded, ERROR for Halted).
    fn set_state(&self, new_state: HealthState, reason: &str);

    /// Convenience â€” returns `true` when the layer may continue
    /// processing (Healthy or Degraded).
    #[inline]
    fn is_operational(&self) -> bool {
        self.state().is_operational()
    }

    /// Canonical layer identifier.  Used by the SystemHealth
    /// orchestrator for metric labelling and halt routing (Â§3).
    fn layer_id(&self) -> LayerId;
}