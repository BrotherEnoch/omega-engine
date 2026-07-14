// omega-engine\crates\omega-testnet\src\report.rs
//! Report format for a completed (or in-progress) testnet dry-run.
//!
//! Extends what `omega-simulation::SimulationReport` can measure with the
//! fields its own docs say it explicitly cannot: relay latency, bundle
//! inclusion probability, and sim-vs-real profit divergence.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::Result;

/// Outcome of a single bundle submission to a real (testnet) relay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayOutcome {
    pub cycle_index: u32,
    pub submitted_at: DateTime<Utc>,

    /// Time from submission to the relay accepting (not necessarily
    /// including) the bundle. `None` if the relay never responded.
    pub relay_ack_latency_ms: Option<u64>,

    /// Whether the relay accepted the bundle for consideration (this is
    /// NOT the same as on-chain inclusion).
    pub relay_accepted: bool,

    /// Whether the bundle was actually included in a block.
    pub included_onchain: bool,

    /// Blocks between submission and inclusion, if included.
    pub blocks_to_inclusion: Option<u32>,

    /// Profit expected at scoring time (from the same detector logic used
    /// in `omega-simulation`), in wei.
    pub expected_profit_wei: i128,

    /// Profit actually realized on the testnet chain, in wei. `None` if
    /// the bundle was never included.
    pub realized_profit_wei: Option<i128>,

    /// Relay-side rejection reason, if `relay_accepted` is false.
    pub rejection_reason: Option<String>,
}

impl RelayOutcome {
    /// Absolute difference between expected and realized profit, in wei.
    /// `None` if there's no realized figure to compare against.
    pub fn profit_divergence_wei(&self) -> Option<i128> {
        self.realized_profit_wei
            .map(|realized| (realized - self.expected_profit_wei).abs())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestnetReport {
    pub run_label: String,
    pub chain_id: u64,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub outcomes: Vec<RelayOutcome>,
}

impl TestnetReport {
    pub fn new(run_label: String, chain_id: u64) -> Self {
        Self {
            run_label,
            chain_id,
            started_at: Utc::now(),
            finished_at: None,
            outcomes: Vec::new(),
        }
    }

    pub fn record(&mut self, outcome: RelayOutcome) {
        self.outcomes.push(outcome);
    }

    pub fn finish(&mut self) {
        self.finished_at = Some(Utc::now());
    }

    pub fn total_submitted(&self) -> usize {
        self.outcomes.len()
    }

    pub fn relay_acceptance_rate(&self) -> f64 {
        if self.outcomes.is_empty() {
            return 0.0;
        }
        let accepted = self.outcomes.iter().filter(|o| o.relay_accepted).count();
        accepted as f64 / self.outcomes.len() as f64
    }

    pub fn inclusion_rate(&self) -> f64 {
        if self.outcomes.is_empty() {
            return 0.0;
        }
        let included = self.outcomes.iter().filter(|o| o.included_onchain).count();
        included as f64 / self.outcomes.len() as f64
    }

    /// Mean relay acknowledgment latency across outcomes that received one.
    pub fn mean_ack_latency_ms(&self) -> Option<f64> {
        let latencies: Vec<u64> = self
            .outcomes
            .iter()
            .filter_map(|o| o.relay_ack_latency_ms)
            .collect();
        if latencies.is_empty() {
            return None;
        }
        Some(latencies.iter().sum::<u64>() as f64 / latencies.len() as f64)
    }

    /// Mean absolute divergence between expected and realized profit, in
    /// wei, across included bundles. Large values here mean the
    /// detector's profitability math doesn't survive contact with a real
    /// relay/chain, independent of whether individual trades were
    /// profitable.
    pub fn mean_profit_divergence_wei(&self) -> Option<f64> {
        let divergences: Vec<i128> = self
            .outcomes
            .iter()
            .filter_map(|o| o.profit_divergence_wei())
            .collect();
        if divergences.is_empty() {
            return None;
        }
        Some(divergences.iter().sum::<i128>() as f64 / divergences.len() as f64)
    }

    pub fn total_realized_profit_wei(&self) -> i128 {
        self.outcomes
            .iter()
            .filter_map(|o| o.realized_profit_wei)
            .sum()
    }

    /// Breakdown of relay rejection reasons by frequency, most common
    /// first. Empty reasons are grouped as "unspecified".
    pub fn rejection_taxonomy(&self) -> Vec<(String, usize)> {
        use std::collections::HashMap;
        let mut counts: HashMap<String, usize> = HashMap::new();
        for outcome in &self.outcomes {
            if !outcome.relay_accepted {
                let reason = outcome
                    .rejection_reason
                    .clone()
                    .unwrap_or_else(|| "unspecified".to_string());
                *counts.entry(reason).or_insert(0) += 1;
            }
        }
        let mut v: Vec<(String, usize)> = counts.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v
    }

    pub fn summary_line(&self) -> String {
        format!(
            "run={} chain_id={} submitted={} relay_accept_rate={:.1}% inclusion_rate={:.1}% \
             mean_ack_latency_ms={} net_realized_profit_wei={} mean_profit_divergence_wei={}",
            self.run_label,
            self.chain_id,
            self.total_submitted(),
            self.relay_acceptance_rate() * 100.0,
            self.inclusion_rate() * 100.0,
            self.mean_ack_latency_ms()
                .map(|v| format!("{v:.0}"))
                .unwrap_or_else(|| "n/a".to_string()),
            self.total_realized_profit_wei(),
            self.mean_profit_divergence_wei()
                .map(|v| format!("{v:.0}"))
                .unwrap_or_else(|| "n/a".to_string()),
        )
    }

    pub fn write_json(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = std::fs::File::create(path)?;
        serde_json::to_writer_pretty(file, self)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(expected: i128, realized: Option<i128>, accepted: bool, included: bool) -> RelayOutcome {
        RelayOutcome {
            cycle_index: 0,
            submitted_at: Utc::now(),
            relay_ack_latency_ms: Some(120),
            relay_accepted: accepted,
            included_onchain: included,
            blocks_to_inclusion: if included { Some(1) } else { None },
            expected_profit_wei: expected,
            realized_profit_wei: realized,
            rejection_reason: if accepted { None } else { Some("stale_state".into()) },
        }
    }

    #[test]
    fn rates_computed_correctly() {
        let mut r = TestnetReport::new("test".into(), 11155111);
        r.record(outcome(100, Some(90), true, true));
        r.record(outcome(100, None, false, false));
        assert!((r.relay_acceptance_rate() - 0.5).abs() < 1e-9);
        assert!((r.inclusion_rate() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn profit_divergence_only_counts_included() {
        let mut r = TestnetReport::new("test".into(), 11155111);
        r.record(outcome(100, Some(80), true, true)); // divergence 20
        r.record(outcome(100, None, false, false));   // no divergence, no realized
        assert_eq!(r.mean_profit_divergence_wei(), Some(20.0));
        assert_eq!(r.total_realized_profit_wei(), 80);
    }

    #[test]
    fn rejection_taxonomy_groups_by_reason() {
        let mut r = TestnetReport::new("test".into(), 11155111);
        r.record(outcome(100, None, false, false));
        r.record(outcome(100, None, false, false));
        r.record(outcome(100, Some(90), true, true));
        let tax = r.rejection_taxonomy();
        assert_eq!(tax, vec![("stale_state".to_string(), 2)]);
    }

    #[test]
    fn empty_report_has_zero_rates_not_nan() {
        let r = TestnetReport::new("empty".into(), 11155111);
        assert_eq!(r.relay_acceptance_rate(), 0.0);
        assert_eq!(r.inclusion_rate(), 0.0);
        assert_eq!(r.mean_ack_latency_ms(), None);
    }
}