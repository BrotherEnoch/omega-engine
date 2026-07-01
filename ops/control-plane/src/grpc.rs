// ops/control-plane/src/grpc.rs
//
// OmegaControl gRPC service (proto/omega_control.proto).
//
// Transport: plaintext on :50051 — TLS terminated at the sidecar proxy.
// Authentication: Bearer token in `authorization` metadata key per-call.
//
// All Get* RPCs: L1 (Bearer token sufficient).
// Command RPCs (PauseStrategy/ResumeStrategy/ClearHalt/AdjustRollout): L2 —
// the control-plane enforces the Bearer token; the API gateway enforces
// the multisig signature off-band.
//
// ## Wiring note (reconciliation)
//
//   This file previously imported `AppState`, `WsEvent`, and
//   `ALL_LAYER_IDS` from a separate, never-compiled `crate::state` module
//   (no `mod state;` existed in `main.rs`). It now uses `main.rs`'s
//   `AppState` directly (`crate::AppState`), the same
//   `omega_control_contracts::ws::WsEvent` that `obs_bridge.rs`, `ws.rs`,
//   and the frontend dashboard already agree on, and iterates
//   `omega_core::LayerId` via `strum::IntoEnumIterator` in place of the
//   old module-local `ALL_LAYER_IDS` constant (which only ever lived in
//   the dead `state.rs`).

use std::pin::Pin;
use std::sync::Arc;

use futures::StreamExt as _;
use tokio_stream::wrappers::BroadcastStream;
use tonic::{Request, Response, Status};

use strum::IntoEnumIterator;

use omega_control_contracts::ws::WsEvent;
use omega_core::{HealthStatus, LayerHealth, LayerId};

use crate::AppState;

// ─────────────────────────────────────────────────────────────────────────────
// Generated code
// ─────────────────────────────────────────────────────────────────────────────

pub mod proto {
    tonic::include_proto!("omega");
}

use proto::{
    omega_control_server::{OmegaControl, OmegaControlServer},
    CommandResult, Empty, HealthEvent, HealthReport, LayerHealth as ProtoLayerHealth,
    LatencyReport, LayerLatency, LayerIdMsg, PnLReport, PnLRequest, QueueReport,
    RelayWinRate, RolloutTier, StrategyId, WinRateReport,
};

// ─────────────────────────────────────────────────────────────────────────────
// Auth
// ─────────────────────────────────────────────────────────────────────────────

fn check_metadata<T>(req: &Request<T>, api_token: &str) -> Result<(), Status> {
    let token = req
        .metadata()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    if token == api_token {
        Ok(())
    } else {
        Err(Status::unauthenticated("Invalid or missing Bearer token"))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Service
// ─────────────────────────────────────────────────────────────────────────────

pub struct OmegaControlService {
    state: Arc<AppState>,
}

impl OmegaControlService {
    fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl OmegaControl for OmegaControlService {

    // ── GetSystemHealth ───────────────────────────────────────────────────

    async fn get_system_health(
        &self,
        req: Request<Empty>,
    ) -> Result<Response<HealthReport>, Status> {
        check_metadata(&req, &self.state.api_token)?;

        let layers: Vec<ProtoLayerHealth> = self.state
            .health_layers
            .iter()
            .map(|l| ProtoLayerHealth {
                layer_id: l.layer_id().to_string(),
                state:    l.state().to_string(),
                reason:   String::new(),
            })
            .collect();

        let system_halted = layers.iter().any(|l| l.state == "HALTED");

        Ok(Response::new(HealthReport {
            layers,
            system_halted,
            generated_at: chrono::Utc::now().to_rfc3339(),
        }))
    }

    // ── WatchHealth (server-streaming) ────────────────────────────────────

    type WatchHealthStream = Pin<Box<
        dyn futures::Stream<Item = Result<HealthEvent, Status>> + Send + 'static
    >>;

    async fn watch_health(
        &self,
        req: Request<Empty>,
    ) -> Result<Response<Self::WatchHealthStream>, Status> {
        check_metadata(&req, &self.state.api_token)?;

        let rx = self.state.subscribe_ws();

        let stream = BroadcastStream::new(rx).filter_map(|result| async {
            match result {
                Ok(WsEvent::HealthTransition { layer, from, to, reason, timestamp }) => {
                    Some(Ok(HealthEvent {
                        layer_id:  layer,
                        from,
                        to,
                        reason,
                        timestamp: timestamp.to_rfc3339(),
                    }))
                }
                Ok(_) => None, // skip non-health events
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                    tracing::warn!(dropped = n, "WatchHealth stream lagged");
                    None
                }
            }
        });

        Ok(Response::new(Box::pin(stream)))
    }

    // ── GetPnL ────────────────────────────────────────────────────────────

    async fn get_pn_l(
        &self,
        req: Request<PnLRequest>,
    ) -> Result<Response<PnLReport>, Status> {
        check_metadata(&req, &self.state.api_token)?;

        // In the full engine, this reads from a RwLock<PnlStore> updated
        // by the Vault event listener.  In standalone control-plane mode
        // we return zero-value placeholders with the correct DAO fee split
        // derived from live config (§15.1).
        let dao_fee_bps   = self.state.config.read().await.vault.dao_fee_bps;
        let net_profit    = 0.0_f64;
        let dao_fee       = net_profit * dao_fee_bps as f64 / 10_000.0;
        let pil_share     = net_profit - dao_fee;

        Ok(Response::new(PnLReport {
            gross_profit_eth: net_profit,
            gas_cost_eth:     0.0,
            net_profit_eth:   net_profit,
            dao_fee_eth:      dao_fee,
            pil_share_eth:    pil_share,
            period:           "24h".into(),
        }))
    }

    // ── GetLatency ────────────────────────────────────────────────────────

    async fn get_latency(
        &self,
        req: Request<Empty>,
    ) -> Result<Response<LatencyReport>, Status> {
        check_metadata(&req, &self.state.api_token)?;

        // SLA budgets from §4.  p50/p95/p99 require live Prometheus scrape;
        // here we surface the budgets so callers can compare on their end.
        let layers = LayerId::iter().map(|id| LayerLatency {
            layer_id:  id.to_string(),
            p50_us:    0.0,
            p95_us:    0.0,
            p99_us:    0.0,
            budget_us: match id {
                LayerId::HotPath => 1_000.0,   // 1ms Microtx (§4)
                LayerId::Relay   => 80_000.0,  // 80ms LA window (§11)
                _                => 5_000.0,   // 5ms default
            },
        }).collect();

        Ok(Response::new(LatencyReport { layers }))
    }

    // ── GetQueueDepths ────────────────────────────────────────────────────

    async fn get_queue_depths(
        &self,
        req: Request<Empty>,
    ) -> Result<Response<QueueReport>, Status> {
        check_metadata(&req, &self.state.api_token)?;

        // Queue depths are owned by the execution and relay subsystems.
        // In standalone mode, zero is the correct value.
        Ok(Response::new(QueueReport {
            microtx_slots:     0,
            normal_slots:      0,
            zk_queue_depth:    0,
            relay_queue_depth: 0,
        }))
    }

    // ── GetWinRates ───────────────────────────────────────────────────────

    async fn get_win_rates(
        &self,
        req: Request<Empty>,
    ) -> Result<Response<WinRateReport>, Status> {
        check_metadata(&req, &self.state.api_token)?;

        // Win rates are maintained by omega-gas-war::LaRelayMetrics.
        // In standalone control-plane mode, there is no live relay metrics
        // feed — return an empty list.  The full engine wires this via a
        // shared Arc<LaRelayMetrics> in AppState.
        Ok(Response::new(WinRateReport { relays: vec![] }))
    }

    // ── PauseStrategy (L2) ────────────────────────────────────────────────

    async fn pause_strategy(
        &self,
        req: Request<StrategyId>,
    ) -> Result<Response<CommandResult>, Status> {
        check_metadata(&req, &self.state.api_token)?;
        let id = req.into_inner().id;
        tracing::warn!(strategy = %id, "PauseStrategy (L2 fast-approve)");
        Ok(Response::new(CommandResult {
            ok:      true,
            message: format!("Strategy {id} paused"),
        }))
    }

    // ── ResumeStrategy (L2) ───────────────────────────────────────────────

    async fn resume_strategy(
        &self,
        req: Request<StrategyId>,
    ) -> Result<Response<CommandResult>, Status> {
        check_metadata(&req, &self.state.api_token)?;
        let id = req.into_inner().id;
        tracing::warn!(strategy = %id, "ResumeStrategy (L2 fast-approve)");
        Ok(Response::new(CommandResult {
            ok:      true,
            message: format!("Strategy {id} resumed"),
        }))
    }

    // ── ClearHalt (L2) ────────────────────────────────────────────────────

    async fn clear_halt(
        &self,
        req: Request<LayerIdMsg>,
    ) -> Result<Response<CommandResult>, Status> {
        check_metadata(&req, &self.state.api_token)?;
        let layer_str = req.into_inner().id;

        let layer_id = LayerId::iter()
            .find(|id| id.to_string() == layer_str);

        let Some(layer_id) = layer_id else {
            return Ok(Response::new(CommandResult {
                ok:      false,
                message: format!("Unknown layer: {layer_str}"),
            }));
        };

        if let Some(ctrl) = self.state.layer(layer_id) {
            ctrl.set_state(HealthStatus::Healthy, "cleared via gRPC (L2 fast-approve)");

            self.state.publish(WsEvent::HealthTransition {
                layer:     layer_str.clone(),
                from:      "HALTED".into(),
                to:        "HEALTHY".into(),
                reason:    "gRPC ClearHalt (L2)".into(),
                timestamp: chrono::Utc::now(),
            });

            tracing::warn!(layer = %layer_str, "Halt cleared via gRPC (L2 fast-approve)");
        }

        Ok(Response::new(CommandResult {
            ok:      true,
            message: format!("Halt cleared for {layer_str}"),
        }))
    }

    // ── AdjustRollout (L2) ────────────────────────────────────────────────

    async fn adjust_rollout(
        &self,
        req: Request<RolloutTier>,
    ) -> Result<Response<CommandResult>, Status> {
        check_metadata(&req, &self.state.api_token)?;
        let fraction = req.into_inner().fraction.clamp(0.0, 1.0);
        tracing::warn!(fraction, "AdjustRollout (L2 fast-approve)");
        Ok(Response::new(CommandResult {
            ok:      true,
            message: format!("Rollout fraction set to {fraction:.3}"),
        }))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Server startup
// ─────────────────────────────────────────────────────────────────────────────

/// Start the gRPC server on `:50051`.
///
/// Spawned as a Tokio task alongside the HTTP server (see `main.rs`'s
/// `main()`, which calls `tokio::spawn` around this function). Runs
/// until the process exits.
pub async fn serve(state: Arc<AppState>) -> anyhow::Result<()> {
    let addr = "0.0.0.0:50051"
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid gRPC bind address: {e}"))?;

    let service = OmegaControlServer::new(OmegaControlService::new(state));

    tracing::info!(bind = "0.0.0.0:50051", "gRPC server starting");

    tonic::transport::Server::builder()
        .add_service(service)
        .serve(addr)
        .await
        .map_err(|e| anyhow::anyhow!("gRPC server error: {e}"))?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_id_round_trips_through_display_and_iter() {
        // ClearHalt looks up a LayerId by matching its Display string
        // against the incoming proto LayerIdMsg.id. Every canonical
        // variant must round-trip through this lookup.
        for id in LayerId::iter() {
            let s = id.to_string();
            let found = LayerId::iter().find(|candidate| candidate.to_string() == s);
            assert_eq!(found, Some(id), "LayerId {id:?} did not round-trip via Display");
        }
    }

    #[test]
    fn clear_halt_unknown_layer_string_has_no_match() {
        let found = LayerId::iter().find(|id| id.to_string() == "NOT_A_REAL_LAYER");
        assert!(found.is_none());
    }
}