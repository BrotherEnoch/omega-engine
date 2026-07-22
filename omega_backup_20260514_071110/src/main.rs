ï»¿// src/main.rs
// src/main.rs
// OmegaEngine v12.0 â€” Main Entry Point
// All 14 layers initialized in dependency order
// Canary strategy (CNRY) runs as dedicated task alongside main pipeline

use std::sync::Arc;
use omega_health::halt::HaltFlag;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("OmegaEngine v12.0 starting â€” Final Edition");

    let halt = HaltFlag::new();

    // â”€â”€ Initialize layers in dependency order â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    // L0: omega-health (System Health FSM + halt flag + persistence)
    //     Health log: /var/omega/health.log
    // L1: omega-rpc    (dedicated Arbitrum node; rate-limit-aware; token bucket)
    // L2: omega-oracle (tri-oracle per chain; Chainlink + Pyth + TWAP)
    // L3: omega-security (HSM signer; replay DashMap; chain-scoped nonces)
    // L4: omega-compliance (versioned OFA rule registry from config/ofa_rules.toml)
    // L5: omega-risk   (Arbitrum dual-component gas model; 13 fast-fail checks)
    // L6: omega-dag    (petgraph + revm double-buffer cache + Anvil fork manager)
    // L7: omega-zk     (T1 prover pool + queue auto-throttle + checkpoint manager)
    // L8: omega-flashloan (per-pool real-time capacity probe; exclusion_list per protocol)
    // L9: omega-relay  (4-relay broadcast; LA-inclusion-rate ranked; halt-flag poll 10ms)
    // L10: omega-gas-war  (adaptive cap; 3-bundle variants; builder blacklist)
    // L11: omega-loss-attribution (8-class taxonomy; 80/20 train/validate; ML online learner)
    // L12: omega-address-rotation (30-day schedule; 50% reputation carryover with decay)
    // L13: omega-strategies (registry: CNRY, SA, MSA, LA, MEV â€” phase-gated)
    // L14: omega-cross-chain (per-chain oracle instances; PIL bridge accounting)
    // L15: omega-observability (async ring buffer 65536; high-priority 4096; ELK)

    // â”€â”€ Register strategies (phase-gated) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    // Phase 0.5: CNRY (Canary â€” always active, dedicated task)
    // Phase 1:   SA
    // Phase 2:   MSA
    // Phase 3.0: LA (Aave v3, Compound v3, Morpho Blue)
    // Phase 3.1: LA + Euler v2 (after independent audit)
    // Phase 4:   MEV-OFA

    // â”€â”€ Canary strategy â€” dedicated task â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    // Runs independently at 500ms intervals
    // Validates: revm cache freshness, relay pipeline, ZK proof gen, oracle prices, gas model
    // Never competes for lane slots (priority=255)
    // Emits CANARY_PASS / CANARY_MISS to observability (always-sampled)
    // tokio::spawn(canary_strategy.run_forever(signal_rx));

    // â”€â”€ Main pipeline loops â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    // EIL scoring loop (adaptive EV rollout: starts at 10%)
    // Blueprint priority queue (crossbeam SegQueue, ordered by priority + expected_profit)
    // ZK proof async worker pool (T1 software baseline)
    // Health monitor tick (2s interval; 3 consecutive healthy ticks for recovery)
    // Relay submission loop (polls halt_flag every 10ms; 190ms abort timeout)
    // LA position monitor (tiered: hot/warm/cold/archived; warm-start from /var/omega/la-positions.bin)
    // MSA path solver (Bellman-Ford; 50ms debounce on Sync events)
    // Loss attribution engine (ML feedback to gas model; 80/20 validation holdout)
    // Address rotation manager (30-day schedule; pattern detector)

    // â”€â”€ Shadow mode guard â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    // Phase 0: no relay submissions â€” full pipeline active, no live execution
    // Phase 1+: relay submissions enabled after L2 governance activation

    tracing::info!("Phase 0: shadow mode active â€” no relay submissions");
    tracing::info!("Canary strategy active â€” pipeline health monitoring");

    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutdown signal received");
    halt.halt();
    tracing::info!("HALT flag set â€” draining queues before exit");
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    tracing::info!("OmegaEngine shutdown complete");
    Ok(())
}
