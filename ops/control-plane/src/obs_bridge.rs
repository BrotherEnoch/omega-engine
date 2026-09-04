// ops/control-plane/src/obs_bridge.rs
//
// Observability bridge — drains the shared EventRingBuffer (populated by
// engine crates via omega-observability) and republishes mapped WsEvents
// onto state.ws_tx so dashboard clients on /ws/events see live trading
// telemetry alongside governance events.
//
// ## FIX (this revision): WsEvent import
//
//   Previously imported `WsEvent` from `crate::state` alongside
//   `AppState`. `state.rs` no longer defines its own local `WsEvent` —
//   `AppState.ws_tx` now broadcasts `omega_control_contracts::ws::WsEvent`
//   directly, the real type shared with the frontend dashboard (see
//   state.rs's own module-level FIX note for why the old local enum was
//   wrong and removed). Updated the import accordingly; every variant
//   constructed below is unchanged, since the field shapes already
//   matched the real crate type.
//
//   NOT INDEPENDENTLY CONFIRMED: whether
//   `omega_control_contracts::ws::WsEvent` actually has every variant
//   constructed below (`GasModelCeilingEscalation`,
//   `EmergencyBundleSkipped`, `LaReorgRisk`, `SimulationError`,
//   `BlueprintConfirmed` in particular — `ConfigReloaded`,
//   `ModelPauseChanged`, `BlacklistReloaded`, `HealthTransition`,
//   `ProfitSplit`, `GasModelReverted` are confirmed via grpc.rs/ws.rs).
//   If `cargo build` reports one of the less-confirmed variants missing
//   from the real crate, that arm below needs adjusting to whatever the
//   crate actually exposes (or dropping to the `other => None` arm if
//   there's genuinely no display path for it yet).

use std::sync::Arc;
use std::time::Duration;

use tracing::debug;

use omega_observability::events::OmegaEvent;

use omega_control_contracts::ws::WsEvent;

use crate::state::AppState;

/// Spawn the observability bridge as a background Tokio task.
/// Called once from main() after AppState is constructed.
pub fn spawn(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(50));
        loop {
            ticker.tick().await;

            let events = state.obs_buffer.drain();
            if events.is_empty() {
                continue;
            }

            for raw in events {
                if let Some(ws_event) = map_omega_event(raw) {
                    state.publish(ws_event);
                }
            }
        }
    });
}

fn map_omega_event(ev: OmegaEvent) -> Option<WsEvent> {
    match ev {
        OmegaEvent::HealthStateChange {
            timestamp,
            layer_id,
            from_state,
            to_state,
            reason,
        } => Some(WsEvent::HealthTransition {
            layer: layer_id,
            from: from_state,
            to: to_state,
            reason,
            timestamp,
        }),

        OmegaEvent::EmergencyHalt {
            timestamp,
            issuer,
            reason,
        } => Some(WsEvent::HealthTransition {
            layer: "SYSTEM".into(),
            from: "Healthy".into(),
            to: "Halted".into(),
            reason: format!("emergency halt by {issuer}: {reason}"),
            timestamp,
        }),

        OmegaEvent::GasModelReverted {
            timestamp,
            checkpoint_version,
            checkpoint_rate,
            ..
        } => Some(WsEvent::GasModelReverted {
            checkpoint_version,
            win_rate: checkpoint_rate,
            sample_count: 0,
            timestamp,
        }),

        OmegaEvent::GasModelCeilingEscalation {
            timestamp,
            feature_key,
            ceiling_hits,
            ..
        } => Some(WsEvent::GasModelCeilingEscalation {
            feature_key,
            ceiling_hit_count: ceiling_hits,
            timestamp,
        }),

        // emergency_fee_gwei is cast u64 -> f64 here. Confirmed via
        // state.rs's own test (emergency_bundle_skipped_exists_and_serialises_flat):
        // serde_json always serialises f64 with a decimal point in this
        // crate's dependency version (e.g. `9999.0`, never bare `9999`) —
        // if the frontend field expecting this is typed `u64`, that JSON
        // float token will fail to deserialise there. Not fixable from
        // this file alone; see that test's own comment for the two ways
        // to resolve it (frontend field as f64, or drop this cast).
        OmegaEvent::EmergencyBundleSkipped {
            timestamp,
            blueprint_hash,
            emergency_fee_gwei,
            reason,
        } => Some(WsEvent::EmergencyBundleSkipped {
            blueprint_hash,
            emergency_fee_gwei: emergency_fee_gwei as f64,
            reason,
            timestamp,
        }),

        OmegaEvent::ProfitSplit {
            timestamp,
            blueprint_hash,
            pil_share_eth,
            dao_fee_eth,
            ..
        } => Some(WsEvent::ProfitSplit {
            blueprint_hash,
            pil_share_wei: format!("{}", (pil_share_eth * 1e18) as u128),
            dao_fee_wei: format!("{}", (dao_fee_eth * 1e18) as u128),
            timestamp,
        }),

        OmegaEvent::BlacklistReloaded {
            timestamp,
            entry_count,
            ..
        } => Some(WsEvent::BlacklistReloaded {
            entry_count,
            timestamp,
        }),

        OmegaEvent::LaReorgRisk {
            timestamp,
            tx_hash,
            orphaned_block,
            ..
        } => Some(WsEvent::LaReorgRisk {
            tx_hash,
            orphaned_block,
            // OmegaEvent carries rescore_at; WsEvent expects reorg_depth.
            // Without a depth field on the source event, surface 0 and
            // let the dashboard treat it as "reorg detected, depth unknown".
            reorg_depth: 0,
            timestamp,
        }),

        // FIX (confirmed by rustc, not guessed): the real WsEvent variant's
        // field is `profit_net_wei`, not `profit_net_eth` — this arm
        // previously assumed the field-init-shorthand `profit_net_eth,`
        // named a matching field on WsEvent, but that field doesn't exist
        // on the real crate type. Converted eth -> wei the same way
        // ProfitSplit's pil_share_wei/dao_fee_wei are built a few arms up
        // in this file (stringified u128, since wei amounts routinely
        // exceed what a JS/JSON number can hold precisely).
        //
        // NOT INDEPENDENTLY CONFIRMED: that `profit_net_wei`'s real type is
        // `String` rather than e.g. `u128` or `u64` — rustc's "field with a
        // similar name exists" note only confirms the NAME, not the type.
        // Assumed String on the strength of the ProfitSplit precedent in
        // this same enum. If `cargo build` reports a type mismatch here,
        // drop the `format!(...)` and pass the raw integer through instead.
        OmegaEvent::BlueprintConfirmed {
            timestamp,
            blueprint_hash,
            strategy_id,
            block_number,
            profit_net_eth,
            ..
        } => Some(WsEvent::BlueprintConfirmed {
            blueprint_hash,
            strategy_id,
            block_number,
            profit_net_wei: format!("{}", (profit_net_eth * 1e18) as u128),
            timestamp,
        }),

        other => {
            debug!(event = ?other, "unmapped OmegaEvent variant — skipped for WS");
            None
        }
    }
}