// omega-frontend-arch/crates/omega-runtime/src/registry.rs — lock-free snapshot reads + broadcast for frontend push
//
// Design:
// - lock-free snapshot reads (ArcSwap)
// - monotonic global version
// - snapshot + delta share identical version source
// - no duplicate event emission
// - no redundant snapshot rebuilds
// - deterministic frontend hydration
// - single writer critical section
// - broadcast only on actual state transitions

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
        Mutex,
    },
    time::Duration,
};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::health::{HealthStatus, LayerHealth};

const STALE_AFTER: Duration = Duration::from_secs(10);
const FAILED_AFTER: Duration = Duration::from_secs(30);

/// ======================================================================
/// EVENTS
/// ======================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerEvent {
    pub version: u64,
    pub layer: String,
    pub status: HealthStatus,
    pub latency_ns: u64,
    pub message: String,
}

/// ======================================================================
/// SNAPSHOT DTO
/// ======================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrySnapshot {
    pub version: u64,
    pub layers: HashMap<String, LayerStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerStatus {
    pub status: HealthStatus,
    pub latency_ns: u64,
    pub message: String,
}

/// ======================================================================
/// REGISTRY
/// ======================================================================

pub struct Registry {
    health: Mutex<HashMap<String, LayerHealth>>,
    snapshot: ArcSwap<RegistrySnapshot>,
    version: AtomicU64,
    tx: broadcast::Sender<LayerEvent>,
}

impl Registry {
    pub fn new() -> Arc<Self> {
        let (tx, _) = broadcast::channel(1024);

        Arc::new(Self {
            health: Mutex::new(HashMap::new()),
            snapshot: ArcSwap::from_pointee(RegistrySnapshot {
                version: 0,
                layers: HashMap::new(),
            }),
            version: AtomicU64::new(0),
            tx,
        })
    }

    /// ==========================================================
    /// WRITE PATH
    /// ==========================================================

    pub fn update_layer(
        &self,
        layer: &str,
        status: HealthStatus,
        latency_ns: u64,
        msg: &str,
    ) {
        let layer_name = layer.to_owned();

        let (changed, snapshot_layer) = {
            let mut map = self.health.lock().expect("registry poisoned");

            let entry = map.entry(layer_name.clone()).or_default();

            let old_status = entry.status;
            let old_latency = entry.latency_ns;
            let old_message = entry.message.as_ref();

            entry.record_heartbeat(latency_ns);
            entry.status = status;

            if old_status == status
                && old_latency == latency_ns
                && old_message == msg
            {
                return;
            }

            let dto = LayerStatus {
                status,
                latency_ns,
                message: msg.to_string(),
            };

            (true, dto)
        };

        if !changed {
            return;
        }

        let version = self.next_version();

        self.publish_snapshot_delta(
            version,
            &layer_name,
            snapshot_layer.clone(),
        );

        self.broadcast(LayerEvent {
            version,
            layer: layer_name.clone(),
            status,
            latency_ns,
            message: msg.to_string(),
        });

        debug!(
            layer = %layer_name,
            version,
            status = %status,
            latency_ns,
            "registry updated"
        );
    }

    /// ==========================================================
    /// FSM TICK
    /// ==========================================================

    pub fn tick(&self) {
        let mut transitions = Vec::new();

        {
            let mut map = self.health.lock().expect("registry poisoned");

            for (layer, health) in map.iter_mut() {
                let before = health.status;

                health.tick(
                    STALE_AFTER,
                    FAILED_AFTER,
                );

                if before != health.status {
                    transitions.push((
                        layer.clone(),
                        health.status,
                        health.latency_ns,
                        health.message.to_string(),
                    ));
                }
            }
        }

        if transitions.is_empty() {
            return;
        }

        let version = self.next_version();

        let current = self.snapshot.load();

        let mut layers = current.layers.clone();

        for (layer, status, latency_ns, message) in &transitions {
            layers.insert(
                layer.clone(),
                LayerStatus {
                    status: *status,
                    latency_ns: *latency_ns,
                    message: message.clone(),
                },
            );
        }

        self.snapshot.store(Arc::new(
            RegistrySnapshot {
                version,
                layers,
            }
        ));

        for (layer, status, latency_ns, message) in transitions {
            warn!(
                layer = %layer,
                status = %status,
                version,
                "FSM transition"
            );

            self.broadcast(LayerEvent {
                version,
                layer,
                status,
                latency_ns,
                message,
            });
        }
    }

    /// ==========================================================
    /// READ PATH
    /// ==========================================================

    #[inline(always)]
    pub fn snapshot(&self) -> Arc<RegistrySnapshot> {
        self.snapshot.load_full()
    }

    #[inline(always)]
    pub fn subscribe(&self) -> broadcast::Receiver<LayerEvent> {
        self.tx.subscribe()
    }

    /// ==========================================================
    /// INTERNAL
    /// ==========================================================

    #[inline(always)]
    fn next_version(&self) -> u64 {
        self.version.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn publish_snapshot_delta(
        &self,
        version: u64,
        layer: &str,
        status: LayerStatus,
    ) {
        let current = self.snapshot.load();

        let mut layers = current.layers.clone();

        layers.insert(
            layer.to_string(),
            status,
        );

        self.snapshot.store(Arc::new(
            RegistrySnapshot {
                version,
                layers,
            }
        ));
    }

    #[inline(always)]
    fn broadcast(
        &self,
        event: LayerEvent,
    ) {
        if self.tx.receiver_count() == 0 {
            return;
        }

        let _ = self.tx.send(event);
    }
}