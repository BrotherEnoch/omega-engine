// crates/omega-frontend/src/render.rs
use omega_control_contracts::health::{HealthStatus, LayerId};
use strum::IntoEnumIterator;
use crate::state::EngineState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Ok,
    Warning,
    Critical,
    Unknown,
}

impl From<HealthStatus> for Severity {
    fn from(s: HealthStatus) -> Self {
        match s {
            HealthStatus::Ok        => Severity::Ok,
            HealthStatus::Recovering => Severity::Warning,
            HealthStatus::Degraded  => Severity::Warning,
            HealthStatus::Halted    => Severity::Critical,
            HealthStatus::Unknown   => Severity::Unknown,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LayerRow {
    pub layer:    LayerId,
    pub label:    &'static str,
    pub status:   HealthStatus,
    pub severity: Severity,
    pub message:  Option<String>,
}

#[derive(Debug, Clone)]
pub struct GasModelPanel {
    pub checkpoint_count:    usize,
    pub active_version:      Option<u64>,
    pub active_win_rate:     Option<f64>,
    pub paused:              bool,
    pub features_at_ceiling: usize,
}

#[derive(Debug, Clone)]
pub struct VaultPanel {
    pub dao_fee_bps:            Option<u16>,
    pub dao_fee_address_short:  Option<String>,
    pub address_change_pending: bool,
}

#[derive(Debug, Clone)]
pub struct BlacklistPanel {
    pub entry_count:   usize,
    pub revision_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AlertBanner {
    pub severity: Severity,
    pub message:  String,
}

#[derive(Debug, Clone)]
pub struct RenderFrame {
    pub revision:         u64,
    pub overall_severity: Severity,
    pub ws_connected:     bool,
    pub ws_status_label:  &'static str,
    pub layers:           Vec<LayerRow>,
    pub gas_model:        GasModelPanel,
    pub vault:            VaultPanel,
    pub blacklist:        BlacklistPanel,
    pub alerts:           Vec<AlertBanner>,
}

#[must_use]
pub fn derive_frame(state: &EngineState) -> RenderFrame {
    use omega_control_contracts::ws::WsConnectionStatus;

    let layers: Vec<LayerRow> = LayerId::iter()
        .map(|layer| {
            let status  = state.layer_status(layer);
            let message = state.layer_health(layer)
                .and_then(|lh| lh.message.as_deref())
                .map(|m| m.chars().take(80).collect());
            LayerRow {
                label:    layer_label(layer),
                layer,
                severity: Severity::from(status),
                status,
                message,
            }
        })
        .collect();

    let active_win_rate = state.active_checkpoint_version().and_then(|v| {
        state.checkpoints().iter().find(|c| c.version == v).map(|c| c.win_rate)
    });
    let features_at_ceiling = state.ceiling_status()
        .map(|cs| cs.features.iter().filter(|f| f.ceiling_hit_count > 0).count())
        .unwrap_or(0);
    let gas_model = GasModelPanel {
        checkpoint_count:    state.checkpoints().len(),
        active_version:      state.active_checkpoint_version(),
        active_win_rate,
        paused:              state.gas_model_paused(),
        features_at_ceiling,
    };

    let vault = VaultPanel {
        dao_fee_bps:           state.dao_fee().map(|d| d.dao_fee_bps),
        dao_fee_address_short: None,
        address_change_pending: false,
    };

    let blacklist = BlacklistPanel {
        entry_count:   state.blacklist().map(|b| b.entry_count).unwrap_or(0),
        revision_hash: None,
    };

    let (ws_connected, ws_status_label) = match state.ws_status() {
        WsConnectionStatus::Connected           => (true,  "Live"),
        WsConnectionStatus::Connecting          => (false, "Connecting…"),
        WsConnectionStatus::Reconnecting { .. } => (false, "Reconnecting…"),
        WsConnectionStatus::Unavailable         => (false, "WS unavailable — polling"),
        WsConnectionStatus::Disconnected        => (false, "Disconnected"),
        WsConnectionStatus::AuthError           => (false, "Auth error"),
    };

    let mut alerts = Vec::new();
    if state.any_halted() {
        alerts.push(AlertBanner {
            severity: Severity::Critical,
            message:  "⚠ One or more engine layers are HALTED".into(),
        });
    }
    if state.gas_model_paused() {
        alerts.push(AlertBanner {
            severity: Severity::Warning,
            message:  "Gas model paused — ceiling escalation active. L2 governance review required.".into(),
        });
    }
    if vault.address_change_pending {
        alerts.push(AlertBanner {
            severity: Severity::Warning,
            message:  "DAO fee address change pending (48h timelock)".into(),
        });
    }

    RenderFrame {
        revision: state.revision(),
        overall_severity: Severity::from(state.overall_health()),
        ws_connected,
        ws_status_label,
        layers,
        gas_model,
        vault,
        blacklist,
        alerts,
    }
}

fn layer_label(layer: LayerId) -> &'static str {
    match layer {
        LayerId::SystemHealth    => "Health FSM",
        LayerId::ExternalData    => "External Data",
        LayerId::Eil             => "EIL",
        LayerId::Risk            => "Risk Engine",
        LayerId::Security        => "Security",
        LayerId::ChaosGuard      => "Chaos Guard",
        LayerId::Dag             => "DAG Planner",
        LayerId::Zk              => "ZK Prover",
        LayerId::HotPath         => "Hot Path",
        LayerId::Strategy        => "Strategy",
        LayerId::Flashloan       => "Flash Loan",
        LayerId::Orchestrator    => "Orchestrator",
        LayerId::Relay           => "Relay",
        LayerId::Vault           => "Vault",
        LayerId::Observability   => "Observability",
        LayerId::LossAttribution => "Loss Attribution",
    }
}

#[allow(dead_code)]
fn shorten_address(addr: &str) -> String {
    if addr.len() > 12 {
        format!("{}…{}", &addr[..6], &addr[addr.len() - 4..])
    } else {
        addr.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_produces_16_layer_rows() {
        let state = EngineState::default();
        let frame = derive_frame(&state);
        assert_eq!(frame.layers.len(), 16);
    }

    #[test]
    fn frame_is_deterministic() {
        let state = EngineState::default();
        let f1 = derive_frame(&state);
        let f2 = derive_frame(&state);
        assert_eq!(f1.revision, f2.revision);
        assert_eq!(f1.layers.len(), f2.layers.len());
    }

    #[test]
    fn no_alerts_on_clean_state() {
        let state = EngineState::default();
        assert!(derive_frame(&state).alerts.is_empty());
    }
}
