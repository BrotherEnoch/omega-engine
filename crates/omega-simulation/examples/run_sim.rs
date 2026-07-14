// omega-engine\crates\omega-simulation\examples\run_sim.rs
//! Example / CLI entry point.
//!
//! Usage:
//!   ARBITRUM_RPC_URL=https://... cargo run --example run_sim -- \
//!       --cycles 50 --fork-block 245000000 --report out/report.json
//!
//! Swap `FixtureDetector` below for the engine's real `OpportunityDetector`
//! implementation (from omega-core) once wiring this into the actual
//! workspace. Until that swap happens, this binary only exercises plumbing
//! (fork spawn, submission, reporting) with placeholder opportunities aimed
//! at the zero address — its `net_profit_wei` output is not a meaningful
//! profitability signal. See the warning printed at startup.

use async_trait::async_trait;
use clap::Parser;
use ethers::providers::Middleware;
use ethers::types::{Address, Bytes, U256};
use omega_simulation::{
    Bundle, ForkConfig, HarnessConfig, Opportunity, OpportunityDetector, OpportunityKind,
    SimulationHarness,
};
use std::path::PathBuf;
use std::str::FromStr;

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

/// Stand-in detector: emits one plausible-looking opportunity per cycle so
/// the harness/plumbing can be exercised end-to-end before the real
/// omega-core detector is wired in. Replace before treating output as a
/// real phase-gate signal.
struct FixtureDetector {
    dummy_pool: Address,
    dummy_asset: Address,
}

#[async_trait]
impl OpportunityDetector for FixtureDetector {
    async fn next_opportunities(
        &mut self,
        block_number: u64,
    ) -> omega_simulation::error::Result<Vec<Opportunity>> {
        Ok(vec![Opportunity {
            id: format!("fixture-{block_number}"),
            kind: OpportunityKind::Arbitrage,
            target_pool: self.dummy_pool,
            flash_loan_asset: self.dummy_asset,
            flash_loan_amount: U256::from(1_000_000_000_000_000_000u128),
            calldata: Bytes::default(),
            expected_profit_wei: U256::from(0),
            gas_estimate: U256::from(300_000u64),
        }])
    }
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

    tracing::info!(endpoint = harness.fork_endpoint(), "fork ready, running cycles");
    eprintln!(
        "WARNING: this run uses FixtureDetector (placeholder opportunities \
         targeting the zero address). net_profit_wei in the resulting \
         report reflects only gas cost, not real strategy performance. \
         Swap in the real omega-core detector before using this output as \
         a phase-gate signal."
    );

    // Pull the fork's actual gas price rather than hardcoding one.
    // Arbitrum's basefee is typically a small fraction of a gwei; a
    // hardcoded mainnet-L1-shaped value here overpays gas by orders of
    // magnitude and skews net_profit_wei negative independent of whether
    // the underlying opportunity was genuinely profitable.
    let gas_price = harness.fork_provider().get_gas_price().await?;
    let max_priority_fee = gas_price / 10;
    let max_fee_per_gas = gas_price + max_priority_fee;

    let detector = FixtureDetector {
        // NOTE: replace with real deployed contract/token addresses on the
        // forked network before treating results as meaningful.
        dummy_pool: Address::from_str("0x0000000000000000000000000000000000000000")?,
        dummy_asset: Address::from_str("0x0000000000000000000000000000000000000000")?,
    };

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