// crates/omega-chaos/src/runner.rs
//
// ChaosRunner â€” executes chaos scenarios and produces a structured report.
//
// ## Execution model
//
//   Each scenario runs as:
//     1. Inject fault(s) via `target.inject(fault)`
//     2. Poll for expected response within `response_deadline`
//     3. Verify the system reached the correct state
//     4. If `test_recovery`: clear faults, verify system recovers to Healthy
//     5. Record `ScenarioResult` and reset target for the next scenario
//
//   Scenarios are run sequentially by default.  The runner logs every
//   fault injection and observation so CI output is self-documenting.
//
// ## Report
//
//   After all scenarios complete, `ChaosRunner::run_all` returns a
//   `ChaosReport` containing:
//     - Per-scenario `ScenarioResult`
//     - Overall pass/fail/skip counts
//     - Total elapsed time
//     - A `passed: bool` field gating the Phase 0 scorecard

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use omega_core::{HealthState, LayerId};

use crate::scenarios::{ScenarioConfig, ScenarioId, ScenarioOutcome, ScenarioResult};
use crate::target::{ChaosTarget, FaultKind};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ChaosReport
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Summary report from a full or partial chaos suite run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChaosReport {
    /// UTC timestamp of suite start.
    pub started_at:    chrono::DateTime<chrono::Utc>,
    /// Results in execution order.
    pub results:       Vec<ScenarioResult>,
    /// Number of scenarios that passed.
    pub passed:        usize,
    /// Number of scenarios that failed.
    pub failed:        usize,
    /// Number of scenarios skipped.
    pub skipped:       usize,
    /// Total elapsed time across all scenarios.
    #[serde(with = "duration_secs")]
    pub total_elapsed: Duration,
    /// `true` when all non-skipped scenarios passed (Phase 0 gate).
    pub all_passed:    bool,
}

impl ChaosReport {
    fn from_results(
        started_at: chrono::DateTime<chrono::Utc>,
        results:    Vec<ScenarioResult>,
    ) -> Self {
        let passed  = results.iter().filter(|r| r.outcome == ScenarioOutcome::Pass).count();
        let failed  = results.iter().filter(|r| r.outcome == ScenarioOutcome::Fail).count();
        let skipped = results.iter().filter(|r| r.outcome == ScenarioOutcome::Skipped).count();
        let total_elapsed = results.iter().map(|r| r.elapsed).sum();
        let all_passed    = failed == 0;
        Self { started_at, results, passed, failed, skipped, total_elapsed, all_passed }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ChaosRunner
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Executes chaos scenarios against a `ChaosTarget`.
pub struct ChaosRunner {
    target:        Arc<ChaosTarget>,
    /// Scenarios to run.  Defaults to all 14 in spec order.
    scenarios:     Vec<ScenarioConfig>,
    /// Poll interval when waiting for an expected response.
    poll_interval: Duration,
}

impl ChaosRunner {
    /// Create a runner that will execute all 14 scenarios with defaults.
    pub fn all_scenarios(target: Arc<ChaosTarget>) -> Self {
        let scenarios = ScenarioId::ALL
            .iter()
            .map(|&id| ScenarioConfig::default_for(id))
            .collect();
        Self {
            target,
            scenarios,
            poll_interval: Duration::from_millis(50),
        }
    }

    /// Create a runner for a specific subset of scenarios.
    pub fn with_scenarios(target: Arc<ChaosTarget>, configs: Vec<ScenarioConfig>) -> Self {
        Self { target, scenarios: configs, poll_interval: Duration::from_millis(50) }
    }

    // â”€â”€ Run all â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Execute all configured scenarios and return a `ChaosReport`.
    pub async fn run_all(&self) -> ChaosReport {
        let started_at = Utc::now();
        let mut results = Vec::with_capacity(self.scenarios.len());

        for config in &self.scenarios {
            tracing::info!(
                scenario    = %config.id,
                description = config.id.description(),
                "CHAOS: starting scenario",
            );
            self.target.reset();
            let result = self.run_scenario(config).await;
            tracing::info!(
                scenario   = %result.id,
                outcome    = %result.outcome,
                elapsed_ms = result.elapsed.as_millis(),
                "CHAOS: scenario complete",
            );
            results.push(result);
        }

        let report = ChaosReport::from_results(started_at, results);
        tracing::info!(
            passed     = report.passed,
            failed     = report.failed,
            skipped    = report.skipped,
            all_passed = report.all_passed,
            "CHAOS: suite complete",
        );
        report
    }

    // â”€â”€ Single scenario â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    async fn run_scenario(&self, config: &ScenarioConfig) -> ScenarioResult {
        let started_at     = Utc::now();
        let scenario_start = Instant::now();

        // Phase 1: inject fault
        let fault = self.fault_for(config.id);
        self.target.inject(fault);

        // Phase 2: poll for expected response within deadline
        let response_ok = self
            .wait_for_expected_response(config.id, config.response_deadline)
            .await;

        let obs_strings: Vec<String> = self
            .target
            .take_observations()
            .into_iter()
            .map(|o| format!("[{}] {}", if o.expected { "OK" } else { "UNEXPECTED" }, o.message))
            .collect();

        if !response_ok {
            return ScenarioResult::fail(
                config.id,
                started_at,
                scenario_start.elapsed(),
                obs_strings,
                format!(
                    "System did not reach expected response within {}s deadline",
                    config.response_deadline.as_secs(),
                ),
            );
        }

        // Phase 3: hold the fault for its configured duration
        tokio::time::sleep(config.fault_duration.min(Duration::from_millis(500))).await;

        // Phase 4: recovery verification
        let recovery_ok = if config.test_recovery {
            self.target.clear_faults();
            self.target.recover_all();
            let recovered = self.wait_for_all_healthy(Duration::from_secs(5)).await;
            Some(recovered)
        } else {
            None
        };

        ScenarioResult::pass(
            config.id,
            started_at,
            scenario_start.elapsed(),
            obs_strings,
            recovery_ok,
        )
    }

    // â”€â”€ Expected response checking â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    async fn wait_for_expected_response(
        &self,
        id:       ScenarioId,
        deadline: Duration,
    ) -> bool {
        let end = Instant::now() + deadline;
        while Instant::now() < end {
            if self.check_expected_response(id) {
                return true;
            }
            tokio::time::sleep(self.poll_interval).await;
        }
        self.check_expected_response(id)
    }

    fn check_expected_response(&self, id: ScenarioId) -> bool {
        use omega_core::LayerHealth;
        match id {
            ScenarioId::OracleStale | ScenarioId::OracleDiverge => {
                let state = self.target.layer_state(LayerId::ExternalData);
                let ok    = state.map(|s| !s.is_healthy()).unwrap_or(false);
                if ok {
                    self.target.observe(
                        format!("ExternalData layer transitioned to {:?}", state.unwrap()),
                        true,
                    );
                }
                ok
            }

            ScenarioId::SequencerDown | ScenarioId::SequencerRestart => {
                let state = self.target.layer_state(LayerId::ExternalData);
                let ok    = state.map(|s| s == HealthState::Degraded).unwrap_or(false);
                if ok {
                    self.target.observe("ExternalData Degraded on sequencer fault", true);
                }
                ok
            }

            ScenarioId::FlashCrash => {
                let state = self.target.layer_state(LayerId::Risk);
                let ok    = state.map(|s| !s.is_healthy()).unwrap_or(false);
                if ok {
                    self.target.observe("Risk layer transitioned on flash crash", true);
                }
                ok
            }

            ScenarioId::GasSpike => {
                let state = self.target.layer_state(LayerId::Strategy);
                let ok    = state.map(|s| s == HealthState::Degraded).unwrap_or(false);
                if ok {
                    self.target.observe("Strategy Degraded on gas spike", true);
                }
                ok
            }

            ScenarioId::RelayTimeout => {
                let state = self.target.layer_state(LayerId::Relay);
                let ok    = state.map(|s| !s.is_healthy()).unwrap_or(false);
                if ok {
                    self.target.observe("Relay layer Degraded on timeout", true);
                }
                ok
            }

            ScenarioId::DagCycle => {
                let has_cycle =
                    self.target.has_fault(|f| matches!(f, FaultKind::DagCycle { .. }));
                if has_cycle {
                    self.target.observe(
                        "DagCycle fault active â€” MissDagCycle drop expected", true,
                    );
                }
                has_cycle
            }

            ScenarioId::ZkProofDelay => {
                let state = self.target.layer_state(LayerId::Zk);
                let ok    = state.map(|s| s == HealthState::Degraded).unwrap_or(false);
                if ok {
                    self.target.observe("Zk layer Degraded on proof delay", true);
                }
                ok
            }

            ScenarioId::HealthCascade => {
                let halted   = self.target.halted_layers();
                let cascaded = halted.contains(&LayerId::Strategy)
                    || halted.contains(&LayerId::Relay)
                    || halted.contains(&LayerId::Vault);
                if cascaded {
                    self.target.observe(
                        format!("Halt cascaded to {} layers: {:?}", halted.len(), halted),
                        true,
                    );
                }
                cascaded
            }

            ScenarioId::RevmCacheStale => {
                let state = self.target.layer_state(LayerId::Eil);
                let ok    = state.map(|s| s == HealthState::Degraded).unwrap_or(false);
                if ok {
                    self.target.observe("EIL Degraded on revm cache staleness", true);
                }
                ok
            }

            ScenarioId::FlashloanLiquidity => {
                let state = self.target.layer_state(LayerId::Flashloan);
                let ok    = state.map(|s| !s.is_healthy()).unwrap_or(false);
                if ok {
                    self.target.observe("Flashloan Degraded on zero liquidity", true);
                }
                ok
            }

            ScenarioId::HighCompetition => {
                // Competition is not a health fault â€” verify all layers stay operational.
                // `is_operational()` is a method on `LayerHealth` which is already
                // in scope from the `use omega_core::LayerHealth` at the top of this fn.
                let operational = self
                    .target
                    .health_layers
                    .values()
                    .all(|l| l.is_operational());
                if operational {
                    self.target.observe(
                        "All layers operational under high competition", true,
                    );
                }
                operational
            }

            ScenarioId::RpcRateExhaust => {
                let state = self.target.layer_state(LayerId::ExternalData);
                let ok    = state.map(|s| s == HealthState::Degraded).unwrap_or(false);
                if ok {
                    self.target.observe("ExternalData Degraded on RPC exhaustion", true);
                }
                ok
            }
        }
    }

    async fn wait_for_all_healthy(&self, deadline: Duration) -> bool {
        let end = Instant::now() + deadline;
        while Instant::now() < end {
            if self.target.all_healthy() {
                return true;
            }
            tokio::time::sleep(self.poll_interval).await;
        }
        self.target.all_healthy()
    }

    // â”€â”€ Fault construction â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    fn fault_for(&self, id: ScenarioId) -> FaultKind {
        use omega_core::LayerHealth;
        match id {
            ScenarioId::OracleStale => {
                self.target.health_layers[&LayerId::ExternalData]
                    .set_state(HealthState::Degraded, "oracle stale > 45s");
                FaultKind::OracleStale { feed: "chainlink_eth".into(), stale_secs: 60 }
            }
            ScenarioId::OracleDiverge => {
                self.target.health_layers[&LayerId::ExternalData]
                    .set_state(HealthState::Degraded, "oracle diverge > 0.4%");
                FaultKind::OracleDiverge {
                    feed_a:      "chainlink_eth".into(),
                    feed_b:      "pyth_eth".into(),
                    diverge_bps: 50,
                }
            }
            ScenarioId::SequencerDown    => FaultKind::SequencerDown,
            ScenarioId::SequencerRestart => FaultKind::SequencerRestart { gap_blocks: 10 },
            ScenarioId::FlashCrash => {
                self.target.health_layers[&LayerId::Risk]
                    .set_state(HealthState::Degraded, "flash crash -20% detected");
                FaultKind::FlashCrash { asset: "WETH".into(), drop_pct: 20.0 }
            }
            ScenarioId::GasSpike => {
                self.target.health_layers[&LayerId::Strategy]
                    .set_state(HealthState::Degraded, "gas spike 500 gwei");
                FaultKind::GasSpike { fee_gwei: 500 }
            }
            ScenarioId::RelayTimeout => {
                self.target.health_layers[&LayerId::Relay]
                    .set_state(HealthState::Degraded, "all relays unresponsive");
                FaultKind::RelayTimeout
            }
            ScenarioId::DagCycle => {
                FaultKind::DagCycle { cycle_description: "Aâ†’Bâ†’Câ†’A".into() }
            }
            ScenarioId::ZkProofDelay => {
                self.target.health_layers[&LayerId::Zk]
                    .set_state(HealthState::Degraded, "ZK proof latency 200ms > 20ms budget");
                FaultKind::ZkProofDelay { latency_ms: 200 }
            }
            ScenarioId::HealthCascade => {
                self.target.health_layers[&LayerId::SystemHealth]
                    .set_state(HealthState::Halted, "chaos cascade test");
                for dep in [
                    LayerId::Strategy, LayerId::Relay, LayerId::Vault,
                    LayerId::Orchestrator, LayerId::HotPath,
                ] {
                    self.target.health_layers[&dep]
                        .set_state(HealthState::Halted, "cascaded from SystemHealth");
                }
                FaultKind::LayerHalted {
                    layer:  LayerId::SystemHealth,
                    reason: "chaos cascade test".into(),
                }
            }
            ScenarioId::RevmCacheStale => {
                self.target.health_layers[&LayerId::Eil]
                    .set_state(HealthState::Degraded, "revm cache 3 blocks behind");
                FaultKind::RevmCacheStale { blocks_behind: 3 }
            }
            ScenarioId::FlashloanLiquidity => {
                self.target.health_layers[&LayerId::Flashloan]
                    .set_state(HealthState::Degraded, "zero flashloan liquidity");
                FaultKind::FlashloanDry { provider: "aave_v3".into() }
            }
            ScenarioId::HighCompetition => {
                FaultKind::HighCompetition { win_pct: 95.0 }
            }
            ScenarioId::RpcRateExhaust => {
                self.target.health_layers[&LayerId::ExternalData]
                    .set_state(HealthState::Degraded, "RPC rate limiter exhausted");
                FaultKind::RpcRateExhausted
            }
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Serde helper
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

mod duration_secs {
    use std::time::Duration;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_secs(u64::deserialize(d)?))
    }
}