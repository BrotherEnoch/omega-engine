// omega-frontend-arch/crates/omega-frontend/src/state.rs

use chrono::{DateTime, Utc};
use omega_control_contracts::{
    health::{HealthStatus, LayerId},
    rest::{
        BlacklistResponse, CeilingStatusResponse, DaoFeeResponse,
        GasModelCheckpoint, HealthSnapshot, LayerHealthEntry,
    },
    ws::{WsConnectionStatus, WsEvent},
};
use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;

use crate::observability::ObservabilityLog;

// ── EngineStore ───────────────────────────────────────────────────────────────

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
    pub revision:                 u64,
    pub realtime_status:          RealtimeStatus,
    pub health:                   Option<HealthSnapshot>,
    pub dao_fee:                  Option<DaoFeeResponse>,
    pub builder_blacklist:        Option<BlacklistResponse>,
    pub model_paused:             Option<bool>,
    pub consecutive_ceiling_hits: Option<u64>,
    pub last_event_at:            Option<DateTime<Utc>>,
    pub last_event:               Option<WsEvent>,
}

impl Default for EngineStore {
    fn default() -> Self {
        Self {
            revision:                 0,
            realtime_status:          RealtimeStatus::Disconnected,
            health:                   None,
            dao_fee:                  None,
            builder_blacklist:        None,
            model_paused:             None,
            consecutive_ceiling_hits: None,
            last_event_at:            None,
            last_event:               None,
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
        self.last_event_at = Some(Utc::now());

        match &event {
            WsEvent::LayerEvent(ev) => {
                if let Some(layer) = ev.layer_id() {
                    let key = layer_backend_key(layer);
                    if let Some(health) = &mut self.health {
                        if let Some(entry) =
                            health.layers.iter_mut().find(|e| e.layer == key)
                        {
                            entry.state          = ev.status.clone();
                            entry.is_operational = ev.health_status() != HealthStatus::Halted;
                            entry.message        = Some(ev.message.clone());
                        }
                        health.system_halted =
                            health.layers.iter().any(|e| e.state == "HALTED");
                    }
                }
            }
            WsEvent::GasModelCeilingEscalation(ev) => {
                let prev = self.consecutive_ceiling_hits.unwrap_or(0);
                self.consecutive_ceiling_hits = Some(prev.saturating_add(ev.ceiling_hit_count));
            }
            WsEvent::GasModelReverted(_)
            | WsEvent::EmergencyBundleSkipped(_)
            | WsEvent::ProfitSplit(_)
            | WsEvent::LaReorgRisk(_)
            | WsEvent::SimulationError(_)
            | WsEvent::BlueprintConfirmed(_)
            | WsEvent::Ping(_) => {}
        }

        self.last_event = Some(event);
        self.bump();
    }

    fn bump(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
}

// ── EngineState ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct EngineState {
    revision:       u64,
    ws_status:      WsConnectionStatus,
    health:         Option<HealthSnapshot>,
    checkpoints:    Vec<GasModelCheckpoint>,
    dao_fee:        Option<DaoFeeResponse>,
    blacklist:      Option<BlacklistResponse>,
    ceiling_status: Option<CeilingStatusResponse>,
    /// Live observability event ring buffer.
    pub obs_log:    ObservabilityLog,
}

impl Default for EngineState {
    fn default() -> Self {
        Self {
            revision:       0,
            ws_status:      WsConnectionStatus::Disconnected,
            health:         None,
            checkpoints:    Vec::new(),
            dao_fee:        None,
            blacklist:      None,
            ceiling_status: None,
            obs_log:        ObservabilityLog::with_capacity(200),
        }
    }
}

// ── Public read interface ─────────────────────────────────────────────────────

impl EngineState {
    pub fn revision(&self) -> u64 { self.revision }

    pub fn ws_status(&self) -> WsConnectionStatus { self.ws_status.clone() }

    pub fn layer_status(&self, layer: LayerId) -> HealthStatus {
        let key = layer_backend_key(layer);
        self.health
            .as_ref()
            .and_then(|h| h.layers.iter().find(|e| e.layer == key))
            .map(|e| parse_health_status(&e.state))
            .unwrap_or(HealthStatus::Unknown)
    }

    pub fn layer_health(&self, layer: LayerId) -> Option<&LayerHealthEntry> {
        let key = layer_backend_key(layer);
        self.health
            .as_ref()
            .and_then(|h| h.layers.iter().find(|e| e.layer == key))
    }

    pub fn overall_health(&self) -> HealthStatus {
        LayerId::iter()
            .map(|l| self.layer_status(l))
            .fold(HealthStatus::Unknown, worst_status)
    }

    pub fn any_halted(&self) -> bool {
        self.health.as_ref().map(|h| h.system_halted).unwrap_or(false)
    }

    pub fn gas_model_paused(&self) -> bool {
        self.ceiling_status
            .as_ref()
            .map(|cs| cs.paused)
            .unwrap_or(false)
    }

    pub fn checkpoints(&self) -> &[GasModelCheckpoint] { &self.checkpoints }

    pub fn active_checkpoint_version(&self) -> Option<u64> {
        self.checkpoints.iter().map(|c| c.version).max()
    }

    pub fn ceiling_status(&self) -> Option<&CeilingStatusResponse> {
        self.ceiling_status.as_ref()
    }

    pub fn dao_fee(&self) -> Option<&DaoFeeResponse> { self.dao_fee.as_ref() }

    pub fn blacklist(&self) -> Option<&BlacklistResponse> { self.blacklist.as_ref() }
}

// ── Mutation interface ────────────────────────────────────────────────────────

impl EngineState {
    /// Unconditionally replace the health snapshot, bypassing the
    /// generated_at staleness check. Used exclusively by ws_client.
    pub fn force_health_snapshot(&mut self, snap: HealthSnapshot) {
        self.health   = Some(snap);
        self.revision = self.revision.saturating_add(1);
    }

    /// Record an observability event into the ring buffer.
    pub fn record_obs_event(&mut self, event: &WsEvent) {
        self.obs_log.record_ws_event(event);
        self.revision = self.revision.saturating_add(1);
    }

    pub fn accept_health_snapshot(&self, snap: &HealthSnapshot) -> Option<Self> {
        if let Some(current) = &self.health {
            if snap.generated_at <= current.generated_at {
                return None;
            }
        }
        let mut next  = self.clone();
        next.health   = Some(snap.clone());
        next.revision = next.revision.saturating_add(1);
        Some(next)
    }

    pub fn with_ws_status(&self, status: WsConnectionStatus) -> Self {
        let mut next   = self.clone();
        next.ws_status = status;
        next.revision  = next.revision.saturating_add(1);
        next
    }

    pub fn with_checkpoints(&self, checkpoints: Vec<GasModelCheckpoint>) -> Self {
        let mut next     = self.clone();
        next.checkpoints = checkpoints;
        next.revision    = next.revision.saturating_add(1);
        next
    }

    pub fn with_dao_fee(&self, dao_fee: DaoFeeResponse) -> Self {
        let mut next  = self.clone();
        next.dao_fee  = Some(dao_fee);
        next.revision = next.revision.saturating_add(1);
        next
    }

    pub fn with_blacklist(&self, blacklist: BlacklistResponse) -> Self {
        let mut next   = self.clone();
        next.blacklist = Some(blacklist);
        next.revision  = next.revision.saturating_add(1);
        next
    }

    pub fn with_ceiling_status(&self, ceiling_status: CeilingStatusResponse) -> Self {
        let mut next        = self.clone();
        next.ceiling_status = Some(ceiling_status);
        next.revision       = next.revision.saturating_add(1);
        next
    }

    pub fn apply_ws_event(&self, _seq: u64, event: &WsEvent) -> Option<Self> {
        match event {
            WsEvent::LayerEvent(ev) => {
                let layer = ev.layer_id()?;
                let key   = layer_backend_key(layer);
                let mut next = self.clone();
                if let Some(health) = &mut next.health {
                    if let Some(entry) =
                        health.layers.iter_mut().find(|e| e.layer == key)
                    {
                        entry.state          = ev.status.clone();
                        entry.is_operational = ev.health_status() != HealthStatus::Halted;
                        entry.message        = Some(ev.message.clone());
                        health.system_halted =
                            health.layers.iter().any(|e| e.state == "HALTED");
                        next.revision = next.revision.saturating_add(1);
                        return Some(next);
                    }
                }
                None
            }
            WsEvent::GasModelReverted(_)
            | WsEvent::GasModelCeilingEscalation(_)
            | WsEvent::EmergencyBundleSkipped(_)
            | WsEvent::ProfitSplit(_)
            | WsEvent::LaReorgRisk(_)
            | WsEvent::SimulationError(_)
            | WsEvent::BlueprintConfirmed(_)
            | WsEvent::Ping(_) => None,
        }
    }
}

// ── LayerId → backend key ─────────────────────────────────────────────────────

/// Maps a `LayerId` to the exact string the backend uses to identify that
/// layer in JSON ("layer" field of `LayerHealthEntry`/`HealthSnapshot`,
/// and the "layer" field of WS `LayerEvent` payloads).
///
/// These RHS strings MUST match `LayerId`'s canonical v12 `Display` output
/// exactly (see `crates/omega-core/src/types/health.rs` —
/// `#[strum(serialize_all = "SCREAMING_SNAKE_CASE")]` on the real variant
/// names), because that Display impl is literally what
/// `ops/control-plane`'s `get_health` handler calls
/// (`l.layer_id().to_string()`) to build the `layer` field every
/// `HealthSnapshot` carries. Using a pre-v12 alias name on the RHS here
/// (e.g. "SYSTEM_HEALTH" instead of "HEALTH") silently breaks every
/// lookup in `layer_status()`/`layer_health()` — the search for a
/// matching `entry.layer == key` never finds a match, and every layer
/// falls back to `HealthStatus::Unknown` regardless of what the backend
/// actually reports.
///
/// LHS match patterns may freely use either canonical names or pre-v12
/// aliases (`LayerId::SystemHealth`, `LayerId::ExternalData`, `LayerId::Eil`,
/// `LayerId::Strategy`, `LayerId::Flashloan`, `LayerId::Orchestrator`,
/// `LayerId::Vault` are all const aliases that resolve to a canonical
/// variant at compile time — see `omega_core::LayerId`'s
/// `#[allow(non_upper_case_globals)] impl LayerId` block). `ChaosGuard`
/// (alias for `Security`) is intentionally NOT listed as a separate arm
/// here: since it resolves to the exact same variant as the `Security`
/// arm above it, an additional `LayerId::ChaosGuard` arm would be
/// unreachable and was previously a duplicate-vs-missing-Oracle bug.
fn layer_backend_key(layer: LayerId) -> &'static str {
    match layer {
        LayerId::SystemHealth    => "HEALTH",
        LayerId::ExternalData    => "RPC",
        LayerId::Oracle          => "ORACLE",
        LayerId::Eil             => "COMPLIANCE",
        LayerId::Risk            => "RISK",
        LayerId::Security        => "SECURITY",
        LayerId::Dag             => "DAG",
        LayerId::Zk              => "ZK",
        LayerId::HotPath         => "HOT_PATH",
        LayerId::Strategy        => "STRATEGIES",
        LayerId::Flashloan       => "FLASH_LOAN",
        LayerId::Orchestrator    => "GAS_WAR",
        LayerId::Relay           => "RELAY",
        LayerId::Vault           => "ADDRESS_ROTATION",
        LayerId::Observability   => "OBSERVABILITY",
        LayerId::LossAttribution => "LOSS_ATTRIBUTION",
    }
}

fn parse_health_status(s: &str) -> HealthStatus {
    match s {
        "HEALTHY" | "OK" => HealthStatus::Ok,
        "DEGRADED"       => HealthStatus::Degraded,
        "HALTED"         => HealthStatus::Halted,
        "RECOVERING"     => HealthStatus::Recovering,
        _                => HealthStatus::Unknown,
    }
}

fn worst_status(a: HealthStatus, b: HealthStatus) -> HealthStatus {
    use HealthStatus::*;
    match (a, b) {
        (Halted, _) | (_, Halted)         => Halted,
        (Degraded, _) | (_, Degraded)     => Degraded,
        (Recovering, _) | (_, Recovering) => Recovering,
        (Ok, _) | (_, Ok)                 => Ok,
        _                                 => Unknown,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use omega_control_contracts::ws::LayerEventPayload;

    fn snap_with(layer_key: &str, status: &str) -> HealthSnapshot {
        HealthSnapshot {
            generated_at:  chrono::Utc::now(),
            layers:        vec![LayerHealthEntry {
                layer:          layer_key.into(),
                state:          status.into(),
                is_operational: status != "HALTED",
                message:        None,
            }],
            system_halted: status == "HALTED",
        }
    }

    #[test]
    fn default_state_all_layers_unknown() {
        let state = EngineState::default();
        for layer in LayerId::iter() {
            assert_eq!(state.layer_status(layer), HealthStatus::Unknown);
        }
    }

    #[test]
    fn accept_snapshot_sets_layer_status() {
        let state = EngineState::default();
        let snap  = snap_with("RELAY", "HEALTHY");
        let next  = state.accept_health_snapshot(&snap).unwrap();
        assert_eq!(next.layer_status(LayerId::Relay), HealthStatus::Ok);
    }

    #[test]
    fn stale_snapshot_returns_none() {
        let state = EngineState::default();
        let snap1 = snap_with("RELAY", "HEALTHY");
        let next1 = state.accept_health_snapshot(&snap1).unwrap();
        assert!(next1.accept_health_snapshot(&snap1).is_none());
    }

    #[test]
    fn ws_status_transition() {
        let state = EngineState::default();
        let next  = state.with_ws_status(WsConnectionStatus::Connected);
        assert_eq!(next.ws_status(), WsConnectionStatus::Connected);
        assert_eq!(next.revision(), 1);
    }

    #[test]
    fn apply_ws_event_layer_event_halts() {
        let state = EngineState::default();
        let snap  = snap_with("RELAY", "HEALTHY");
        let state = state.accept_health_snapshot(&snap).unwrap();
        let event = WsEvent::LayerEvent(LayerEventPayload {
            layer: "L12".into(), status: "HALTED".into(),
            message: "circuit breaker tripped".into(), version: 1, latency_ns: 0,
        });
        let next = state.apply_ws_event(1, &event).unwrap();
        assert_eq!(next.layer_status(LayerId::Relay), HealthStatus::Halted);
        assert!(next.any_halted());
    }

    #[test]
    fn force_health_snapshot_bypasses_timestamp() {
        let mut state = EngineState::default();
        let snap = snap_with("RELAY", "HEALTHY");
        state.force_health_snapshot(snap);
        assert_eq!(state.layer_status(LayerId::Relay), HealthStatus::Ok);
    }

    #[test]
    fn record_obs_event_increments_revision() {
        use omega_control_contracts::ws::GasModelRevertedEvent;
        let mut state = EngineState::default();
        let rev0 = state.revision();
        state.record_obs_event(&WsEvent::GasModelReverted(GasModelRevertedEvent {
            checkpoint_version: 1, win_rate: 0.7, sample_count: 1000,
        }));
        assert!(state.revision() > rev0);
        assert_eq!(state.obs_log.metrics.gas_model_reverts, 1);
    }

    #[test]
    fn blueprint_confirmed_is_no_op_in_apply_ws_event() {
        use omega_control_contracts::ws::BlueprintConfirmedEvent;
        let state = EngineState::default();
        let event = WsEvent::BlueprintConfirmed(BlueprintConfirmedEvent {
            blueprint_hash: "0x1".into(),
            strategy_id:    "SA".into(),
            block_number:   1,
            profit_net_eth: 0.001,
        });
        // BlueprintConfirmed produces no state change — returns None
        assert!(state.apply_ws_event(1, &event).is_none());
    }

    #[test]
    fn blueprint_confirmed_does_not_enter_obs_log() {
        use omega_control_contracts::ws::BlueprintConfirmedEvent;
        let mut state = EngineState::default();
        let rev0 = state.revision();
        state.record_obs_event(&WsEvent::BlueprintConfirmed(BlueprintConfirmedEvent {
            blueprint_hash: "0x1".into(),
            strategy_id:    "SA".into(),
            block_number:   1,
            profit_net_eth: 0.001,
        }));
        // revision still bumps (record_obs_event always bumps)
        assert!(state.revision() > rev0);
        // but no ring buffer entry is created
        assert_eq!(state.obs_log.len(), 0);
    }

    #[test]
    fn layer_backend_key_all_variants_covered() {
        for layer in LayerId::iter() {
            assert!(!layer_backend_key(layer).is_empty());
        }
    }

    #[test]
    fn layer_backend_key_matches_canonical_backend_strings() {
        assert_eq!(layer_backend_key(LayerId::SystemHealth),    "HEALTH");
        assert_eq!(layer_backend_key(LayerId::ExternalData),    "RPC");
        assert_eq!(layer_backend_key(LayerId::Oracle),          "ORACLE");
        assert_eq!(layer_backend_key(LayerId::Eil),             "COMPLIANCE");
        assert_eq!(layer_backend_key(LayerId::Risk),            "RISK");
        assert_eq!(layer_backend_key(LayerId::Security),        "SECURITY");
        assert_eq!(layer_backend_key(LayerId::Dag),             "DAG");
        assert_eq!(layer_backend_key(LayerId::Zk),              "ZK");
        assert_eq!(layer_backend_key(LayerId::HotPath),         "HOT_PATH");
        assert_eq!(layer_backend_key(LayerId::Strategy),        "STRATEGIES");
        assert_eq!(layer_backend_key(LayerId::Flashloan),       "FLASH_LOAN");
        assert_eq!(layer_backend_key(LayerId::Orchestrator),    "GAS_WAR");
        assert_eq!(layer_backend_key(LayerId::Relay),           "RELAY");
        assert_eq!(layer_backend_key(LayerId::Vault),           "ADDRESS_ROTATION");
        assert_eq!(layer_backend_key(LayerId::Observability),   "OBSERVABILITY");
        assert_eq!(layer_backend_key(LayerId::LossAttribution), "LOSS_ATTRIBUTION");
    }

    #[test]
    fn layer_backend_key_produces_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for layer in LayerId::iter() {
            let key = layer_backend_key(layer);
            assert!(
                seen.insert(key),
                "layer_backend_key produced a duplicate key {key:?} for {layer:?}"
            );
        }
    }
}