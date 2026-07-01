// omega-frontend-arch/crates/omega-frontend/src/render.rs

use strum::IntoEnumIterator;

use omega_control_contracts::{
    health::{HealthStatus, LayerId},
    ws::WsConnectionStatus,
};

use crate::state::{EngineState, EngineStore, RealtimeStatus};

// ── Severity ──────────────────────────────────────────────────────────────────

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
            HealthStatus::Ok         => Severity::Ok,
            HealthStatus::Recovering => Severity::Warning,
            HealthStatus::Degraded   => Severity::Warning,
            HealthStatus::Halted     => Severity::Critical,
            HealthStatus::Unknown    => Severity::Unknown,
        }
    }
}

// ── LayerRow ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct LayerRow {
    pub layer:    LayerId,
    pub label:    &'static str,
    pub status:   HealthStatus,
    pub severity: Severity,
    pub message:  Option<String>,
}

// ── AlertBanner ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct AlertBanner {
    pub severity: Severity,
    pub message:  String,
}

// ── ObsEntry — a single rendered observability event ─────────────────────────

/// Flattened, display-ready observability event for the UI panel.
/// Derived from `ObservabilityEntry` inside `derive_frame`; the UI component
/// reads only this type, never `ObservabilityLog` directly.
#[derive(Debug, Clone, PartialEq)]
pub struct ObsEntry {
    /// UTC timestamp formatted for display.
    pub ts:      String,
    /// Short event kind label, e.g. "GAS_REVERT", "PROFIT_SPLIT".
    pub kind:    &'static str,
    /// CSS severity class: "obs-ok" | "obs-warn" | "obs-crit" | "obs-info".
    pub cls:     &'static str,
    /// Primary display line.
    pub summary: String,
    /// Optional secondary detail line.
    pub detail:  Option<String>,
}

// ── RenderFrame ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct RenderFrame {
    pub revision:         u64,
    pub overall_severity: Severity,
    pub ws_connected:     bool,
    pub ws_status_label:  &'static str,
    pub layers:           Vec<LayerRow>,
    pub alerts:           Vec<AlertBanner>,
    /// Most recent 50 observability events, newest first.
    pub obs_entries:      Vec<ObsEntry>,
    /// Snapshot of ObservabilityMetrics counters.
    pub obs_metrics:      ObsMetricsSnapshot,
}

/// Cheap copy of the metrics counters for the UI summary row.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ObsMetricsSnapshot {
    pub gas_model_reverts:            u64,
    pub ceiling_escalations:          u64,
    pub emergency_bundles_skipped:    u64,
    pub profit_splits:                u64,
    pub la_reorg_risks:               u64,
    pub simulation_errors:            u64,
    pub ws_gaps:                      u64,
}

impl RenderFrame {
    /// Legacy path — kept for EngineStore compat.
    pub fn from_store(store: &EngineStore) -> Self {
        let layers_raw = store.health.as_ref().map(|h| h.layers.as_slice()).unwrap_or(&[]);
        let halted_count = layers_raw.iter().filter(|l| l.state == "HALTED").count();
        let overall_severity = if halted_count > 0 { Severity::Critical }
            else if layers_raw.iter().any(|l| l.state == "DEGRADED") { Severity::Warning }
            else if layers_raw.iter().any(|l| l.is_operational) { Severity::Ok }
            else { Severity::Unknown };
        let ws_status_label = match store.realtime_status {
            RealtimeStatus::Authenticated => "Live",
            RealtimeStatus::Anonymous     => "Connected (anon)",
            RealtimeStatus::Connecting    => "Connecting…",
            RealtimeStatus::Lagged        => "Lagged",
            RealtimeStatus::Disconnected  => "Disconnected",
        };
        let mut alerts = Vec::new();
        if halted_count > 0 {
            alerts.push(AlertBanner {
                severity: Severity::Critical,
                message:  format!("⚠ {} engine layer(s) HALTED", halted_count),
            });
        }
        if store.model_paused == Some(true) {
            alerts.push(AlertBanner {
                severity: Severity::Warning,
                message:  "Gas model paused — ceiling escalation active".into(),
            });
        }
        Self {
            revision: store.revision,
            overall_severity,
            ws_connected: matches!(
                store.realtime_status,
                RealtimeStatus::Authenticated | RealtimeStatus::Anonymous
            ),
            ws_status_label,
            layers: Vec::new(),
            alerts,
            obs_entries: Vec::new(),
            obs_metrics: ObsMetricsSnapshot::default(),
        }
    }
}

// ── derive_frame ──────────────────────────────────────────────────────────────

#[must_use]
pub fn derive_frame(state: &EngineState) -> RenderFrame {
    let layers: Vec<LayerRow> = LayerId::iter()
        .map(|layer| {
            let status  = state.layer_status(layer);
            let message = state
                .layer_health(layer)
                .and_then(|lh| lh.message.as_deref())
                .map(|m| m.chars().take(120).collect());
            LayerRow {
                label:    layer_label(layer),
                layer,
                severity: Severity::from(status),
                status,
                message,
            }
        })
        .collect();

    let (ws_connected, ws_status_label) = match state.ws_status() {
        WsConnectionStatus::Connected           => (true,  "Live"),
        WsConnectionStatus::Connecting          => (false, "Connecting…"),
        WsConnectionStatus::Reconnecting { .. } => (false, "Reconnecting…"),
        WsConnectionStatus::Unavailable         => (false, "WS unavailable — polling"),
        WsConnectionStatus::Disconnected        => (false, "Disconnected"),
        WsConnectionStatus::AuthError           => (false, "Auth error"),
    };

    let overall_severity = Severity::from(state.overall_health());

    let mut alerts = Vec::new();
    if state.any_halted() {
        let n = layers.iter().filter(|r| r.severity == Severity::Critical).count();
        alerts.push(AlertBanner {
            severity: Severity::Critical,
            message:  format!("⚠ {} engine layer(s) HALTED", n),
        });
    }
    if state.gas_model_paused() {
        alerts.push(AlertBanner {
            severity: Severity::Warning,
            message:  "Gas model paused — ceiling escalation active. L2 governance review required.".into(),
        });
    }

    // ── Observability entries ─────────────────────────────────────────────
    use crate::observability::ObservabilityEventKind::*;
    use omega_control_contracts::ws::SimulationErrorSubCode;

    let recent_entries: Vec<_> = state.obs_log.recent(50).collect();
    let obs_entries: Vec<ObsEntry> = recent_entries
        .iter()
        .rev()
        .map(|e| {
            let ts = e.recorded_at.format("%H:%M:%S%.3f").to_string();
            match &e.kind {
                GasModelReverted { checkpoint_version, win_rate } => ObsEntry {
                    ts, kind: "GAS_REVERT", cls: "obs-warn",
                    summary: format!("Gas model reverted → checkpoint v{}", checkpoint_version),
                    detail:  Some(format!("win_rate = {:.1}%", win_rate * 100.0)),
                },
                CeilingEscalation { feature_key, hit_count } => ObsEntry {
                    ts, kind: "CEILING", cls: "obs-warn",
                    summary: format!("Ceiling escalation: {}", feature_key),
                    detail:  Some(format!("cumulative hits = {}", hit_count)),
                },
                EmergencySkipped { blueprint_hash, emergency_fee_gwei } => ObsEntry {
                    ts, kind: "EMERG_SKIP", cls: "obs-warn",
                    summary: format!("Emergency bundle skipped — {}gwei", emergency_fee_gwei),
                    detail:  Some(format!("blueprint {}", &blueprint_hash[..blueprint_hash.len().min(20)])),
                },
                ProfitSplit { blueprint_hash, pil_share_wei, dao_fee_wei } => ObsEntry {
                    ts, kind: "PROFIT", cls: "obs-ok",
                    summary: format!("Profit split settled"),
                    detail:  Some(format!(
                        "PIL {} wei  DAO {} wei  blueprint {}",
                        pil_share_wei, dao_fee_wei,
                        &blueprint_hash[..blueprint_hash.len().min(12)]
                    )),
                },
                LaReorgRisk { tx_hash, orphaned_block } => ObsEntry {
                    ts, kind: "REORG", cls: "obs-crit",
                    summary: format!("Reorg risk — block {} orphaned", orphaned_block),
                    detail:  Some(format!("tx {}", &tx_hash[..tx_hash.len().min(20)])),
                },
                SimulationError { blueprint_hash, sub_code } => {
                    let sub = match sub_code {
                        SimulationErrorSubCode::StateMismatch   => "state mismatch",
                        SimulationErrorSubCode::ExecutionRevert => "execution revert",
                        SimulationErrorSubCode::GasMiscalc      => "gas miscalc",
                    };
                    ObsEntry {
                        ts, kind: "SIM_ERR", cls: "obs-crit",
                        summary: format!("Simulation error: {}", sub),
                        detail:  Some(format!("blueprint {}", &blueprint_hash[..blueprint_hash.len().min(20)])),
                    }
                },
                WsGapDetected { expected_seq, got_seq } => ObsEntry {
                    ts, kind: "WS_GAP", cls: "obs-warn",
                    summary: format!("WS sequence gap: expected {} got {}", expected_seq, got_seq),
                    detail:  None,
                },
                WsStatusChange { status_label } => ObsEntry {
                    ts, kind: "WS_STATUS", cls: "obs-info",
                    summary: format!("WS status → {}", status_label),
                    detail:  None,
                },
            }
        })
        .collect();

    let m = &state.obs_log.metrics;
    let obs_metrics = ObsMetricsSnapshot {
        gas_model_reverts:         m.gas_model_reverts,
        ceiling_escalations:       m.ceiling_escalations,
        emergency_bundles_skipped: m.emergency_bundles_skipped,
        profit_splits:             m.profit_splits,
        la_reorg_risks:            m.la_reorg_risks,
        simulation_errors:         m.simulation_state_mismatches
                                    + m.simulation_execution_reverts
                                    + m.simulation_gas_miscalcs,
        ws_gaps:                   m.ws_gaps,
    };

    RenderFrame {
        revision: state.revision(),
        overall_severity,
        ws_connected,
        ws_status_label,
        layers,
        alerts,
        obs_entries,
        obs_metrics,
    }
}

// ── Layer labels ──────────────────────────────────────────────────────────────

/// Human-readable display label for each layer.
///
/// Variant order matches the canonical v12 LayerId enum (health.rs):
///   L00 SystemHealth, L01 ExternalData, L02 Oracle, L03 Risk, L04 Security,
///   L05 Eil, L06 Dag, L07 Zk, L08 HotPath, L09 Strategy, L10 Flashloan,
///   L11 Orchestrator, L12 Relay, L13 Vault, L14 Observability, L15 LossAttribution
fn layer_label(layer: LayerId) -> &'static str {
    match layer {
        LayerId::SystemHealth    => "Health FSM",
        LayerId::ExternalData    => "RPC / Nodes",
        LayerId::Oracle          => "Oracle Feeds",
        LayerId::Risk            => "Risk Engine",
        LayerId::Security        => "Security",
        LayerId::Eil             => "Compliance / EIL",
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_produces_16_rows() {
        let state = EngineState::default();
        let frame = derive_frame(&state);
        assert_eq!(frame.layers.len(), 16);
    }

    #[test]
    fn all_rows_unknown_on_empty_state() {
        let state = EngineState::default();
        let frame = derive_frame(&state);
        assert!(frame.layers.iter().all(|r| r.severity == Severity::Unknown));
    }

    #[test]
    fn no_alerts_on_clean_state() {
        let state = EngineState::default();
        assert!(derive_frame(&state).alerts.is_empty());
    }

    #[test]
    fn obs_entries_empty_on_fresh_state() {
        let state = EngineState::default();
        let frame = derive_frame(&state);
        assert!(frame.obs_entries.is_empty());
        assert_eq!(frame.obs_metrics.profit_splits, 0);
    }

    #[test]
    fn severity_from_health_status() {
        assert_eq!(Severity::from(HealthStatus::Ok),         Severity::Ok);
        assert_eq!(Severity::from(HealthStatus::Degraded),   Severity::Warning);
        assert_eq!(Severity::from(HealthStatus::Recovering), Severity::Warning);
        assert_eq!(Severity::from(HealthStatus::Halted),     Severity::Critical);
        assert_eq!(Severity::from(HealthStatus::Unknown),    Severity::Unknown);
    }

    #[test]
    fn layer_labels_cover_all_16_variants() {
        use strum::IntoEnumIterator;
        // Every LayerId variant must have a non-empty label. This catches
        // any future enum addition that wasn't reflected in layer_label.
        for layer in LayerId::iter() {
            assert!(
                !layer_label(layer).is_empty(),
                "layer_label returned empty string for {layer:?}"
            );
        }
    }

    #[test]
    fn oracle_label_is_oracle_feeds() {
        // Regression: previously Eil was mapped to "Oracle Feeds" and
        // Oracle had no entry (ChaosGuard => "Chaos Guard" took its slot).
        assert_eq!(layer_label(LayerId::Oracle), "Oracle Feeds");
        assert_eq!(layer_label(LayerId::Eil),    "Compliance / EIL");
    }
}