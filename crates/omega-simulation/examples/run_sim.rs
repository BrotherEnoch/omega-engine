// omega-engine\crates\omega-simulation\examples\run_sim.rs
//! Example / CLI entry point.
//!
//! Usage (PowerShell):
//!   $env:ARBITRUM_RPC_URL = "https://..."
//!   cargo run --example run_sim -p omega-simulation -- --cycles 20 --report out/sim_report.json
//!
//! Uses StrategyDetector:
//!   - CNRY: score-only (never builds blueprints — by design)
//!   - SA:   score → build_blueprint → submit on local Anvil fork

use clap::Parser;
use ethers::providers::Middleware;
use ethers::types::U256;
use omega_core::OmegaConfig;
use omega_simulation::{Bundle, ForkConfig, HarnessConfig, SimulationHarness, StrategyDetector};
use std::path::PathBuf;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value_t = 20)]
    cycles: u32,

    #[arg(long)]
    fork_block: Option<u64>,

    #[arg(long, default_value = "out/sim_report.json")]
    report: PathBuf,

    #[arg(long, default_value_t = 0)]
    signer_index: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let upstream_rpc_url = std::env::var("ARBITRUM_RPC_URL")
        .expect("set ARBITRUM_RPC_URL to a read-only Arbitrum RPC endpoint");

    let harness = SimulationHarness::start(HarnessConfig {
        fork: ForkConfig {
            upstream_rpc_url,
            fork_block_number: args.fork_block,
            port: 0,
            dev_accounts: 5,
            startup_timeout: std::time::Duration::from_secs(30),
        },
        cycles: args.cycles,
        signer_index: args.signer_index,
        report_output_path: Some(args.report.clone()),
        ..HarnessConfig::default()
    })
    .await?;

    tracing::info!(
        endpoint = harness.fork_endpoint(),
        "fork ready, running cycles"
    );
    eprintln!(
        "INFO: using StrategyDetector (CNRY score-only + SA blueprints). \
         Contract address is still a placeholder until fork deploy helper lands."
    );

    // Pull the fork's actual gas price rather than hardcoding one.
    // Arbitrum's basefee is typically a small fraction of a gwei; a
    // hardcoded mainnet-L1-shaped value here overpays gas by orders of
    // magnitude and skews net_profit_wei negative independent of whether
    // the underlying opportunity was genuinely profitable.
    let gas_price = harness.fork_provider().get_gas_price().await?;
    let max_priority_fee = gas_price / 10;
    let max_fee_per_gas = gas_price + max_priority_fee;

    let config = OmegaConfig::default();
    let detector = StrategyDetector::new(&config);

    let report = harness
        .run(detector, move |opp| Bundle {
            opportunity_id: opp.id.clone(),
            target_contract: opp.target_pool,
            calldata: opp.calldata.clone(),
            value: U256::zero(),
            gas_limit: opp.gas_estimate,
            max_fee_per_gas,
            max_priority_fee_per_gas: max_priority_fee,
        })
        .await?;

    println!("{}", report.summary_line());
    println!("full report written to {:?}", args.report);

    Ok(())
}
