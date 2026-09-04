// omega-frontend-arch/crates/omega-runtime/src/orchestrator.rs

use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::info;

use crate::health::HealthStatus;
use crate::registry::Registry;

pub const LAYER_ID: &str = "L11";

/// Spawn the L11 orchestrator task.
pub fn spawn(registry: Arc<Registry>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!(layer = LAYER_ID, stage = "task_startup", "orchestrator starting");
        registry.update_layer(LAYER_ID, HealthStatus::Starting, 0, "initialising");

        tokio::time::sleep(Duration::from_millis(300)).await;
        registry.update_layer(LAYER_ID, HealthStatus::Healthy, 0, "ready");

        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut tick: u64 = 0;
        let mut consecutive_errors: u32 = 0;

        loop {
            interval.tick().await;
            tick += 1;
            let t0 = Instant::now();

            let summary = {
                let snap = registry.snapshot();
                consecutive_errors = 0;
                let unhealthy: Vec<String> = snap
                    .layers
                    .iter()
                    .filter(|(_, st)| !st.status.is_healthy() && st.status != HealthStatus::Starting)
                    .map(|(id, st)| format!("{id}:{:?}", st.status))
                    .collect();
                if unhealthy.is_empty() {
                    format!("tick={tick} all-layers-ok")
                } else {
                    format!("tick={tick} degraded=[{}]", unhealthy.join(","))
                }
            };

            let latency_ns = t0.elapsed().as_nanos() as u64;

            let status = if consecutive_errors >= 5 {
                HealthStatus::Degraded
            } else {
                HealthStatus::Healthy
            };

            registry.update_layer(LAYER_ID, status, latency_ns, &summary);

            info!(
                layer = LAYER_ID,
                tick,
                latency_ns,
                stage = "tick",
                %summary,
                "orchestrator tick"
            );
        }
    })
}