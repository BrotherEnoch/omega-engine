// omega-frontend-arch/crates/omega-runtime/src/loss_attribution.rs — L15 Loss Attribution
// Post-execution analysis. Heartbeat every 5s — batch, non-latency-critical.
// Depends on: L13 Vault, L08 Hot Path being healthy.

use std::{sync::Arc, time::{Duration, Instant}};
use tracing::{info, warn};

use crate::{health::HealthStatus, registry::Registry};

pub const LAYER_ID: &str = "L15";

const DEPS: &[&str] = &["L13", "L08"];

pub fn spawn(registry: Arc<Registry>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!(layer = LAYER_ID, stage = "task_startup", "event emitted");
        registry.update_layer(LAYER_ID, HealthStatus::Starting, 0, "initialising");
        tokio::time::sleep(Duration::from_millis(800)).await;

        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut tick: u64 = 0;

        loop {
            interval.tick().await;
            tick += 1;

            let snap = registry.snapshot();
            let deps_ok = DEPS.iter().all(|id| {
                snap.layers
                    .get(*id)
                    .map(|l| matches!(l.status, HealthStatus::Healthy))
                    .unwrap_or(false)
            });

            if !deps_ok {
                registry.update_layer(
                    LAYER_ID, HealthStatus::Degraded, 0,
                    "waiting for L13 Vault / L08 Hot Path",
                );
                warn!(layer = LAYER_ID, tick, stage = "event_emitted",
                    "[1/6] event emitted → registry (DEGRADED — deps not ready)");
                continue;
            }

            let t0 = Instant::now();
            let ok = do_work().await;
            let latency_ns = t0.elapsed().as_nanos() as u64;

            if ok {
                registry.update_layer(
                    LAYER_ID, HealthStatus::Healthy, latency_ns,
                    &format!("tick={tick}"),
                );
                info!(layer = LAYER_ID, tick, latency_ns,
                    stage = "event_emitted", "[1/6] event emitted → registry");
            } else {
                registry.update_layer(
                    LAYER_ID, HealthStatus::Degraded, latency_ns,
                    "attribution error",
                );
                warn!(layer = LAYER_ID, tick, stage = "event_emitted",
                    "[1/6] event emitted → registry (DEGRADED)");
            }
        }
    })
}

async fn do_work() -> bool { true }