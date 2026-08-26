// crates/omega-frontend/src/state.rs
use chrono::{DateTime, Utc};
use omega_control_contracts::rest::{BlacklistResponse, DaoFeeResponse, HealthSnapshot};
use omega_control_contracts::ws::WsEvent;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RealtimeStatus {
    Disconnected,
    Connecting,
    Anonymous,
    Authenticated,
    Lagged,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineStore {
    pub revision: u64,
    pub realtime_status: RealtimeStatus,
    pub health: Option<HealthSnapshot>,
    pub dao_fee: Option<DaoFeeResponse>,
    pub builder_blacklist: Option<BlacklistResponse>,
    pub model_paused: Option<bool>,
    pub consecutive_ceiling_hits: Option<u64>,
    pub last_event_at: Option<DateTime<Utc>>,
    pub last_event: Option<WsEvent>,
}

impl Default for EngineStore {
    fn default() -> Self {
        Self {
            revision: 0,
            realtime_status: RealtimeStatus::Disconnected,
            health: None,
            dao_fee: None,
            builder_blacklist: None,
            model_paused: None,
            consecutive_ceiling_hits: None,
            last_event_at: None,
            last_event: None,
        }
    }
}

impl EngineStore {
    pub fn ingest_health(&mut self, health: HealthSnapshot) {
        self.health = Some(health);
        self.bump();
    }

    pub fn ingest_dao_fee(&mut self, dao_fee: DaoFeeResponse) {
        self.dao_fee = Some(dao_fee);
        self.bump();
    }

    pub fn ingest_builder_blacklist(&mut self, blacklist: BlacklistResponse) {
        self.builder_blacklist = Some(blacklist);
        self.bump();
    }

    pub fn set_realtime_status(&mut self, status: RealtimeStatus) {
        if self.realtime_status != status {
            self.realtime_status = status;
            self.bump();
        }
    }

    pub fn apply_event(&mut self, event: WsEvent) {
        self.last_event_at = Some(event.timestamp());

        match &event {
            WsEvent::HealthTransition { layer, to, .. } => {
                if let Some(health) = &mut self.health {
                    if let Some(entry) =
                        health.layers.iter_mut().find(|entry| entry.layer_id == *layer)
                    {
                        entry.state = to.clone();
                        entry.is_operational = to != "HALTED";
                    }
                    health.system_halted = health.layers.iter().any(|entry| entry.state == "HALTED");
                }
            }
            WsEvent::ModelPauseChanged { paused, .. } => {
                self.model_paused = Some(*paused);
            }
            WsEvent::BlacklistReloaded { entry_count, .. } => {
                if let Some(blacklist) = &mut self.builder_blacklist {
                    blacklist.entry_count = *entry_count;
                    blacklist.is_empty = *entry_count == 0;
                }
            }
            WsEvent::ConfigReloaded { .. } => {}
            WsEvent::CeilingEscalation {
                consecutive_hits,
                paused,
                ..
            } => {
                self.consecutive_ceiling_hits = Some(*consecutive_hits);
                self.model_paused = Some(*paused);
            }
            // Trading-engine telemetry (bridged from OmegaEvent via obs_bridge).
            // EngineStore has no dedicated fields for these yet — they're
            // captured via last_event/last_event_at below but not otherwise
            // projected into store state. Add fields here if/when the UI
            // needs to surface gas-model reverts, emergency-skip reasons,
            // profit splits, reorg risk, or confirmed-blueprint profit.
            WsEvent::GasModelReverted { .. }
            | WsEvent::GasModelCeilingEscalation { .. }
            | WsEvent::EmergencyBundleSkipped { .. }
            | WsEvent::ProfitSplit { .. }
            | WsEvent::LaReorgRisk { .. }
            | WsEvent::BlueprintConfirmed { .. } => {}
        }

        self.last_event = Some(event);
        self.bump();
    }

    fn bump(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
}

trait EventTimestamp {
    fn timestamp(&self) -> DateTime<Utc>;
}

impl EventTimestamp for WsEvent {
    fn timestamp(&self) -> DateTime<Utc> {
        match self {
            WsEvent::HealthTransition { timestamp, .. }
            | WsEvent::ModelPauseChanged { timestamp, .. }
            | WsEvent::BlacklistReloaded { timestamp, .. }
            | WsEvent::ConfigReloaded { timestamp }
            | WsEvent::CeilingEscalation { timestamp, .. }
            | WsEvent::GasModelReverted { timestamp, .. }
            | WsEvent::GasModelCeilingEscalation { timestamp, .. }
            | WsEvent::EmergencyBundleSkipped { timestamp, .. }
            | WsEvent::ProfitSplit { timestamp, .. }
            | WsEvent::LaReorgRisk { timestamp, .. }
            | WsEvent::BlueprintConfirmed { timestamp, .. } => *timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omega_control_contracts::rest::LayerHealthEntry;

    #[test]
    fn health_transition_updates_existing_snapshot() {
        let mut store = EngineStore::default();
        store.ingest_health(HealthSnapshot {
            generated_at: Utc::now(),
            layers: vec![LayerHealthEntry {
                layer_id: "relay".into(),
                state: "HEALTHY".into(),
                is_operational: true,
                // NOTE: `reason` is assumed to be `String` here (empty on a
                // healthy entry). If `LayerHealthEntry::reason` is actually
                // `Option<String>` in omega-control-contracts, change this to
                // `None`. I don't have that struct's definition, only the
                // compiler's field list, so this line may need a one-word fix.
                reason: String::new(),
            }],
            system_halted: false,
        });

        store.apply_event(WsEvent::HealthTransition {
            layer: "relay".into(),
            from: "HEALTHY".into(),
            to: "HALTED".into(),
            reason: "test".into(),
            timestamp: Utc::now(),
        });

        let health = store.health.unwrap();
        assert!(health.system_halted);
        assert_eq!(health.layers[0].state, "HALTED");
        assert!(!health.layers[0].is_operational);
    }
}