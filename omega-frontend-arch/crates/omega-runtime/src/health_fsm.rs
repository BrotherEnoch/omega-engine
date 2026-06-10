// omega-frontend-arch/crates/omega-runtime/src/health_fsm.rs — L00 Health FSM
// Aggregates all 15 subsystem layers into a single system health signal.
// HEALTHY only when all watched layers are HEALTHY.

use std::{sync::Arc, time::Duration};
use tracing::info;

use crate::{health::HealthStatus, registry::Registry};

pub const LAYER_ID: &str = "L00";

/// All layers L00 watches. Updated to include execution tier.
const WATCHED: &[&str] = &[
    "L01", "L02", "L03", "L04", "L05",   // infrastructure data/security
    "L06", "L07", "L08", "L09", "L10",   // execution
    "L11", "L12",                          // orchestration/relay
    "L13", "L14", "L15",                  // vault/observability/attribution
];

pub fn spawn(registry: Arc<Registry>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!(layer = LAYER_ID, stage = "task_startup", "event emitted");
        registry.update_layer(LAYER_ID, HealthStatus::Starting, 0, "initialising");

        let mut interval = tokio::time::interval(Duration::from_secs(2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;

            let snap = registry.snapshot();
            let mut any_failed   = false;
            let mut any_degraded = false;
            let mut any_unknown  = false;
            let mut healthy_count = 0usize;

            for id in WATCHED {
                match snap.layers.get(*id).map(|l| l.status) {
                    Some(HealthStatus::Failed)  |
                    Some(HealthStatus::Stopped) => any_failed   = true,
                    Some(HealthStatus::Degraded)|
                    Some(HealthStatus::Stale)   => any_degraded = true,
                    Some(HealthStatus::Unknown) |
                    Some(HealthStatus::Starting)|
                    None                        => any_unknown  = true,
                    Some(HealthStatus::Healthy) => healthy_count += 1,
                }
            }

            let total = WATCHED.len();
            let (status, msg) = if any_failed {
                (HealthStatus::Failed,
                 format!("{healthy_count}/{total} healthy — one or more FAILED"))
            } else if any_degraded {
                (HealthStatus::Degraded,
                 format!("{healthy_count}/{total} healthy — one or more DEGRADED"))
            } else if any_unknown {
                (HealthStatus::Starting,
                 format!("{healthy_count}/{total} healthy — waiting for all layers"))
            } else {
                (HealthStatus::Healthy,
                 format!("all {total} layers HEALTHY"))
            };

            registry.update_layer(LAYER_ID, status, 0, &msg);
            info!(
                layer = LAYER_ID,
                %status,
                healthy_count,
                total,
                stage = "event_emitted",
                "[1/6] event emitted → registry"
            );
        }
    })
}