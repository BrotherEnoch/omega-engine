// crates/omega-health/src/lib.rs
//
// omega-health — Health FSM implementation for the Omega Engine.
//
// ## Architectural role (§22.1)
//
// omega-health depends on omega-core (for the LayerHealth trait, LayerId,
// HealthState) and implements the concrete mechanisms described in §3.
// Every other crate may depend on omega-health to obtain layer health
// controllers.
//
// ## Module map
//
//   halt.rs           — HaltFlag: system-wide emergency halt, 10ms poll SLA
//   state_machine.rs  — LayerHealthImpl: concrete LayerHealth + FSM validation
//   persistence.rs    — HealthLog: append-only NDJSON transition audit log
//   propagation.rs    — TransitionSender/Receiver, PropagationRouter,
//                       SystemHealthOrchestrator (halt cascade §3, §22.1)
//   reorg_handler.rs  — ReorgGuard: LA blueprint reorg protection (§11.4)
//   monitors.rs       — OracleLivenessMonitor, GasSpikeMonitor,
//                       HaltPollLoop
//
// ## Public surface
//
// Downstream crates should import from the crate root rather than from
// individual modules:
//
//   use omega_health::{HaltFlag, LayerHealthImpl, SystemHealthOrchestrator};

pub mod halt;
pub mod monitors;
pub mod persistence;
pub mod propagation;
pub mod reorg_handler;
pub mod state_machine;

// ── Convenience re-exports ────────────────────────────────────────────────────

pub use halt::{HaltFlag, HaltRecord};

pub use state_machine::{LayerHealthImpl, TransitionError};

pub use persistence::{HealthLog, HealthLogEntry};

pub use propagation::{
    channel as health_channel, PropagationRouter, SystemHealthOrchestrator, TransitionEvent,
    TransitionReceiver, TransitionSender,
};

pub use reorg_handler::{ReorgGuard, ReorgRiskEvent, STABILITY_WINDOW_BLOCKS};

pub use monitors::{GasSpikeMonitor, HaltPollLoop, OracleFeedHandle, OracleLivenessMonitor};
