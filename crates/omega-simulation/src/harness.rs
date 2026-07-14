// omega-engine\crates\omega-simulation\src\harness.rs
//! Ties together `ForkHandle`, `SimulationSubmitter`, a caller-supplied
//! `OpportunityDetector`, and `SimulationReport` into one run loop.
//!
//! Deliberately does NOT know how to construct a live submitter. The only
//! `BundleSubmitter` it ever builds is `SimulationSubmitter::bound_to`,
//! which is itself only constructible from a `ForkHandle`.
//!
//! Emits a heartbeat once per cycle via `omega_risk::heartbeat`, so a
//! stalled or crashed harness process is distinguishable from a harness
//! that's simply running through cycles with no opportunities — the same
//! distinction `ComponentHeartbeatStale` draws against
//! `NoBlueprintsPassingChecks` in ops/alerts/omega-risk.yaml. The
//! component name used is `"omega-simulation-harness"`; callers running
//! multiple harnesses concurrently in one process should construct each
//! with a distinct heartbeat component name (see `HarnessConfig::heartbeat_component`)
//! so their liveness signals don't collide under the same label.

use crate::error::Result;
use crate::fork::{ForkConfig, ForkHandle};
use crate::report::{CycleResult, SimulationReport};
use crate::submitter::SimulationSubmitter;
use crate::traits::{Bundle, BundleSubmitter, OpportunityDetector};
use ethers::providers::{Http, Middleware, Provider};
use omega_risk::heartbeat::{HeartbeatConfig, HeartbeatRegistry};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Default component name used for the harness's own heartbeat when
/// `HarnessConfig::heartbeat_component` is left as `None`.
pub const DEFAULT_HEARTBEAT_COMPONENT: &str = "omega-simulation-harness";

#[derive(Debug, Clone)]
pub struct HarnessConfig {
    pub fork: ForkConfig,
    /// How many detection→execution cycles to run before stopping.
    pub cycles: u32,
    /// Which dev account (index into the fork's funded accounts) executes
    /// transactions.
    pub signer_index: u32,
    /// Optional path to write the final `SimulationReport` as JSON.
    pub report_output_path: Option<PathBuf>,
    /// Heartbeat component name to beat under. Defaults to
    /// `DEFAULT_HEARTBEAT_COMPONENT` if `None` — override this when
    /// running more than one harness concurrently in the same process so
    /// their heartbeats don't share a label.
    pub heartbeat_component: Option<String>,
    /// How long the harness is allowed to go without completing a cycle
    /// before its heartbeat is considered stale. A single cycle includes
    /// fork block-mining, opportunity detection, and bundle submission —
    /// this should comfortably exceed the slowest expected cycle, not the
    /// average one, to avoid false alarms on a fork that's briefly slow
    /// to respond rather than actually stuck.
    pub heartbeat_max_silence: Duration,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            fork: ForkConfig {
                upstream_rpc_url: String::new(),
                fork_block_number: None,
                port: 0,
                dev_accounts: 5,
                startup_timeout: Duration::from_secs(30),
            },
            cycles: 20,
            signer_index: 0,
            report_output_path: None,
            heartbeat_component: None,
            heartbeat_max_silence: Duration::from_secs(120),
        }
    }
}

pub struct SimulationHarness {
    fork: ForkHandle,
    submitter: SimulationSubmitter,
    cfg: HarnessConfig,
    heartbeat: Arc<HeartbeatRegistry>,
    heartbeat_component: String,
}

impl SimulationHarness {
    /// Spawns the fork and binds a submitter to it. This is the only
    /// initialization path — there's no way to hand this harness a live
    /// relay client instead.
    ///
    /// Uses a fresh, harness-owned `HeartbeatRegistry`. Use
    /// `start_with_heartbeat` instead when the caller wants to share one
    /// registry across multiple components (e.g. a harness plus a
    /// separately-run relay-submission loop) so a single `/healthz`
    /// endpoint or dashboard can see all of them together.
    pub async fn start(cfg: HarnessConfig) -> Result<Self> {
        Self::start_with_heartbeat(cfg, Arc::new(HeartbeatRegistry::new())).await
    }

    /// Same as `start`, but binds to a caller-supplied heartbeat registry
    /// instead of creating a private one.
    pub async fn start_with_heartbeat(
        cfg: HarnessConfig,
        heartbeat: Arc<HeartbeatRegistry>,
    ) -> Result<Self> {
        let fork = ForkHandle::spawn(cfg.fork.clone()).await?;
        let submitter = SimulationSubmitter::bound_to(&fork, cfg.signer_index).await?;

        let heartbeat_component = cfg
            .heartbeat_component
            .clone()
            .unwrap_or_else(|| DEFAULT_HEARTBEAT_COMPONENT.to_string());

        heartbeat.register(
            &heartbeat_component,
            HeartbeatConfig { max_silence: cfg.heartbeat_max_silence },
        );

        Ok(Self { fork, submitter, cfg, heartbeat, heartbeat_component })
    }

    pub fn fork_endpoint(&self) -> &str {
        self.fork.endpoint()
    }

    /// Exposes the fork's provider so callers (e.g. the CLI) can read live
    /// chain state — such as current base fee — instead of hardcoding gas
    /// parameters.
    pub fn fork_provider(&self) -> Arc<Provider<Http>> {
        self.fork.provider()
    }

    /// Exposes the heartbeat registry so callers can inspect liveness
    /// (e.g. from a `/healthz` HTTP handler) without needing their own
    /// separate handle into it.
    pub fn heartbeat_registry(&self) -> Arc<HeartbeatRegistry> {
        self.heartbeat.clone()
    }

    /// Runs `cfg.cycles` iterations: pull opportunities for the current
    /// fork block, convert each into a bundle, submit it to the fork, and
    /// record the outcome. Nothing here touches real capital because
    /// `self.submitter` can only ever be a `SimulationSubmitter`.
    ///
    /// Beats the harness's heartbeat component once per cycle, right
    /// after the cycle's work completes (opportunity detection +
    /// submission), so a hang inside detector or submission logic shows
    /// up as a stale heartbeat rather than being masked by a beat that
    /// fired before the hang occurred.
    pub async fn run<D: OpportunityDetector>(
        &self,
        mut detector: D,
        to_bundle: impl Fn(&crate::traits::Opportunity) -> Bundle,
    ) -> Result<SimulationReport> {
        let mut report = SimulationReport::new(self.fork.endpoint().to_string());

        for cycle_index in 0..self.cfg.cycles {
            // Advance one block between cycles (skip before the first,
            // which runs against the fork's starting state). Without this,
            // cycles with no opportunity never move the chain forward —
            // Anvil only auto-mines in response to a submitted transaction
            // — so a run with many empty cycles would otherwise re-sample
            // the same block state repeatedly.
            if cycle_index > 0 {
                self.fork.mine_block().await?;
            }

            let block_number = self.fork.provider().get_block_number().await?.as_u64();
            let opportunities = detector.next_opportunities(block_number).await?;

            if opportunities.is_empty() {
                report.record(CycleResult::empty(cycle_index, block_number));
                self.heartbeat.beat(&self.heartbeat_component);
                continue;
            }

            for opportunity in opportunities {
                let bundle = to_bundle(&opportunity);
                let expected_profit = opportunity.expected_profit_wei;

                match self.submitter.submit(bundle).await {
                    Ok(receipt) => {
                        report.record(CycleResult::executed(
                            cycle_index,
                            block_number,
                            opportunity,
                            expected_profit,
                            receipt,
                        ));
                    }
                    Err(e) => {
                        report.record(CycleResult::failed(
                            cycle_index,
                            block_number,
                            opportunity,
                            e.to_string(),
                        ));
                    }
                }
            }

            self.heartbeat.beat(&self.heartbeat_component);
        }

        if let Some(path) = &self.cfg.report_output_path {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            report.write_json(path)?;
        }

        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_heartbeat_component_name_is_stable() {
        // Regression guard: this name is what ops/alerts/omega-risk.yaml's
        // ComponentHeartbeatStale rule will show up as in the `component`
        // label — changing it silently would desync dashboards/runbooks
        // from the actual metric.
        assert_eq!(DEFAULT_HEARTBEAT_COMPONENT, "omega-simulation-harness");
    }

    #[test]
    fn default_config_has_reasonable_heartbeat_tolerance() {
        let cfg = HarnessConfig::default();
        assert_eq!(cfg.heartbeat_max_silence, Duration::from_secs(120));
        assert!(cfg.heartbeat_component.is_none());
    }
}