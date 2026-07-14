// omega-engine\crates\omega-testnet\src\gate.rs
//! Phase 1 gate criteria — tracked as data, not tribal knowledge.
//!
//! Each criterion has a fixed target and an updatable observed value.
//! `GateStatus::all_met()` is the single source of truth for "are we
//! actually allowed to call this Phase 1," so the answer can't quietly
//! drift into "probably fine" without someone deliberately marking a
//! criterion as met.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::report::TestnetReport;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateCriteria {
    /// Minimum number of cycles the canary/soak run must complete.
    pub min_cycles: u32,
    /// Minimum continuous soak duration, in hours.
    pub min_soak_hours: u32,
    /// Minimum acceptable relay acceptance rate (0.0-1.0).
    pub min_relay_acceptance_rate: f64,
    /// Maximum acceptable mean profit divergence, in wei, between expected
    /// and realized profit on included bundles.
    pub max_mean_profit_divergence_wei: f64,
    /// Net realized profit must be >= this value (can be negative, e.g.
    /// zero or a small allowed loss for a canary run) for the gate to pass
    /// purely on the numbers.
    pub min_net_realized_profit_wei: i128,
}

impl Default for GateCriteria {
    fn default() -> Self {
        Self {
            min_cycles: 500,
            min_soak_hours: 72,
            min_relay_acceptance_rate: 0.90,
            max_mean_profit_divergence_wei: 50_000_000_000_000, // 0.00005 ETH
            min_net_realized_profit_wei: 0,
        }
    }
}

/// Manual attestations that can't be derived from a report automatically —
/// each requires a human to explicitly record that it happened.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ManualAttestations {
    pub kill_switch_tested_at: Option<DateTime<Utc>>,
    pub kill_switch_tested_by: Option<String>,

    pub circuit_breaker_reviewed_at: Option<DateTime<Utc>>,
    pub circuit_breaker_reviewed_by: Option<String>,

    pub key_custody_reviewed_at: Option<DateTime<Utc>>,
    pub key_custody_reviewed_by: Option<String>,

    pub multisig_reviewed_at: Option<DateTime<Utc>>,
    pub multisig_reviewed_by: Option<String>,
}

/// A single criterion's evaluated status, for display/checklist rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionStatus {
    pub name: String,
    pub met: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateStatus {
    pub criteria: Vec<CriterionStatus>,
}

impl GateStatus {
    pub fn all_met(&self) -> bool {
        self.criteria.iter().all(|c| c.met)
    }

    /// Renders the checklist as markdown, suitable for pasting into a
    /// phase-gate doc or CI summary.
    pub fn to_markdown(&self) -> String {
        let mut out = String::from("# Phase 1 Gate Checklist\n\n");
        for c in &self.criteria {
            let mark = if c.met { "x" } else { " " };
            out.push_str(&format!("- [{mark}] **{}** — {}\n", c.name, c.detail));
        }
        out.push_str(&format!(
            "\n**Overall: {}**\n",
            if self.all_met() { "GATE PASSED" } else { "GATE NOT PASSED" }
        ));
        out
    }
}

/// Evaluates `criteria` and `attestations` against a completed `report`,
/// producing the full checklist. This function only reads data — it never
/// decides on its own that a criterion is met; every automatic check is
/// derived directly from report numbers, and every manual criterion is
/// only "met" if a human explicitly recorded it in `attestations`.
pub fn evaluate(
    report: &TestnetReport,
    criteria: &GateCriteria,
    attestations: &ManualAttestations,
) -> GateStatus {
    let mut list = Vec::new();

    let cycles_met = report.total_submitted() as u32 >= criteria.min_cycles;
    list.push(CriterionStatus {
        name: "Minimum cycles completed".into(),
        met: cycles_met,
        detail: format!(
            "{} / {} required",
            report.total_submitted(),
            criteria.min_cycles
        ),
    });

    let soak_hours = report
        .finished_at
        .map(|end| (end - report.started_at).num_minutes() as f64 / 60.0)
        .unwrap_or(0.0);
    let soak_met = soak_hours >= criteria.min_soak_hours as f64;
    list.push(CriterionStatus {
        name: "Minimum soak duration".into(),
        met: soak_met,
        detail: format!("{soak_hours:.1}h / {}h required", criteria.min_soak_hours),
    });

    let acceptance_met = report.relay_acceptance_rate() >= criteria.min_relay_acceptance_rate;
    list.push(CriterionStatus {
        name: "Relay acceptance rate".into(),
        met: acceptance_met,
        detail: format!(
            "{:.1}% / {:.1}% required",
            report.relay_acceptance_rate() * 100.0,
            criteria.min_relay_acceptance_rate * 100.0
        ),
    });

    let divergence = report.mean_profit_divergence_wei();
    let divergence_met = match divergence {
        Some(d) => d <= criteria.max_mean_profit_divergence_wei,
        // No included bundles means no divergence data — that's a failure
        // to meet the criterion, not a pass by default.
        None => false,
    };
    list.push(CriterionStatus {
        name: "Sim-vs-real profit divergence within tolerance".into(),
        met: divergence_met,
        detail: match divergence {
            Some(d) => format!(
                "{d:.0} wei mean divergence / {:.0} wei max allowed",
                criteria.max_mean_profit_divergence_wei
            ),
            None => "no included bundles to measure divergence from".to_string(),
        },
    });

    let profit_met = report.total_realized_profit_wei() >= criteria.min_net_realized_profit_wei;
    list.push(CriterionStatus {
        name: "Net realized profit meets minimum".into(),
        met: profit_met,
        detail: format!(
            "{} wei realized / {} wei required",
            report.total_realized_profit_wei(),
            criteria.min_net_realized_profit_wei
        ),
    });

    list.push(CriterionStatus {
        name: "Kill switch manually tested during run".into(),
        met: attestations.kill_switch_tested_at.is_some(),
        detail: match (&attestations.kill_switch_tested_at, &attestations.kill_switch_tested_by) {
            (Some(t), Some(who)) => format!("tested {t} by {who}"),
            _ => "not yet attested".to_string(),
        },
    });

    list.push(CriterionStatus {
        name: "Circuit breaker configuration reviewed".into(),
        met: attestations.circuit_breaker_reviewed_at.is_some(),
        detail: match (
            &attestations.circuit_breaker_reviewed_at,
            &attestations.circuit_breaker_reviewed_by,
        ) {
            (Some(t), Some(who)) => format!("reviewed {t} by {who}"),
            _ => "not yet attested".to_string(),
        },
    });

    list.push(CriterionStatus {
        name: "Key custody architecture reviewed".into(),
        met: attestations.key_custody_reviewed_at.is_some(),
        detail: match (
            &attestations.key_custody_reviewed_at,
            &attestations.key_custody_reviewed_by,
        ) {
            (Some(t), Some(who)) => format!("reviewed {t} by {who}"),
            _ => "not yet attested".to_string(),
        },
    });

    list.push(CriterionStatus {
        name: "Multisig keyholders/threshold reviewed".into(),
        met: attestations.multisig_reviewed_at.is_some(),
        detail: match (&attestations.multisig_reviewed_at, &attestations.multisig_reviewed_by) {
            (Some(t), Some(who)) => format!("reviewed {t} by {who}"),
            _ => "not yet attested".to_string(),
        },
    });

    GateStatus { criteria: list }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::RelayOutcome;

    fn passing_report() -> TestnetReport {
        let mut r = TestnetReport::new("gate-test".into(), 11155111);
        r.started_at = Utc::now() - chrono::Duration::hours(80);
        for i in 0..600 {
            r.record(RelayOutcome {
                cycle_index: i,
                submitted_at: Utc::now(),
                relay_ack_latency_ms: Some(100),
                relay_accepted: true,
                included_onchain: true,
                blocks_to_inclusion: Some(1),
                expected_profit_wei: 1_000_000_000_000_000,
                realized_profit_wei: Some(990_000_000_000_000),
                rejection_reason: None,
            });
        }
        r.finished_at = Some(Utc::now());
        r
    }

    fn full_attestations() -> ManualAttestations {
        let now = Utc::now();
        ManualAttestations {
            kill_switch_tested_at: Some(now),
            kill_switch_tested_by: Some("alice".into()),
            circuit_breaker_reviewed_at: Some(now),
            circuit_breaker_reviewed_by: Some("bob".into()),
            key_custody_reviewed_at: Some(now),
            key_custody_reviewed_by: Some("alice".into()),
            multisig_reviewed_at: Some(now),
            multisig_reviewed_by: Some("carol".into()),
        }
    }

    #[test]
    fn fully_met_report_passes_gate() {
        let status = evaluate(&passing_report(), &GateCriteria::default(), &full_attestations());
        assert!(status.all_met(), "{:#?}", status);
    }

    #[test]
    fn missing_attestation_fails_gate_even_with_good_numbers() {
        let status = evaluate(
            &passing_report(),
            &GateCriteria::default(),
            &ManualAttestations::default(),
        );
        assert!(!status.all_met());
    }

    #[test]
    fn insufficient_cycles_fails_gate() {
        let mut r = passing_report();
        r.outcomes.truncate(10);
        let status = evaluate(&r, &GateCriteria::default(), &full_attestations());
        assert!(!status.all_met());
        let cycles = status.criteria.iter().find(|c| c.name.contains("cycles")).unwrap();
        assert!(!cycles.met);
    }

    #[test]
    fn no_included_bundles_fails_divergence_criterion_not_pass_by_default() {
        let mut r = TestnetReport::new("empty".into(), 11155111);
        r.started_at = Utc::now() - chrono::Duration::hours(80);
        r.finished_at = Some(Utc::now());
        let status = evaluate(&r, &GateCriteria::default(), &full_attestations());
        let divergence = status
            .criteria
            .iter()
            .find(|c| c.name.contains("divergence"))
            .unwrap();
        assert!(!divergence.met);
    }

    #[test]
    fn markdown_renders_checkboxes() {
        let status = evaluate(&passing_report(), &GateCriteria::default(), &full_attestations());
        let md = status.to_markdown();
        assert!(md.contains("GATE PASSED"));
        assert!(md.contains("- [x]"));
    }
}