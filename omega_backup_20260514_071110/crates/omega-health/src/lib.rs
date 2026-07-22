ï»¿// crates/omega-health/src/lib.rs
//
// omega-health â€” Health FSM implementation for the Omega Engine.
//
// ## Architectural role (Â§22.1)
//
// omega-health depends on omega-core (for the LayerHealth trait, LayerId,
// HealthState) and implements the concrete mechanisms described in Â§3.
// Every other crate may depend on omega-health to obtain layer health
// controllers.
//
// ## Module map
//
//   halt.rs           â€” HaltFlag: system-wide emergency halt, 10ms poll SLA
//   state_machine.rs  â€” LayerHealthImpl: concrete LayerHealth + FSM validation
//   persistence.rs    â€” HealthLog: append-only NDJSON transition audit log
//   propagation.rs    â€” TransitionSender/Receiver, PropagationRouter,
//                       SystemHealthOrchestrator (halt cascade Â§3, Â§22.1)
//   reorg_handler.rs  â€” ReorgGuard: LA blueprint reorg protection (Â§11.4)
//   monitors.rs       â€” OracleLivenessMonitor, GasSpikeMonitor,
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

// â”€â”€ Convenience re-exports â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub use halt::{HaltFlag, HaltRecord};

pub use state_machine::{LayerHealthImpl, TransitionError};

pub use persistence::{HealthLog, HealthLogEntry};

pub use propagation::{
    channel as health_channel,
    PropagationRouter,
    SystemHealthOrchestrator,
    TransitionEvent,
    TransitionReceiver,
    TransitionSender,
};

pub use reorg_handler::{ReorgGuard, ReorgRiskEvent, STABILITY_WINDOW_BLOCKS};

pub use monitors::{
    GasSpikeMonitor,
    HaltPollLoop,
    OracleFeedHandle,
    OracleLivenessMonitor,
};