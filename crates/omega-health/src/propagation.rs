// crates/omega-health/src/propagation.rs
//
// Health FSM propagation — SystemHealth orchestrator and halt cascade.
//
// Spec §3:
//   When any layer transitions to Halted, the SystemHealth orchestrator
//   must propagate the halt to all layers that depend on it, top-down,
//   within the 200ms SLA.  The propagation order follows the dependency
//   graph in §22.1.
//
// ## Architecture
//
//   TransitionSender     — held by each LayerHealthImpl; sends transition
//                          events into a bounded Tokio channel.
//   TransitionReceiver   — owned by the SystemHealth propagation task;
//                          receives events and drives halt cascade.
//   PropagationRouter    — encodes the §22.1 dependency graph; given a
//                          halted layer, returns the set of dependent
//                          layers that must also halt.
//   SystemHealthOrchestrator — async task that drains the channel and
//                          applies the propagation rules.
//
// ## Channel sizing
//
//   Capacity = 256 events.  At one transition per layer per second
//   (generous upper bound) this is 18 seconds of headroom.  Back-
//   pressure is intentional: if the channel fills, `send_nonblocking`
//   drops the event with an ERROR log so the propagation task falling
//   behind is immediately visible in telemetry.
//
// ## Halt propagation order (§22.1 dependency graph, top-down)
//
//   SystemHealth  → everything
//   ExternalData  → Eil, Strategy, HotPath
//   Eil           → Strategy, HotPath, Orchestrator
//   Risk          → Strategy, Orchestrator
//   Security      → everything (same as SystemHealth)
//   ChaosGuard    → Strategy, Orchestrator
//   Dag           → Strategy, Orchestrator
//   Zk            → Vault
//   HotPath       → (no dependents — leaf)
//   Strategy      → Orchestrator, Relay
//   Flashloan     → Strategy, Orchestrator
//   Orchestrator  → Relay, Vault
//   Relay         → (no dependents — leaf)
//   Vault         → (no dependents — leaf)
//   Observability → (no dependents — monitoring only)
//   LossAttribution→ Strategy (ceiling escalation pauses Strategy)

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;

use omega_core::{HealthState, LayerId};

// ─────────────────────────────────────────────────────────────────────────────
// TransitionEvent
// ─────────────────────────────────────────────────────────────────────────────

/// A single health FSM transition, sent from a layer to the propagation task.
#[derive(Debug, Clone)]
pub struct TransitionEvent {
    /// Layer that transitioned.
    pub layer: LayerId,
    /// State before the transition.
    pub from: HealthState,
    /// State after the transition.
    pub to: HealthState,
    /// Human-readable reason.
    pub reason: String,
    /// UTC timestamp of the transition.
    pub timestamp: DateTime<Utc>,
}

// ─────────────────────────────────────────────────────────────────────────────
// TransitionSender / TransitionReceiver
// ─────────────────────────────────────────────────────────────────────────────

const CHANNEL_CAPACITY: usize = 256;

/// Sending half of the propagation channel.
///
/// Cloned into each `LayerHealthImpl`.  Non-blocking send: if the
/// channel is full the event is dropped with an ERROR log — the
/// propagation task falling behind must be immediately visible.
#[derive(Clone, Debug)]
pub struct TransitionSender(mpsc::Sender<TransitionEvent>);

impl TransitionSender {
    /// Send a transition event without blocking.
    ///
    /// Drops the event with an ERROR log if the channel is full.
    pub fn send_nonblocking(&self, event: TransitionEvent) {
        match self.0.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(ev)) => {
                tracing::error!(
                    layer  = %ev.layer,
                    from   = %ev.from,
                    to     = %ev.to,
                    "Propagation channel full — transition event dropped; \
                     propagation task may be lagging",
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // Propagation task has exited — engine is shutting down.
                // Log at WARN: this is expected during graceful shutdown.
                tracing::warn!("Propagation channel closed — engine shutting down",);
            }
        }
    }
}

/// Receiving half of the propagation channel.
///
/// Owned by the `SystemHealthOrchestrator` task.
pub struct TransitionReceiver(mpsc::Receiver<TransitionEvent>);

impl TransitionReceiver {
    /// Receive the next transition event, waiting until one is available.
    pub async fn recv(&mut self) -> Option<TransitionEvent> {
        self.0.recv().await
    }
}

/// Create a matched (sender, receiver) pair for the propagation channel.
pub fn channel() -> (TransitionSender, TransitionReceiver) {
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    (TransitionSender(tx), TransitionReceiver(rx))
}

// ─────────────────────────────────────────────────────────────────────────────
// PropagationRouter
// ─────────────────────────────────────────────────────────────────────────────

/// Encodes the §22.1 dependency graph for halt propagation.
///
/// Given a layer that has transitioned to `Halted`, returns the set of
/// dependent layers that must also be halted.
///
/// The graph is built once at startup from the static spec table and
/// cached as a `HashMap<LayerId, Vec<LayerId>>`.
pub struct PropagationRouter {
    /// dependents[L] = layers that must halt when L halts.
    dependents: HashMap<LayerId, Vec<LayerId>>,
}

impl PropagationRouter {
    /// Build the router from the §22.1 static dependency spec.
    pub fn new() -> Self {
        // For readability, defined as (source, dependents) pairs.
        // Every layer that a source layer depends on — if the source
        // halts, these dependents must also halt.
        let edges: &[(LayerId, &[LayerId])] = &[
            (
                LayerId::SystemHealth,
                &[
                    LayerId::ExternalData,
                    LayerId::Eil,
                    LayerId::Risk,
                    LayerId::Security,
                    LayerId::ChaosGuard,
                    LayerId::Dag,
                    LayerId::Zk,
                    LayerId::HotPath,
                    LayerId::Strategy,
                    LayerId::Flashloan,
                    LayerId::Orchestrator,
                    LayerId::Relay,
                    LayerId::Vault,
                    LayerId::Observability,
                    LayerId::LossAttribution,
                ],
            ),
            (
                LayerId::Security,
                &[
                    LayerId::ExternalData,
                    LayerId::Eil,
                    LayerId::Risk,
                    LayerId::ChaosGuard,
                    LayerId::Dag,
                    LayerId::Zk,
                    LayerId::HotPath,
                    LayerId::Strategy,
                    LayerId::Flashloan,
                    LayerId::Orchestrator,
                    LayerId::Relay,
                    LayerId::Vault,
                    LayerId::Observability,
                    LayerId::LossAttribution,
                ],
            ),
            (LayerId::ExternalData, &[LayerId::Eil, LayerId::Strategy, LayerId::HotPath]),
            (LayerId::Eil, &[LayerId::Strategy, LayerId::HotPath, LayerId::Orchestrator]),
            (LayerId::Risk, &[LayerId::Strategy, LayerId::Orchestrator]),
            (LayerId::ChaosGuard, &[LayerId::Strategy, LayerId::Orchestrator]),
            (LayerId::Dag, &[LayerId::Strategy, LayerId::Orchestrator]),
            (LayerId::Zk, &[LayerId::Vault]),
            (LayerId::Flashloan, &[LayerId::Strategy, LayerId::Orchestrator]),
            (LayerId::Strategy, &[LayerId::Orchestrator, LayerId::Relay]),
            (LayerId::Orchestrator, &[LayerId::Relay, LayerId::Vault]),
            (LayerId::LossAttribution, &[LayerId::Strategy]),
            // Leaf layers — no dependents
            (LayerId::HotPath, &[]),
            (LayerId::Relay, &[]),
            (LayerId::Vault, &[]),
            (LayerId::Observability, &[]),
        ];

        let mut dependents: HashMap<LayerId, Vec<LayerId>> = HashMap::with_capacity(edges.len());

        for (source, deps) in edges {
            dependents.insert(*source, deps.to_vec());
        }

        Self { dependents }
    }

    /// Return the layers that must be halted when `layer` halts.
    ///
    /// Returns an empty slice for leaf layers.
    pub fn halt_dependents(&self, layer: LayerId) -> &[LayerId] {
        self.dependents
            .get(&layer)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

impl Default for PropagationRouter {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SystemHealthOrchestrator
// ─────────────────────────────────────────────────────────────────────────────

/// Async task that drains the propagation channel and drives halt cascade.
///
/// Started once at engine startup by the control-plane.  Runs until the
/// channel is closed (engine shutdown).
///
/// ## Halt cascade logic
///
/// When a `Halted` transition is received for layer L:
///   1. Look up `PropagationRouter::halt_dependents(L)`.
///   2. For each dependent layer D:
///      - If D is not already Halted, call `D.set_state(Halted, cascade_reason)`.
///      - `set_state` will in turn emit another `TransitionEvent` that
///        re-enters this loop - so the cascade is depth-first and
///        guaranteed to reach all transitive dependents.
///
/// ## Recovery
///
/// Recovery (Halted → Healthy) is NOT propagated automatically.
/// Each layer recovers independently under governance clearance (§5).
pub struct SystemHealthOrchestrator {
    receiver: TransitionReceiver,
    router: PropagationRouter,
    /// Mutable view of all layer health controllers, keyed by LayerId.
    /// Used to apply cascaded halt transitions.
    layers: HashMap<LayerId, Arc<dyn omega_core::LayerHealth>>,
}

impl SystemHealthOrchestrator {
    /// Create the orchestrator.
    ///
    /// `layers` must contain a controller for every `LayerId` variant.
    /// `receiver` is the receiving half of the propagation channel.
    pub fn new(
        receiver: TransitionReceiver,
        layers: HashMap<LayerId, Arc<dyn omega_core::LayerHealth>>,
    ) -> Self {
        Self {
            receiver,
            router: PropagationRouter::new(),
            layers,
        }
    }

    /// Run the propagation loop.  Does not return until the channel
    /// is closed (engine shutdown).
    pub async fn run(mut self) {
        tracing::info!("SystemHealthOrchestrator started");

        while let Some(event) = self.receiver.recv().await {
            self.handle_event(event);
        }

        tracing::info!("SystemHealthOrchestrator stopped — channel closed");
    }

    fn handle_event(&self, event: TransitionEvent) {
        tracing::debug!(
            layer  = %event.layer,
            from   = %event.from,
            to     = %event.to,
            reason = %event.reason,
            "Propagation event received",
        );

        if event.to != HealthState::Halted {
            // Only Halted transitions trigger cascade propagation.
            // Degraded and Healthy transitions are informational.
            return;
        }

        let cascade_reason = format!(
            "cascaded halt from layer {} (reason: {})",
            event.layer, event.reason,
        );

        for &dependent in self.router.halt_dependents(event.layer) {
            if let Some(ctrl) = self.layers.get(&dependent) {
                if ctrl.state() != HealthState::Halted {
                    tracing::warn!(
                        source    = %event.layer,
                        dependent = %dependent,
                        "Cascading halt to dependent layer",
                    );
                    ctrl.set_state(HealthState::Halted, &cascade_reason);
                }
            } else {
                tracing::error!(
                    layer = %dependent,
                    "Propagation router references layer with no registered controller",
                );
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_system_health_halts_all() {
        let router = PropagationRouter::new();
        let deps = router.halt_dependents(LayerId::SystemHealth);
        // All other layers must be in the dependency list
        for layer in [
            LayerId::ExternalData,
            LayerId::Eil,
            LayerId::Risk,
            LayerId::Security,
            LayerId::ChaosGuard,
            LayerId::Dag,
            LayerId::Zk,
            LayerId::HotPath,
            LayerId::Strategy,
            LayerId::Flashloan,
            LayerId::Orchestrator,
            LayerId::Relay,
            LayerId::Vault,
            LayerId::Observability,
            LayerId::LossAttribution,
        ] {
            assert!(
                deps.contains(&layer),
                "SystemHealth halt must cascade to {layer:?}",
            );
        }
    }

    #[test]
    fn router_leaf_layers_have_no_dependents() {
        let router = PropagationRouter::new();
        for leaf in [
            LayerId::Relay,
            LayerId::Vault,
            LayerId::HotPath,
            LayerId::Observability,
        ] {
            assert!(
                router.halt_dependents(leaf).is_empty(),
                "{leaf:?} is a leaf — must have no dependents",
            );
        }
    }

    #[test]
    fn router_zk_halts_vault() {
        let router = PropagationRouter::new();
        assert!(
            router
                .halt_dependents(LayerId::Zk)
                .contains(&LayerId::Vault)
        );
    }

    #[test]
    fn router_loss_attribution_halts_strategy() {
        let router = PropagationRouter::new();
        assert!(
            router
                .halt_dependents(LayerId::LossAttribution)
                .contains(&LayerId::Strategy),
        );
    }

    #[test]
    fn channel_send_and_receive() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (tx, mut rx) = channel();
            tx.send_nonblocking(TransitionEvent {
                layer: LayerId::Relay,
                from: HealthState::Healthy,
                to: HealthState::Degraded,
                reason: "test".into(),
                timestamp: Utc::now(),
            });
            let ev = rx.recv().await.expect("event");
            assert_eq!(ev.layer, LayerId::Relay);
        });
    }
}
