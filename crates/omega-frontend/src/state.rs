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
                    // FIX (this revision): E0609 — `LayerHealthEntry` has no
                    // field named `layer`. Confirmed via the compiler's own
                    // note this session: the real fields are `layer_id`,
                    // `state`, `is_operational`, `reason`. Renamed to match
                    // the actual struct, same fix as render.rs's
                    // `stable_layer_key`.
                    if let Some(entry) = health.layers.iter_mut().find(|entry| entry.layer_id == *layer)
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
            // FIX (this revision): E0004 — `WsEvent` gained 6 new
            // trading-engine telemetry variants (GasModelReverted,
            // GasModelCeilingEscalation, EmergencyBundleSkipped,
            // ProfitSplit, LaReorgRisk, BlueprintConfirmed — see
            // omega-control-contracts::ws.rs's own "Trading-engine
            // telemetry (bridged from OmegaEvent via obs_bridge)" comment)
            // that this match never gained arms for, making it
            // non-exhaustive. Added here as explicit no-ops rather than a
            // blanket `_ => {}` — `EngineStore` currently has no field to
            // hold any of this data, and inventing storage/UI behavior for
            // six distinct financial/risk events without a real product
            // spec would be guessing, not fixing a compile error. Each
            // event is still captured via `self.last_event` /
            // `self.last_event_at` / `self.bump()` below regardless (that
            // logic is unconditional, after this match), so no event data
            // is silently lost — it's just not yet reflected in any
            // dedicated `EngineStore` field the way health/dao_fee/
            // blacklist/model_paused/ceiling_hits are. Revisit each of
            // these six with a real field + UI requirement rather than
            // building it out speculatively here.
            WsEvent::GasModelReverted { .. } => {}
            WsEvent::GasModelCeilingEscalation { .. } => {}
            WsEvent::EmergencyBundleSkipped { .. } => {}
            WsEvent::ProfitSplit { .. } => {}
            WsEvent::LaReorgRisk { .. } => {}
            WsEvent::BlueprintConfirmed { .. } => {}
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
        // FIX (this revision): E0004 — same root cause as apply_event's
        // match above (WsEvent gained 6 variants this match never covered)
        // but this was the one the compiler actually reported this
        // session. All 6 new variants carry a `timestamp: DateTime<Utc>`
        // field (confirmed against ws.rs's real struct-variant
        // definitions), so each slots into the existing `{ timestamp, .. }`
        // or-pattern the same way every prior variant already does.
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
            // FIX (this revision): E0609 — `layer` renamed to `layer_id`,
            // matching LayerHealthEntry's real fields (same fix as the two
            // confirmed compiler errors elsewhere in this file/render.rs).
            // FIX (this revision): E0063 — `reason: String` added.
            // Confirmed via omega-control-contracts::rest.rs's real
            // `LayerHealthEntry` definition this session: `reason` is a
            // plain, non-optional `String` with no default, and that same
            // file's own test (`layer_health_entry_serializes_with_
            // layer_id_key`) uses `String::new()` as its "nothing to
            // report" value — matched here for consistency rather than a
            // different placeholder.
            layers: vec![LayerHealthEntry {
                layer_id: "relay".into(),
                state: "HEALTHY".into(),
                is_operational: true,
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