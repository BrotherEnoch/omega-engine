// src/main.rs — OmegaEngine v12.0 Main Entry Point
//
// Required: ARBITRUM_RPC_URL (WebSocket endpoint)
// Optional: OMEGA_CONFIG (default: config/default.toml)

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tracing::Level;

// LayerHealth trait must be in scope for .state(), .layer_id(), .set_state()
// to resolve on Arc<LayerHealthImpl>
use omega_core::{HealthState, LayerId, LayerHealth, OmegaConfig, StrategyId};
use omega_dag::{DagConfig, ExecutionDag};
use omega_health::{halt::HaltFlag, LayerHealthImpl};
use omega_hot_path::{HotPathConfig, HotPathRequest, HotPathRunner, MICROTX_GAS_LIMIT};
use omega_observability::{EventRingBuffer, ExporterConfig, OmegaExporter, Sampler, DEFAULT_CAPACITY};
use omega_oracle::PerChainOracle;
use omega_rpc::{
    rate_limiter::RpcRateLimiter,
    run_dex_sync_stream, run_fee_oracle_stream, run_lending_protocol_stream,
    run_mev_share_stream, run_pending_tx_stream,
    OmegaRpcClient, RpcClientConfig,
};
use omega_strategies::{registry::StrategyRegistryBuilder, CnryStrategy, StrategyRegistry};
use omega_zk::{config::ProverTierConfig, ProofQueue, ProofWorkerPool, ZkConfig};

const CHAIN_ID:         u64  = 42_161;
const DEFAULT_RPS:      u32  = 500;
const SHUTDOWN_DRAIN_S: u64  = 5;
const DEFAULT_CONFIG:   &str = "config/default.toml";

// ── Config ────────────────────────────────────────────────────────────────────

fn load_config(path: &str) -> Result<OmegaConfig> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        tracing::warn!(path, "Config not found — using defaults");
        return Ok(OmegaConfig::default());
    }
    let s = std::fs::read_to_string(p).with_context(|| format!("reading {path}"))?;
    let cfg: OmegaConfig = toml::from_str(&s).with_context(|| format!("parsing {path}"))?;
    let errs = cfg.validate();
    if !errs.is_empty() {
        anyhow::bail!("Config errors:\n{}", errs.join("\n"));
    }
    Ok(cfg)
}

// ── Health layers ─────────────────────────────────────────────────────────────

// new_bare() returns Arc<Self> directly — do NOT wrap in Arc::new again
fn make_layers() -> [Arc<LayerHealthImpl>; 16] {
    [
        LayerId::Health,          LayerId::Rpc,
        LayerId::Oracle,          LayerId::Security,
        LayerId::Compliance,      LayerId::Risk,
        LayerId::Dag,             LayerId::Zk,
        LayerId::FlashLoan,       LayerId::Relay,
        LayerId::GasWar,          LayerId::LossAttribution,
        LayerId::AddressRotation, LayerId::Strategies,
        LayerId::HotPath,         LayerId::Observability,
    ]
    .map(LayerHealthImpl::new_bare)
}

fn find_layer(layers: &[Arc<LayerHealthImpl>; 16], id: LayerId) -> Arc<LayerHealthImpl> {
    layers.iter()
        .find(|l| l.layer_id() == id)
        .cloned()
        .unwrap_or_else(|| panic!("layer {id:?} not found"))
}

fn as_health(h: Arc<LayerHealthImpl>) -> Arc<dyn LayerHealth> {
    h as Arc<dyn LayerHealth>
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(true)
        .json()
        .init();

    tracing::info!("OmegaEngine v12.0 starting");

    let rpc_url = std::env::var("ARBITRUM_RPC_URL")
        .context("ARBITRUM_RPC_URL must be set")?;
    let config_path = std::env::var("OMEGA_CONFIG")
        .unwrap_or_else(|_| DEFAULT_CONFIG.to_string());

    let config       = load_config(&config_path)?;
    let active_phase = config.active_phase;

    tracing::info!(active_phase, chain_id = CHAIN_ID, "Config loaded");
    if active_phase == 0 {
        tracing::info!("Phase 0: shadow mode — relay submission suppressed");
    }

    // ── L0: HaltFlag + 16 health layers ──────────────────────────────────────
    let halt   = HaltFlag::new();
    let layers = make_layers();

    // ── L15: Observability ────────────────────────────────────────────────────
    let obs_buffer = EventRingBuffer::new(DEFAULT_CAPACITY);
    let (sd_tx, sd_rx) = tokio::sync::watch::channel(false);
    {
        let buf     = obs_buffer.clone();
        let sampler = Sampler::new(1.0);
        let cfg     = ExporterConfig::default();
        tokio::spawn(async move {
            OmegaExporter::run(buf, sampler, cfg, sd_rx).await;
        });
    }
    tracing::info!("L15 observability running");

    // ── L1: RPC client ────────────────────────────────────────────────────────
    let rpc = OmegaRpcClient::connect_with_retry(
        RpcClientConfig::new(&rpc_url, DEFAULT_RPS, CHAIN_ID)
    ).await
    .with_health(as_health(find_layer(&layers, LayerId::Rpc)));

    { let r = rpc.clone();
      tokio::spawn(async move { r.run_block_subscription().await }); }

    // Extract ws_url and rate_limiter for subscription fns
    // Subscription fns take (ws_url, chain_id, limiter, tx) directly
    let ws_url   = rpc_url.clone();
    let limiter  = Arc::new(RpcRateLimiter::new());

    let (fee_tx, fee_rx)  = tokio::sync::broadcast::channel(256);
    let (dex_tx, dex_rx)  = tokio::sync::broadcast::channel(1024);
    let (lend_tx, lend_rx)= tokio::sync::broadcast::channel(512);
    let (ptx_tx, _ptx_rx) = tokio::sync::broadcast::channel(512);
    let (mev_tx, _mev_rx) = tokio::sync::broadcast::channel(256);

    { let u = ws_url.clone(); let l = Arc::clone(&limiter); let t = ptx_tx.clone();
      tokio::spawn(async move { run_pending_tx_stream(u, CHAIN_ID, l, t).await }); }
    { let u = ws_url.clone(); let l = Arc::clone(&limiter); let t = fee_tx.clone();
      tokio::spawn(async move { run_fee_oracle_stream(u, CHAIN_ID, l, t).await }); }
    { let u = ws_url.clone(); let l = Arc::clone(&limiter); let t = dex_tx.clone();
      tokio::spawn(async move { run_dex_sync_stream(u, CHAIN_ID, l, t).await }); }
    { let u = ws_url.clone(); let l = Arc::clone(&limiter); let t = lend_tx.clone();
      tokio::spawn(async move { run_lending_protocol_stream(u, CHAIN_ID, l, t).await }); }
    { let t = mev_tx.clone();
      tokio::spawn(async move { run_mev_share_stream(t).await }); }

    tracing::info!("L1 RPC: 5 subscription streams running");

    // ── L2: Oracle ────────────────────────────────────────────────────────────
    let oracle = PerChainOracle::new(CHAIN_ID)
        .with_health(as_health(find_layer(&layers, LayerId::Oracle)));

    { let o = Arc::clone(&oracle);
      tokio::spawn(async move { o.run_fee_oracle(fee_rx).await }); }
    { let o = Arc::clone(&oracle);
      tokio::spawn(async move { o.run_dex_sync(dex_rx).await }); }
    { let o = Arc::clone(&oracle);
      tokio::spawn(async move { o.run_lending_protocol(lend_rx).await }); }

    tracing::info!("L2 oracle: 3 update streams running");

    // ── L6: DAG ───────────────────────────────────────────────────────────────
    let dag = Arc::new(Mutex::new(ExecutionDag::new(DagConfig {
        microtx_slots: 16, normal_slots: 4, eviction_log_capacity: 1_000,
    })));
    tracing::info!("L6 DAG initialised");

    // ── L7: ZK ────────────────────────────────────────────────────────────────
    let zk_cfg = ZkConfig {
        prover_tier:          ProverTierConfig::T1Software,
        worker_count:         std::thread::available_parallelism()
                                  .map(|n| n.get()).unwrap_or(4).min(8),
        microtx_sla_ms:       1_200,
        normal_sla_ms:        4_000,
        proof_queue_throttle: 128,
        proof_queue_suspend:  256,
        proof_queue_halt:     512,
        allow_skip_in_shadow: active_phase == 0,
        checkpoint_dir:       config.ml.checkpoint_dir.clone(),
        max_checkpoints:      config.ml.checkpoint_retention,
    };
    let proof_queue = ProofQueue::new(zk_cfg.clone());
    let _pool       = ProofWorkerPool::start(zk_cfg, proof_queue.clone());
    tracing::info!("L7 ZK: proof worker pool started");

    // ── L8: Hot-path ──────────────────────────────────────────────────────────
    let (hp_runner, hp_tx) = HotPathRunner::new(HotPathConfig {
        channel_capacity: 64, revm_trust_window_blocks: 1,
    });
    { let h = find_layer(&layers, LayerId::HotPath);
      tokio::spawn(async move {
          hp_runner.run().await;
          h.set_state(HealthState::Halted, "hot-path runner exited unexpectedly");
      }); }
    tracing::info!("L8 hot-path: runner started");

    // ── L13: Strategy registry ────────────────────────────────────────────────
    let registry = StrategyRegistryBuilder::new(active_phase)
        .register(CnryStrategy::new(CHAIN_ID, &config))
        .expect("CNRY registration must succeed")
        .build();

    tracing::info!(
        total  = registry.len(),
        active = registry.active_strategies().len(),
        phase  = active_phase,
        "L13 strategy registry built",
    );

    // ── Canary loop ───────────────────────────────────────────────────────────
    { let cnry  = registry.get(StrategyId::Cnry).expect("CNRY in registry");
      let ora2  = Arc::clone(&oracle);
      let halt2 = halt.clone();
      tokio::spawn(async move { run_canary_loop(cnry, ora2, halt2, 500).await }); }

    // ── Scoring loop ──────────────────────────────────────────────────────────
    { let reg   = registry.clone();
      let ora3  = Arc::clone(&oracle);
      let dag2  = Arc::clone(&dag);
      let halt3 = halt.clone();
      let tx    = hp_tx.clone();
      let pq    = proof_queue.clone();
      let ph    = active_phase;
      tokio::spawn(async move {
          run_scoring_loop(reg, ora3, dag2, tx, pq, halt3, ph).await;
      }); }

    // ── Health monitor ────────────────────────────────────────────────────────
    { let ls    = layers.clone();
      let halt4 = halt.clone();
      tokio::spawn(async move { run_health_monitor(ls, halt4).await }); }

    tracing::info!(
        active_phase, chain_id = CHAIN_ID,
        "OmegaEngine v12.0 running — all layers initialised",
    );

    // ── Shutdown ──────────────────────────────────────────────────────────────
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutdown signal received");
    let _ = sd_tx.send(true);
    halt.halt(LayerId::Health, "operator shutdown");
    tokio::time::sleep(Duration::from_secs(SHUTDOWN_DRAIN_S)).await;
    tracing::info!("OmegaEngine shutdown complete");
    Ok(())
}

// ── Background tasks ──────────────────────────────────────────────────────────

async fn run_canary_loop(
    cnry:        Arc<dyn omega_core::StrategyTrait>,
    oracle:      Arc<PerChainOracle>,
    halt:        HaltFlag,
    interval_ms: u64,
) {
    let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        if halt.is_halted() { break; }
        let snap = oracle.snapshot();
        let sig  = omega_core::SignalState {
            state_version:    snap.state_version,
            chain_id:         CHAIN_ID,
            block_number:     snap.fee.block_number,
            base_fee_gwei:    snap.fee.base_fee_gwei,
            l1_data_fee_gwei: snap.fee.l1_data_fee_gwei,
            state_hash:       snap.state_hash,
        };
        match cnry.score(&sig).await {
            Ok(op) => tracing::debug!(score = op.score, "CANARY_PASS"),
            Err(e) => tracing::warn!(error = %e, "CANARY_MISS"),
        }
    }
}

async fn run_scoring_loop(
    registry:     StrategyRegistry,
    oracle:       Arc<PerChainOracle>,
    dag:          Arc<Mutex<ExecutionDag>>,
    hp_tx:        tokio::sync::mpsc::Sender<HotPathRequest>,
    proof_queue:  ProofQueue,
    halt:         HaltFlag,
    active_phase: u8,
) {
    let mut rx = oracle.subscribe();
    loop {
        if halt.is_halted() { break; }
        match rx.recv().await {
            Ok(_) => {
                let snap = oracle.snapshot();
                let sig  = omega_core::SignalState {
                    state_version:    snap.state_version,
                    chain_id:         CHAIN_ID,
                    block_number:     snap.fee.block_number,
                    base_fee_gwei:    snap.fee.base_fee_gwei,
                    l1_data_fee_gwei: snap.fee.l1_data_fee_gwei,
                    state_hash:       snap.state_hash,
                };
                for strategy in registry.active_strategies() {
                    if strategy.strategy_id().is_canary() { continue; }
                    let s2   = sig.clone();
                    let dag2 = Arc::clone(&dag);
                    let tx2  = hp_tx.clone();
                    let pq2  = proof_queue.clone();
                    let h2   = halt.clone();
                    let ph   = active_phase;
                    tokio::spawn(async move {
                        score_and_admit(strategy, s2, dag2, tx2, pq2, h2, ph).await;
                    });
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) =>
                tracing::warn!(skipped = n, "scoring loop lagged"),
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn score_and_admit(
    strategy:     Arc<dyn omega_core::StrategyTrait>,
    signal:       omega_core::SignalState,
    dag:          Arc<Mutex<ExecutionDag>>,
    hp_tx:        tokio::sync::mpsc::Sender<HotPathRequest>,
    proof_queue:  ProofQueue,
    halt:         HaltFlag,
    active_phase: u8,
) {
    if halt.is_halted() { return; }

    let op = match strategy.score(&signal).await {
        Ok(op) if op.score > 0.0 => op,
        _ => return,
    };
    let _ = op;

    let bp = match strategy.build_blueprint(&signal).await {
        Ok(bp) => bp,
        Err(e) => { tracing::debug!(error = %e, "build_blueprint failed"); return; }
    };

    { let mut g = dag.lock().await;
      if g.admit(bp.clone(), &[]).is_err() { return; } }

    let hot = strategy.hot_path_eligible()
        && bp.lane == omega_core::Lane::Microtx
        && bp.l2_exec_gas_estimate <= MICROTX_GAS_LIMIT;

    if hot {
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        if hp_tx.try_send(HotPathRequest { blueprint: bp.clone(), resp_tx: rtx }).is_ok() {
            if let Ok(resp) = rrx.await {
                if active_phase >= 1 && resp.result.is_ok() {
                    tracing::info!(hash = %bp.blueprint_hash, "hot-path blueprint ready");
                }
            }
        }
    } else {
        let hb: [u8; 32] = *bp.blueprint_hash;
        let profit: u128  = bp.expected_profit_net.try_into().unwrap_or(u128::MAX);
        let micro         = bp.lane == omega_core::Lane::Microtx;
        if let Ok(rx) = proof_queue.submit(
            hb, profit, CHAIN_ID, bp.strategy_id.to_string(), micro,
        ) {
            if let Ok(Ok(proof)) = rx.await {
                if active_phase >= 1 {
                    tracing::info!(
                        hash   = %bp.blueprint_hash,
                        gen_ms = proof.generation_ms,
                        "ZK proof ready",
                    );
                }
            }
        }
    }

    let mut g = dag.lock().await;
    g.complete(bp.blueprint_hash);
}

async fn run_health_monitor(
    layers: [Arc<LayerHealthImpl>; 16],
    halt:   HaltFlag,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(2));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        if halt.is_halted() { break; }
        let halted   = layers.iter().filter(|l| l.state() == HealthState::Halted).count();
        let degraded = layers.iter().filter(|l| l.state() == HealthState::Degraded).count();
        if halted > 0 {
            tracing::error!(halted, degraded, "health check: layers HALTED");
        } else if degraded > 0 {
            tracing::warn!(degraded, "health check: layers degraded");
        } else {
            tracing::debug!("health check: all layers healthy");
        }
    }
}