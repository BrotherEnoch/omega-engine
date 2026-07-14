// omega-engine\crates\omega-simulation\src\report.rs
use crate::error::Result;
use crate::traits::{Opportunity, Receipt};
use chrono::{DateTime, Utc};
use ethers::types::U256;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CycleResult {
    pub cycle_index: u32,
    pub block_number: u64,
    pub opportunity: Option<Opportunity>,
    pub expected_profit_wei: Option<U256>,
    pub receipt: Option<Receipt>,
    pub error: Option<String>,
}

impl CycleResult {
    pub fn empty(cycle_index: u32, block_number: u64) -> Self {
        Self {
            cycle_index,
            block_number,
            opportunity: None,
            expected_profit_wei: None,
            receipt: None,
            error: None,
        }
    }

    pub fn executed(
        cycle_index: u32,
        block_number: u64,
        opportunity: Opportunity,
        expected_profit_wei: U256,
        receipt: Receipt,
    ) -> Self {
        Self {
            cycle_index,
            block_number,
            opportunity: Some(opportunity),
            expected_profit_wei: Some(expected_profit_wei),
            receipt: Some(receipt),
            error: None,
        }
    }

    pub fn failed(
        cycle_index: u32,
        block_number: u64,
        opportunity: Opportunity,
        error: String,
    ) -> Self {
        Self {
            cycle_index,
            block_number,
            opportunity: Some(opportunity),
            expected_profit_wei: None,
            receipt: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationReport {
    pub fork_endpoint: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub cycles: Vec<CycleResult>,
}

impl SimulationReport {
    pub fn new(fork_endpoint: String) -> Self {
        Self {
            fork_endpoint,
            started_at: Utc::now(),
            finished_at: None,
            cycles: Vec::new(),
        }
    }

    pub fn record(&mut self, cycle: CycleResult) {
        self.cycles.push(cycle);
    }

    /// Total cycles that surfaced at least one opportunity attempt.
    pub fn attempted_count(&self) -> usize {
        self.cycles.iter().filter(|c| c.opportunity.is_some()).count()
    }

    /// Successful on-fork executions (bundle landed, contract didn't revert).
    pub fn success_count(&self) -> usize {
        self.cycles
            .iter()
            .filter(|c| c.receipt.as_ref().map(|r| r.success).unwrap_or(false))
            .count()
    }

    /// Reverts + submitter errors combined — anything that did NOT result
    /// in a successful on-fork execution despite an opportunity being found.
    pub fn failure_count(&self) -> usize {
        self.attempted_count().saturating_sub(self.success_count())
    }

    /// Sum of realized (on-fork) profit across successful executions, in
    /// wei. Negative values mean the "opportunity" was net-unprofitable
    /// once gas and loan fees were paid on real forked state — exactly the
    /// kind of thing Phase 0 shadow-mode heuristics can miss.
    pub fn total_realized_profit_wei(&self) -> i128 {
        self.cycles
            .iter()
            .filter_map(|c| c.receipt.as_ref())
            .filter_map(|r| r.realized_profit_wei)
            .sum()
    }

    pub fn success_rate(&self) -> f64 {
        let attempted = self.attempted_count();
        if attempted == 0 {
            return 0.0;
        }
        self.success_count() as f64 / attempted as f64
    }

    pub fn finish(&mut self) {
        self.finished_at = Some(Utc::now());
    }

    pub fn write_json(&self, path: &Path) -> Result<()> {
        let file = std::fs::File::create(path)?;
        serde_json::to_writer_pretty(file, self)?;
        Ok(())
    }

    /// One-line human summary, suitable for CI logs or a phase-gate
    /// checklist — deliberately conservative (rounds down, calls out zero
    /// attempts) so it can't be misread as "looks fine" by default.
    pub fn summary_line(&self) -> String {
        format!(
            "cycles={} attempted={} success={} failed={} success_rate={:.1}% net_profit_wei={}",
            self.cycles.len(),
            self.attempted_count(),
            self.success_count(),
            self.failure_count(),
            self.success_rate() * 100.0,
            self.total_realized_profit_wei()
        )
    }
}