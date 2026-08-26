// crates/omega-frontend/src/render.rs — OmegaEngine v12.0 Render Frame
use omega_control_contracts::rest::LayerHealthEntry;

use crate::state::{EngineStore, RealtimeStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderFrame {
    pub revision: u64,
    pub status_label: &'static str,
    pub halted_layers: usize,
    pub degraded_layers: usize,
    pub operational_layers: usize,
}

impl RenderFrame {
    pub fn from_store(store: &EngineStore) -> Self {
        let layers: &[LayerHealthEntry] = store
            .health
            .as_ref()
            .map(|health| health.layers.as_slice())
            .unwrap_or(&[]);

        Self {
            revision: store.revision,
            status_label: status_label(store.realtime_status),
            halted_layers: layers.iter().filter(|layer| layer.state == "HALTED").count(),
            degraded_layers: layers.iter().filter(|layer| layer.state == "DEGRADED").count(),
            operational_layers: layers
                .iter()
                .filter(|layer| layer.is_operational)
                .count(),
        }
    }
}

pub fn stable_layer_key(layer: &LayerHealthEntry) -> &str {
    &layer.layer_id
}

fn status_label(status: RealtimeStatus) -> &'static str {
    match status {
        RealtimeStatus::Disconnected => "disconnected",
        RealtimeStatus::Connecting => "connecting",
        RealtimeStatus::Anonymous => "anonymous",
        RealtimeStatus::Authenticated => "authenticated",
        RealtimeStatus::Lagged => "lagged",
    }
}