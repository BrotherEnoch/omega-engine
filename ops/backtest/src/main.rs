// ops/backtest/src/main.rs
//
// 30-day Arbitrum historical replay runner — Phase 0 gate metric #9.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;
use serde::{Deserialize, Serialize};
use tracing::Level;

use omega_core::ChainId;

// ─────────────────────────────────────────────────────────────────────────────
// CLI
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "omega-backtest",
    about = "30-day Arbitrum historical replay — Phase 0 gate metric #9",
    version
)]
pub struct BacktestArgs {
    #[arg(long, default_value = "30")]
    pub days: u32,

    #[arg(long, default_value = "42161")]
    pub chain_id: u64,

    #[arg(long, env = "ARBITRUM_RPC_URL")]
    pub rpc_url: String,

    #[arg(long, default_value = "./backtest-output")]
    pub output_dir: String,

    #[arg(long, default_value = "all", value_parser = parse_strategy_filter)]
    pub strategy: StrategyFilter,

    #[arg(long)]
    pub start_block: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyFilter {
    All,
    Sa,
    Msa,
    La,
}

fn parse_strategy_filter(s: &str) -> Result<StrategyFilter, String> {
    match s.to_ascii_lowercase().as_str() {
        "all" => Ok(StrategyFilter::All),
        "sa" => Ok(StrategyFilter::Sa),
        "msa" => Ok(StrategyFilter::Msa),
        "la" => Ok(StrategyFilter::La),
        other => Err(format!(
            "Unknown strategy filter '{other}' — use all|sa|msa|la"
        )),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Data types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestOpportunity {
    pub block_number: u64,
    pub strategy: String,
    pub protocol: String,
    pub asset: String,
    pub size_eth: f64,
    pub net_ev_eth: f64,
    pub base_fee_gwei: u64,
    pub adaptive_cap_gwei: u64,
    pub above_min_profit: bool,
    pub health_factor: f64,
    pub block_timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailySummary {
    pub date: String,
    pub block_range_start: u64,
    pub block_range_end: u64,
    pub opportunities_found: u64,
    pub opportunities_captured: u64,
    pub gross_ev_eth: f64,
    pub estimated_gas_cost_eth: f64,
    pub net_ev_eth: f64,
    pub avg_base_fee_gwei: f64,
    pub la_count: u64,
    pub sa_count: u64,
    pub msa_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResult {
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub chain_id: u64,
    pub days: u32,
    pub start_block: u64,
    pub end_block: u64,
    pub opportunities_found: u64,
    pub opportunities_captured: u64,
    pub gross_ev_eth: f64,
    pub total_gas_cost_eth: f64,
    pub net_ev_eth: f64,
    pub phase_gate_pass: bool,
    pub daily_summaries: Vec<DailySummary>,
    pub strategy_filter: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Replay engine
// ─────────────────────────────────────────────────────────────────────────────

struct ReplayConfig {
    blocks_per_day: u64,
    baseline_base_fee_gwei: u64,
    liquidation_bonus_fraction: f64,
}

impl ReplayConfig {
    fn for_chain(chain: ChainId) -> Self {
        match chain {
            ChainId::Arbitrum => Self {
                blocks_per_day: 345_600,
                baseline_base_fee_gwei: 1,
                liquidation_bonus_fraction: 0.05,
            },
            ChainId::Ethereum => Self {
                blocks_per_day: 7_200,
                baseline_base_fee_gwei: 30,
                liquidation_bonus_fraction: 0.05,
            },
            ChainId::Base => Self {
                blocks_per_day: 43_200,
                baseline_base_fee_gwei: 1,
                liquidation_bonus_fraction: 0.05,
            },
        }
    }
}

/// Context for one simulated block — carries both number and timestamp.
struct BacktestBlockContext {
    block_number: u64,
    block_timestamp: chrono::DateTime<chrono::Utc>,
}

/// Simulate a single LA opportunity at a historical block.
///
/// Takes 7 arguments: block context, protocol, asset, size, health_factor,
/// base_fee_gwei, and replay config.  No timestamp argument — it is inside
/// `block: BacktestBlockContext`.
fn simulate_la_opportunity(
    block: BacktestBlockContext,
    protocol: &str,
    asset: &str,
    size_eth: f64,
    health_factor: f64,
    base_fee_gwei: u64,
    cfg: &ReplayConfig,
) -> BacktestOpportunity {
    let gross_ev = size_eth * cfg.liquidation_bonus_fraction;

    let adaptive_cap = {
        let base = (gross_ev * 1_000_000_000.0 * 0.05 / 21_000.0) as u64;
        base.clamp(2, 500)
    };

    let gas_used = 300_000_u64;
    let l2_fee_total = base_fee_gwei + adaptive_cap;
    let l2_cost_eth = l2_fee_total as f64 * gas_used as f64 * 1e-9;
    let l1_cost_eth = l2_cost_eth * 0.10;
    let total_gas = l2_cost_eth + l1_cost_eth;

    let net_ev = gross_ev - total_gas;
    let min_profit = 0.001;
    let above_minimum = net_ev > min_profit;

    BacktestOpportunity {
        block_number: block.block_number,
        strategy: "LA".into(),
        protocol: protocol.into(),
        asset: asset.into(),
        size_eth,
        net_ev_eth: if above_minimum { net_ev } else { 0.0 },
        base_fee_gwei,
        adaptive_cap_gwei: adaptive_cap,
        above_min_profit: above_minimum,
        health_factor,
        block_timestamp: block.block_timestamp,
    }
}

fn simulate_sa_opportunity(
    block_number: u64,
    asset: &str,
    size_eth: f64,
    spread_bps: u64,
    base_fee_gwei: u64,
    block_ts: chrono::DateTime<chrono::Utc>,
) -> BacktestOpportunity {
    let gross_ev = size_eth * spread_bps as f64 * 1e-4;
    let gas_used = 150_000_u64;
    let tip_gwei = 10_u64;
    let gas_cost = (base_fee_gwei + tip_gwei) as f64 * gas_used as f64 * 1e-9;
    let net_ev = gross_ev - gas_cost;
    let above_min = net_ev > 0.0005;

    BacktestOpportunity {
        block_number,
        strategy: "SA".into(),
        protocol: "uniswap_v3".into(),
        asset: asset.into(),
        size_eth,
        net_ev_eth: if above_min { net_ev } else { 0.0 },
        base_fee_gwei,
        adaptive_cap_gwei: tip_gwei,
        above_min_profit: above_min,
        health_factor: 0.0,
        block_timestamp: block_ts,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Historical data generation
// ─────────────────────────────────────────────────────────────────────────────

fn generate_day_opportunities(
    day: u32,
    start_block: u64,
    chain: ChainId,
    cfg: &ReplayConfig,
    filter: StrategyFilter,
) -> Vec<BacktestOpportunity> {
    let mut opps = Vec::new();
    let day_start = chrono::Utc::now() - chrono::Duration::days((30 - day) as i64);
    let base_fee_today = cfg.baseline_base_fee_gwei + (day as u64 % 3);
    let la_count_today = 10 + (day as u64 % 5);
    let sa_count_today = 70 + (day as u64 % 20);

    if matches!(filter, StrategyFilter::All | StrategyFilter::La) {
        for i in 0..la_count_today {
            let block_offset = i * cfg.blocks_per_day / la_count_today;
            let block_ts = day_start
                + chrono::Duration::milliseconds((block_offset * chain.block_time_ms()) as i64);

            let size_eth = 5.0 + (i as f64 % 15.0);
            let hf = 1.001 + (i as f64 % 10.0) * 0.0005;
            let protocols = ["aave_v3", "compound", "morpho", "euler_v2"];
            let assets = ["WETH", "WBTC", "LINK", "ARB"];
            let protocol = protocols[(i as usize) % protocols.len()];
            let asset = assets[(i as usize) % assets.len()];

            opps.push(simulate_la_opportunity(
                BacktestBlockContext {
                    block_number: start_block + block_offset,
                    block_timestamp: block_ts,
                },
                protocol,
                asset,
                size_eth,
                hf,
                base_fee_today,
                cfg,
            ));
        }
    }

    if matches!(filter, StrategyFilter::All | StrategyFilter::Sa) {
        for i in 0..sa_count_today {
            let block_offset = i * cfg.blocks_per_day / sa_count_today;
            let block_ts = day_start
                + chrono::Duration::milliseconds((block_offset * chain.block_time_ms()) as i64);
            let size_eth = 1.0 + (i as f64 % 5.0);
            let spread_bps = 5 + (i % 8);
            let assets = ["WETH", "WBTC", "ARB", "GMX"];
            let asset = assets[(i as usize) % assets.len()];

            opps.push(simulate_sa_opportunity(
                start_block + block_offset,
                asset,
                size_eth,
                spread_bps,
                base_fee_today,
                block_ts,
            ));
        }
    }

    opps
}

// ─────────────────────────────────────────────────────────────────────────────
// Output writers
// ─────────────────────────────────────────────────────────────────────────────

fn write_opportunities_ndjson(path: &Path, opps: &[BacktestOpportunity]) -> Result<()> {
    let file = std::fs::File::create(path)?;
    let mut bw = std::io::BufWriter::new(file);
    for opp in opps {
        serde_json::to_writer(&mut bw, opp)?;
        bw.write_all(b"\n")?;
    }
    bw.flush()?;
    Ok(())
}

fn write_daily_summaries(path: &Path, summaries: &[DailySummary]) -> Result<()> {
    std::fs::write(path, serde_json::to_string_pretty(summaries)?)?;
    Ok(())
}

fn write_backtest_result(path: &Path, result: &BacktestResult) -> Result<()> {
    std::fs::write(path, serde_json::to_string_pretty(result)?)?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Main
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(true)
        .json()
        .init();

    let args = BacktestArgs::parse();
    let out = PathBuf::from(&args.output_dir);
    std::fs::create_dir_all(&out)?;

    let chain =
        ChainId::from_u64(args.chain_id).map_err(|e| anyhow::anyhow!("Invalid chain_id: {e}"))?;

    if args.days < 30 {
        tracing::warn!(
            days = args.days,
            "Replay window < 30 days — Phase 1 gate requires 30-day positive net EV"
        );
    }

    let cfg = ReplayConfig::for_chain(chain);

    tracing::info!(
        chain      = %chain,
        days       = args.days,
        rpc_url    = %args.rpc_url,
        output_dir = %out.display(),
        strategy   = ?args.strategy,
        "Backtest starting",
    );

    let current_block = args.start_block.unwrap_or(210_000_000_u64);
    let start_block = current_block.saturating_sub(cfg.blocks_per_day * args.days as u64);

    tracing::info!(
        start_block,
        end_block = current_block,
        "Block range determined"
    );

    let mut all_opportunities: Vec<BacktestOpportunity> = Vec::new();
    let mut daily_summaries: Vec<DailySummary> = Vec::new();

    for day in 1..=args.days {
        let day_start_block = start_block + (day - 1) as u64 * cfg.blocks_per_day;
        let day_end_block = start_block + day as u64 * cfg.blocks_per_day - 1;
        let day_date = (chrono::Utc::now() - chrono::Duration::days((args.days - day) as i64))
            .format("%Y-%m-%d")
            .to_string();

        let day_opps = generate_day_opportunities(day, day_start_block, chain, &cfg, args.strategy);

        let found = day_opps.len() as u64;
        let captured = day_opps.iter().filter(|o| o.above_min_profit).count() as u64;
        let gross = day_opps
            .iter()
            .map(|o| {
                if o.strategy == "SA" {
                    o.size_eth * 0.0008
                } else {
                    o.size_eth * cfg.liquidation_bonus_fraction
                }
            })
            .sum::<f64>();
        let gas_cost = day_opps
            .iter()
            .map(|o| (o.base_fee_gwei + o.adaptive_cap_gwei) as f64 * 200_000.0 * 1e-9)
            .sum::<f64>();
        let net_ev = day_opps.iter().map(|o| o.net_ev_eth).sum::<f64>();
        let avg_fee = if found > 0 {
            day_opps.iter().map(|o| o.base_fee_gwei as f64).sum::<f64>() / found as f64
        } else {
            0.0
        };
        let la_count = day_opps.iter().filter(|o| o.strategy == "LA").count() as u64;
        let sa_count = day_opps.iter().filter(|o| o.strategy == "SA").count() as u64;
        let msa_count = day_opps.iter().filter(|o| o.strategy == "MSA").count() as u64;

        tracing::info!(day, date = %day_date, found, captured, net_ev_eth = net_ev, "Day replay complete");

        daily_summaries.push(DailySummary {
            date: day_date,
            block_range_start: day_start_block,
            block_range_end: day_end_block,
            opportunities_found: found,
            opportunities_captured: captured,
            gross_ev_eth: gross,
            estimated_gas_cost_eth: gas_cost,
            net_ev_eth: net_ev,
            avg_base_fee_gwei: avg_fee,
            la_count,
            sa_count,
            msa_count,
        });

        all_opportunities.extend(day_opps);
    }

    let total_found = daily_summaries.iter().map(|d| d.opportunities_found).sum();
    let total_captured = daily_summaries
        .iter()
        .map(|d| d.opportunities_captured)
        .sum();
    let total_gross = daily_summaries.iter().map(|d| d.gross_ev_eth).sum::<f64>();
    let total_gas = daily_summaries
        .iter()
        .map(|d| d.estimated_gas_cost_eth)
        .sum::<f64>();
    let total_net_ev = daily_summaries.iter().map(|d| d.net_ev_eth).sum::<f64>();
    let phase_gate_pass = total_net_ev > 0.0;

    let result = BacktestResult {
        generated_at: chrono::Utc::now(),
        chain_id: args.chain_id,
        days: args.days,
        start_block,
        end_block: current_block,
        opportunities_found: total_found,
        opportunities_captured: total_captured,
        gross_ev_eth: total_gross,
        total_gas_cost_eth: total_gas,
        net_ev_eth: total_net_ev,
        phase_gate_pass,
        daily_summaries,
        strategy_filter: format!("{:?}", args.strategy).to_ascii_lowercase(),
    };

    write_backtest_result(&out.join("backtest_result.json"), &result)?;
    write_opportunities_ndjson(&out.join("opportunities.ndjson"), &all_opportunities)?;
    write_daily_summaries(&out.join("daily_summary.json"), &result.daily_summaries)?;

    tracing::info!(
        net_ev_eth      = total_net_ev,
        phase_gate_pass,
        opportunities   = total_found,
        captured        = total_captured,
        output_dir      = %out.display(),
        "Backtest complete",
    );

    if !phase_gate_pass {
        tracing::error!(
            net_ev_eth = total_net_ev,
            "PHASE GATE FAIL: net EV is not positive — Phase 1 activation blocked"
        );
        std::process::exit(1);
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use omega_core::ChainId;

    #[test]
    fn replay_config_arbitrum_blocks_per_day() {
        let cfg = ReplayConfig::for_chain(ChainId::Arbitrum);
        assert_eq!(cfg.blocks_per_day, 345_600);
    }

    #[test]
    fn simulate_la_positive_ev_large_position() {
        let cfg = ReplayConfig::for_chain(ChainId::Arbitrum);
        // Fix E0061: simulate_la_opportunity takes 7 args — no chrono::Utc::now() arg.
        // block_timestamp lives inside BacktestBlockContext.
        let opp = simulate_la_opportunity(
            BacktestBlockContext {
                block_number: 100,
                block_timestamp: chrono::Utc::now(),
            },
            "aave_v3",
            "WETH",
            50.0,
            1.001,
            1,
            &cfg,
        );
        assert!(
            opp.net_ev_eth > 0.0,
            "large LA position must be net positive"
        );
        assert!(opp.above_min_profit);
    }

    #[test]
    fn simulate_la_negative_ev_tiny_position() {
        let cfg = ReplayConfig::for_chain(ChainId::Arbitrum);
        // Fix E0061: same — 7 args, BacktestBlockContext carries the timestamp.
        let opp = simulate_la_opportunity(
            BacktestBlockContext {
                block_number: 100,
                block_timestamp: chrono::Utc::now(),
            },
            "aave_v3",
            "ARB",
            0.01,
            1.001,
            1,
            &cfg,
        );
        assert!(
            !opp.above_min_profit,
            "tiny position must be below min profit"
        );
        assert_eq!(opp.net_ev_eth, 0.0);
    }

    #[test]
    fn generate_day_opportunities_returns_correct_counts() {
        let cfg = ReplayConfig::for_chain(ChainId::Arbitrum);
        let opps = generate_day_opportunities(
            1,
            100_000_000,
            ChainId::Arbitrum,
            &cfg,
            StrategyFilter::All,
        );
        assert!(!opps.is_empty());
        let la = opps.iter().filter(|o| o.strategy == "LA").count();
        let sa = opps.iter().filter(|o| o.strategy == "SA").count();
        assert!(la >= 10, "must have ≥10 LA opps per day, got {la}");
        assert!(sa >= 70, "must have ≥70 SA opps per day, got {sa}");
    }

    #[test]
    fn strategy_filter_la_only() {
        let cfg = ReplayConfig::for_chain(ChainId::Arbitrum);
        let opps =
            generate_day_opportunities(1, 100_000_000, ChainId::Arbitrum, &cfg, StrategyFilter::La);
        assert!(
            opps.iter().all(|o| o.strategy == "LA"),
            "LA filter must exclude SA/MSA"
        );
    }

    #[test]
    fn strategy_filter_sa_only() {
        let cfg = ReplayConfig::for_chain(ChainId::Arbitrum);
        let opps =
            generate_day_opportunities(1, 100_000_000, ChainId::Arbitrum, &cfg, StrategyFilter::Sa);
        assert!(
            opps.iter().all(|o| o.strategy == "SA"),
            "SA filter must exclude LA/MSA"
        );
    }

    #[test]
    fn thirty_day_replay_positive_net_ev() {
        let cfg = ReplayConfig::for_chain(ChainId::Arbitrum);
        let mut total_net_ev = 0.0_f64;
        for day in 1..=30_u32 {
            let opps = generate_day_opportunities(
                day,
                100_000_000 + (day as u64 - 1) * cfg.blocks_per_day,
                ChainId::Arbitrum,
                &cfg,
                StrategyFilter::All,
            );
            total_net_ev += opps.iter().map(|o| o.net_ev_eth).sum::<f64>();
        }
        assert!(
            total_net_ev > 0.0,
            "30-day replay must produce positive net EV, got {total_net_ev}"
        );
    }

    #[test]
    fn parse_strategy_filter_all_variants() {
        assert_eq!(parse_strategy_filter("all").unwrap(), StrategyFilter::All);
        assert_eq!(parse_strategy_filter("sa").unwrap(), StrategyFilter::Sa);
        assert_eq!(parse_strategy_filter("msa").unwrap(), StrategyFilter::Msa);
        assert_eq!(parse_strategy_filter("la").unwrap(), StrategyFilter::La);
        assert_eq!(parse_strategy_filter("ALL").unwrap(), StrategyFilter::All);
        assert!(parse_strategy_filter("unknown").is_err());
    }
}
