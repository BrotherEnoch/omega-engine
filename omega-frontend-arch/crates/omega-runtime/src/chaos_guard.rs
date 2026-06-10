// omega-frontend-arch/crates/omega-runtime/src/chaos_guard.rs — L05 subsystem heartbeat

use std::{sync::Arc, time::{Duration, Instant}};
use tracing::{info, warn};

use crate::{health::HealthStatus, registry::Registry};

pub const LAYER_ID: &str = "L05";

pub fn spawn(registry: Arc<Registry>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!(layer = LAYER_ID, stage = "task_startup", "event emitted");
        registry.update_layer(LAYER_ID, HealthStatus::Starting, 0, "initialising");
        tokio::time::sleep(Duration::from_millis(550)).await;

        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut tick: u64 = 0;

        loop {
            interval.tick().await;
            tick += 1;

            let t0  = Instant::now();
            let ok  = do_work().await;
            let latency_ns = t0.elapsed().as_nanos() as u64;

            if ok {
                registry.update_layer(
                    LAYER_ID,
                    HealthStatus::Healthy,
                    latency_ns,
                    &format!("tick={tick}"),
                );
                info!(
                    layer = LAYER_ID, tick, latency_ns,
                    stage = "event_emitted",
                    "[1/6] event emitted → registry"
                );
            } else {
                registry.update_layer(
                    LAYER_ID,
                    HealthStatus::Degraded,
                    latency_ns,
                    "work loop returned error",
                );
                warn!(
                    layer = LAYER_ID, tick,
                    stage = "event_emitted",
                    "[1/6] event emitted → registry (DEGRADED)"
                );
            }
        }
    })
}

/// Replace with real subsystem logic.
async fn do_work() -> bool {
    true
}