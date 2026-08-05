// crates/omega-hot-path/src/lib.rs
//
// omega-hot-path — <1ms Microtx execution lane for SA and LA hot-tier (§4, §11.1).
//
// ## Spec §4 — hot-path constraints
//
//   Only two strategy configurations qualify for the hot path:
//     1. SA (Simple Arbitrage) — Microtx lane, gas < 200,000.
//     2. LA hot-tier — HF < 1.01 (§11.1), Microtx lane.
//   Canary (CNRY) blueprints MUST never enter the hot path.
//
//   Target latency: < 1ms per blueprint (CPU-pinned Tokio task).
//   Max concurrent slots: 4 (HOT_PATH_SLOTS).
//   Max RPC reads per blueprint: 8 (MICROTX_MAX_READS).
//   Simulator: revm in-process (zero-copy, no Anvil fork).
//
// ## Architectural role (§22.1)
//
//   omega-hot-path ← omega-core, omega-risk
//
//   The dependency on omega-risk is new as of this revision — see the
//   "oracle freshness / price sanity / slippage" audit note below for
//   why. Before this revision this crate depended only on omega-core.
//
// ## API alignment notes
//
//   All call sites in this file match the actual signatures in their
//   respective modules:
//
//   gate.rs:
//     HotPathGate::new() — takes 0 args (HOT_PATH_SLOTS is compiled in)
//     AdmissionResult — variants NOT including "Rejected { code }";
//       lib.rs uses a catch-all pattern for the non-Admitted branch.
//
//   simulator.rs:
//     MicrotxSimulator::simulate(&self, bp, read_budget, oracle, metrics)
//       — 4 args as of this revision (previously 3; `oracle` is new —
//       see the audit note below).
//     SimulationError variants: WrongSimulator, GasLimitExceeded,
//       Expired, OracleStale, OracleDiverged, PriceSanityViolation,
//       SlippageExceeded, Unprofitable, ReadBudgetExhausted (the four
//       oracle/slippage variants are new as of this revision).
//     HotPathSimResult::inner.profit_net — access via .inner field
//
//   metrics.rs:
//     record_success(latency_us: u64, profit_net: U256) — 2 args (no strategy_id)
//     record_miss() — used for all failure/rejection cases (no record_failure/record_rejection)
//
// ## Audit fix (this revision): lint escalation split
//
// Added `#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]`,
// mirroring the same fix in omega-risk/src/lib.rs. Cargo.toml's
// `[lints.clippy]` table sets unwrap_used/expect_used to "warn"
// crate-wide (see that file's own audit note); a manifest-level table
// can't express "deny outside tests, warn inside them" on its own, so
// that split is expressed here instead. Verified before adding: this
// crate's non-test code (`HotPathRunner::run` and everything it calls)
// contains no `.unwrap()`/`.expect()` calls — every `.unwrap()`/
// `.expect()` in this file lives inside `#[cfg(test)] mod tests` — so
// the deny should apply cleanly with nothing to fix first. The
// `unreachable!()` inside the `other` match arm below is unaffected by
// either `clippy::panic` (which targets `panic!()` specifically, not
// `unreachable!()`) or this new attribute (which only covers
// unwrap_used/expect_used) — it's a distinct, deliberate invariant, not
// an oversight covered by this fix.
//
// ## Audit fix (this revision): oracle freshness, price sanity, and
// slippage protection were entirely absent from this lane
//
// Before this revision, nothing in this crate ever read oracle data or
// `bp.slippage_bps`. A blueprint could reach `Ok(HotPathSimResult)` on
// this lane with a stale oracle, a non-sane or wildly-diverged price, or
// a slippage tolerance the system never configured — independent of
// `omega-execution::ExecutionPipeline`'s 16-check pipeline, which this
// lane structurally never goes through.
//
// Fixed with two changes:
//   1. `HotPathRequest` now carries a mandatory `oracle:
//      omega_risk::context::OracleSnapshot` field — the caller
//      constructing a request must assemble a live snapshot, the same
//      requirement `ExecutionPipeline::execute` already places on its
//      `CheckContext` parameter. There is no default, and none is
//      offered: a missing snapshot must fail the freshness check, not
//      silently skip it.
//   2. `MicrotxSimulator::simulate` (see simulator.rs's own audit note)
//      now runs four checks — freshness, hierarchy, price sanity,
//      slippage — before it can report success. `run()` below maps each
//      new failure mode to the SAME `DropCode` the 16-check pipeline
//      would produce for the equivalent condition (`MissOracle`,
//      `MissOracleDiverge`, `MissFlashCrash`, `MissSlippage`), so
//      loss-attribution telemetry reads identically regardless of which
//      execution route caught the problem.
//
// `HotPathRequest` gaining a required field, and `simulate()` gaining a
// required parameter, are both breaking API changes — the same category
// of breaking-but-correct fix already established elsewhere in this
// codebase: the alternative (an optional field defaulting to "skip these
// checks") would silently reintroduce the exact bypass this fix exists
// to close.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod gate;
pub mod metrics;
pub mod simulator;

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, oneshot};

use omega_core::errors::{DropCode, OmegaError};
use omega_core::types::blueprint::ExecutionBlueprint;
use omega_risk::context::OracleSnapshot;

// ── Re-exports (pub use only — no duplicate private use crate:: imports) ──────

pub use gate::{
    AdmissionResult, HotPathGate, HOT_PATH_SLOTS, MICROTX_GAS_LIMIT, MICROTX_MAX_READS,
};
pub use metrics::{HotPathMetrics, HotPathMetricsSnapshot};
pub use simulator::{HotPathSimResult, MicrotxSimulator, SimulationError};

// ─────────────────────────────────────────────────────────────────────────────
// HotPathRequest / HotPathResponse
// ─────────────────────────────────────────────────────────────────────────────

pub struct HotPathRequest {
    pub blueprint: ExecutionBlueprint,
    /// Live oracle snapshot for the blueprint's asset, assembled fresh by
    /// the caller at request time. See this file's module-level audit
    /// note ("oracle freshness, price sanity, and slippage protection")
    /// for why this field is mandatory rather than optional/defaulted —
    /// a missing or stale snapshot must fail `MicrotxSimulator::
    /// simulate`'s freshness check, not silently bypass it.
    pub oracle: OracleSnapshot,
    pub resp_tx: oneshot::Sender<HotPathResponse>,
}

#[derive(Debug)]
pub struct HotPathResponse {
    pub result: Result<HotPathSimResult, OmegaError>,
    pub elapsed_us: u64,
}

// ─────────────────────────────────────────────────────────────────────────────
// HotPathRunner
// ─────────────────────────────────────────────────────────────────────────────

pub struct HotPathRunner {
    gate: Arc<HotPathGate>,
    simulator: Arc<MicrotxSimulator>,
    metrics: Arc<HotPathMetrics>,
    rx: mpsc::Receiver<HotPathRequest>,
}

#[derive(Debug, Clone)]
pub struct HotPathConfig {
    pub channel_capacity: usize,
    pub revm_trust_window_blocks: u64,
}

impl Default for HotPathConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 64,
            revm_trust_window_blocks: 1,
        }
    }
}

impl HotPathRunner {
    pub fn new(config: HotPathConfig) -> (Self, mpsc::Sender<HotPathRequest>) {
        let (tx, rx) = mpsc::channel(config.channel_capacity);

        // HotPathGate::new() takes 0 arguments — HOT_PATH_SLOTS is a compile-time
        // constant embedded in gate.rs, not a runtime parameter.
        let gate = Arc::new(HotPathGate::new());
        let simulator = Arc::new(MicrotxSimulator::new(config.revm_trust_window_blocks));
        let metrics = Arc::new(HotPathMetrics::new());

        let runner = Self {
            gate,
            simulator,
            metrics,
            rx,
        };
        (runner, tx)
    }

    pub fn metrics(&self) -> Arc<HotPathMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Run the hot-path event loop.
    ///
    /// Must be spawned as a dedicated Tokio task pinned to a CPU core (§4).
    pub async fn run(mut self) {
        tracing::info!(slots = HOT_PATH_SLOTS, "HotPathRunner started");

        while let Some(req) = self.rx.recv().await {
            let start = Instant::now();

            let admission = self.gate.admit(&req.blueprint);

            let result: Result<HotPathSimResult, OmegaError> = match admission {
                AdmissionResult::Admitted { read_budget } => {
                    // simulate() takes 4 args as of this revision: blueprint,
                    // read_budget, oracle, metrics — see this file's and
                    // simulator.rs's audit notes.
                    let sim_result = self.simulator.simulate(
                        &req.blueprint,
                        read_budget,
                        &req.oracle,
                        &self.metrics,
                    );

                    // Release slot always, regardless of simulation outcome
                    self.gate.release();

                    match sim_result {
                        Ok(sim) => {
                            // record_success takes (latency_us, profit_net) — no strategy_id
                            // profit_net is on sim.inner, not sim directly
                            self.metrics.record_success(
                                start.elapsed().as_micros() as u64,
                                sim.inner.profit_net,
                            );
                            Ok(sim)
                        }

                        // Map actual SimulationError variants → DropCode.
                        // (NOT Reverted / StaleCache / GasMiscalc / BudgetExceeded)
                        Err(SimulationError::WrongSimulator { .. }) => {
                            self.metrics.record_miss();
                            Err(OmegaError::dropped(DropCode::SimulationExecutionRevert))
                        }
                        Err(SimulationError::GasLimitExceeded { .. }) => {
                            self.metrics.record_miss();
                            Err(OmegaError::dropped(DropCode::SimulationGasMiscalc))
                        }
                        Err(SimulationError::Expired { .. }) => {
                            self.metrics.record_miss();
                            Err(OmegaError::dropped(DropCode::SimulationStateMismatch))
                        }

                        // New as of this revision — oracle/price/slippage
                        // failures map to the SAME DropCode the 16-check
                        // pipeline (omega_risk::checks::run_all_checks)
                        // would produce for the equivalent condition, not
                        // to a hot-path-specific Simulation* code, so
                        // loss-attribution telemetry is identical
                        // regardless of which execution route caught it.
                        Err(SimulationError::OracleStale) => {
                            self.metrics.record_miss();
                            Err(OmegaError::dropped(DropCode::MissOracle))
                        }
                        Err(SimulationError::OracleDiverged) => {
                            self.metrics.record_miss();
                            Err(OmegaError::dropped(DropCode::MissOracleDiverge))
                        }
                        Err(SimulationError::PriceSanityViolation) => {
                            self.metrics.record_miss();
                            Err(OmegaError::dropped(DropCode::MissFlashCrash))
                        }
                        Err(SimulationError::SlippageExceeded { .. }) => {
                            self.metrics.record_miss();
                            Err(OmegaError::dropped(DropCode::MissSlippage))
                        }

                        Err(SimulationError::Unprofitable { .. }) => {
                            self.metrics.record_miss();
                            Err(OmegaError::dropped(DropCode::SimulationGasMiscalc))
                        }
                        Err(SimulationError::ReadBudgetExhausted { .. }) => {
                            self.metrics.record_miss();
                            Err(OmegaError::dropped(DropCode::SimulationGasMiscalc))
                        }
                    }
                }

                // Catch-all for any non-Admitted variant.
                // AdmissionResult does not have a "Rejected { code }" struct variant;
                // use a wildcard and record the miss.
                other => {
                    tracing::debug!(
                        blueprint_hash = %req.blueprint.blueprint_hash,
                        "Hot-path admission rejected",
                    );
                    // Extract a drop code if the variant carries one, otherwise
                    // default to MissCapacity (slot full / strategy ineligible).
                    let drop_code = match &other {
                        AdmissionResult::Admitted { .. } => unreachable!(),
                        _ => DropCode::MissCapacity,
                    };
                    // record_miss() is the only rejection recorder on HotPathMetrics
                    self.metrics.record_miss();
                    Err(OmegaError::dropped(drop_code))
                }
            };

            let elapsed_us = start.elapsed().as_micros() as u64;

            if elapsed_us > 1_000 {
                tracing::warn!(
                    blueprint_hash = %req.blueprint.blueprint_hash,
                    elapsed_us,
                    sla_us = 1_000,
                    "Hot-path SLA breach: exceeded 1ms target",
                );
            }

            let _ = req.resp_tx.send(HotPathResponse { result, elapsed_us });
        }

        tracing::info!("HotPathRunner stopped — channel closed");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, Bytes, B256, U256};
    use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
    use omega_core::types::lane::{Lane, Simulator};
    use uuid::Uuid;

    fn make_bp(strategy: StrategyId, gas: u64, hash_byte: u8) -> ExecutionBlueprint {
        let mut hash = B256::ZERO;
        hash.0[0] = hash_byte;
        // signal_id/client_order_id/idempotency_key: these hot-path gate
        // tests exercise admission/simulation logic only, never
        // verify_hash()/verify_idempotency_key() — same placeholder
        // caveat as omega-dag's test helper.
        let signal_id = Uuid::from_bytes([hash_byte; 16]);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(strategy, 42161, 0, signal_id);
        ExecutionBlueprint {
            blueprint_hash: hash,
            chain_id: 42161,
            strategy_id: strategy,
            lane: Lane::Microtx,
            simulator: Simulator::Revm,
            signal_state_hash: B256::ZERO,
            state_version: 1,
            signal_id,
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::ZERO,
            flashloan_available: U256::ZERO,
            calldata: Bytes::default(),
            strategy_bytecode_hash: B256::ZERO,
            l2_exec_gas_estimate: gas,
            l1_data_gas_estimate: 0,
            extraction_gas: 21_000,
            expected_profit_net: U256::from(2_000_000_000_000_000_u128),
            dynamic_min_profit: U256::from(100_000_u64),
            l2_buffer_factor: 1.15,
            l1_data_buffer_factor: 1.10,
            // 20 bps — comfortably under MAX_SLIPPAGE_BPS_SA (30), the
            // tightest per-strategy cap any blueprint built by this
            // helper could be checked against. The previous hardcoded
            // 100 predates slippage actually being enforced on this
            // lane (see this file's audit note) and would now fail
            // every test that expects success.
            slippage_bps: 20,
            base_fee_at_creation: 10,
            l1_data_fee_at_creation: 2,
            priority_fee_gwei: 10,
            price_impact_bps: None,
            ofa_compliant: false,
            expiry_block: 1_001,
            nonce: 0,
            confirmation_depth: 12,
            client_order_id,
            idempotency_key: B256::ZERO,
            relay_targets: vec!["relay_a".into()],
            zk_proof_commitment: None,
        }
    }

    /// A live, sane, mutually-consistent oracle snapshot — every check
    /// this crate's simulator runs should pass against this.
    fn passing_oracle() -> OracleSnapshot {
        OracleSnapshot {
            chainlink_price: 2000.0,
            pyth_price: 2001.0,
            twap_price: 1999.0,
            chainlink_age_s: 10,
            pyth_age_s: 10,
            twap_age_s: 60,
        }
    }

    /// All three feeds stale.
    fn stale_oracle() -> OracleSnapshot {
        OracleSnapshot {
            chainlink_price: 2000.0,
            pyth_price: 2001.0,
            twap_price: 1999.0,
            chainlink_age_s: 100,
            pyth_age_s: 100,
            twap_age_s: 200,
        }
    }

    #[tokio::test]
    async fn runner_processes_eligible_sa_blueprint() {
        let (runner, tx) = HotPathRunner::new(HotPathConfig::default());
        tokio::spawn(runner.run());

        let bp = make_bp(StrategyId::Sa, 100_000, 1);
        let (resp_tx, resp_rx) = oneshot::channel();
        tx.try_send(HotPathRequest {
            blueprint: bp,
            oracle: passing_oracle(),
            resp_tx,
        })
        .unwrap();

        let resp = tokio::time::timeout(std::time::Duration::from_millis(100), resp_rx)
            .await
            .expect("timeout")
            .expect("channel closed");

        assert!(
            resp.result.is_ok(),
            "SA blueprint with valid gas should succeed: {:?}",
            resp.result
        );
    }

    #[tokio::test]
    async fn runner_rejects_cnry_blueprint() {
        let (runner, tx) = HotPathRunner::new(HotPathConfig::default());
        tokio::spawn(runner.run());

        let bp = make_bp(StrategyId::Cnry, 50_000, 2);
        let (resp_tx, resp_rx) = oneshot::channel();
        tx.try_send(HotPathRequest {
            blueprint: bp,
            oracle: passing_oracle(),
            resp_tx,
        })
        .unwrap();

        let resp = tokio::time::timeout(std::time::Duration::from_millis(100), resp_rx)
            .await
            .expect("timeout")
            .expect("channel closed");

        assert!(
            resp.result.is_err(),
            "CNRY must be rejected at hot-path gate"
        );
    }

    #[tokio::test]
    async fn runner_rejects_msa_blueprint() {
        let (runner, tx) = HotPathRunner::new(HotPathConfig::default());
        tokio::spawn(runner.run());

        let bp = make_bp(StrategyId::Msa, 100_000, 3);
        let (resp_tx, resp_rx) = oneshot::channel();
        tx.try_send(HotPathRequest {
            blueprint: bp,
            oracle: passing_oracle(),
            resp_tx,
        })
        .unwrap();

        let resp = tokio::time::timeout(std::time::Duration::from_millis(100), resp_rx)
            .await
            .expect("timeout")
            .expect("channel closed");

        assert!(
            resp.result.is_err(),
            "MSA (not hot_path_eligible) must be rejected"
        );
    }

    #[tokio::test]
    async fn runner_rejects_gas_over_limit() {
        let (runner, tx) = HotPathRunner::new(HotPathConfig::default());
        tokio::spawn(runner.run());

        let bp = make_bp(StrategyId::Sa, MICROTX_GAS_LIMIT + 1, 4);
        let (resp_tx, resp_rx) = oneshot::channel();
        tx.try_send(HotPathRequest {
            blueprint: bp,
            oracle: passing_oracle(),
            resp_tx,
        })
        .unwrap();

        let resp = tokio::time::timeout(std::time::Duration::from_millis(100), resp_rx)
            .await
            .expect("timeout")
            .expect("channel closed");

        assert!(
            resp.result.is_err(),
            "SA with gas > MICROTX_GAS_LIMIT must be rejected"
        );
    }

    // ── Oracle freshness / price sanity / slippage (this revision) ──────

    #[tokio::test]
    async fn runner_rejects_stale_oracle() {
        let (runner, tx) = HotPathRunner::new(HotPathConfig::default());
        tokio::spawn(runner.run());

        let bp = make_bp(StrategyId::Sa, 100_000, 5);
        let (resp_tx, resp_rx) = oneshot::channel();
        tx.try_send(HotPathRequest {
            blueprint: bp,
            oracle: stale_oracle(),
            resp_tx,
        })
        .unwrap();

        let resp = tokio::time::timeout(std::time::Duration::from_millis(100), resp_rx)
            .await
            .expect("timeout")
            .expect("channel closed");

        match resp.result {
            Err(e) => assert_eq!(e.drop_code(), Some(DropCode::MissOracle)),
            Ok(_) => panic!("stale oracle must be rejected, not silently succeed"),
        }
    }

    #[tokio::test]
    async fn runner_rejects_price_sanity_violation() {
        let (runner, tx) = HotPathRunner::new(HotPathConfig::default());
        tokio::spawn(runner.run());

        let mut oracle = passing_oracle();
        oracle.chainlink_price = 2000.0;
        oracle.pyth_price = 2000.0; // agrees with chainlink — check 8 passes
        oracle.twap_price = 1.0; // wildly diverged — flash-crash guard must catch it

        let bp = make_bp(StrategyId::Sa, 100_000, 6);
        let (resp_tx, resp_rx) = oneshot::channel();
        tx.try_send(HotPathRequest {
            blueprint: bp,
            oracle,
            resp_tx,
        })
        .unwrap();

        let resp = tokio::time::timeout(std::time::Duration::from_millis(100), resp_rx)
            .await
            .expect("timeout")
            .expect("channel closed");

        match resp.result {
            Err(e) => assert_eq!(e.drop_code(), Some(DropCode::MissFlashCrash)),
            Ok(_) => panic!("flash-crash-diverged price must be rejected, not silently succeed"),
        }
    }

    #[tokio::test]
    async fn runner_rejects_slippage_exceeded() {
        let (runner, tx) = HotPathRunner::new(HotPathConfig::default());
        tokio::spawn(runner.run());

        let mut bp = make_bp(StrategyId::Sa, 100_000, 7);
        bp.slippage_bps = 200; // far above MAX_SLIPPAGE_BPS_SA (30)

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.try_send(HotPathRequest {
            blueprint: bp,
            oracle: passing_oracle(),
            resp_tx,
        })
        .unwrap();

        let resp = tokio::time::timeout(std::time::Duration::from_millis(100), resp_rx)
            .await
            .expect("timeout")
            .expect("channel closed");

        match resp.result {
            Err(e) => assert_eq!(e.drop_code(), Some(DropCode::MissSlippage)),
            Ok(_) => panic!("over-limit slippage must be rejected, not silently succeed"),
        }
    }

    #[tokio::test]
    async fn concurrent_burst_with_stale_oracle_never_succeeds() {
        // "Cannot be bypassed under concurrent execution" at the
        // HotPathRunner level: fire many requests at the single shared
        // runner before awaiting any individual response, and confirm
        // every single one is rejected. HotPathRunner's event loop is
        // sequential (a single task pulling from one mpsc channel), so
        // this also proves ordering/interleaving of sends doesn't create
        // any window where a stale-oracle request could slip past the
        // check.
        let (runner, tx) = HotPathRunner::new(HotPathConfig {
            channel_capacity: 128,
            ..HotPathConfig::default()
        });
        tokio::spawn(runner.run());

        let mut receivers = Vec::new();
        for i in 0u8..40 {
            let bp = make_bp(StrategyId::Sa, 100_000, i.wrapping_add(50));
            let (resp_tx, resp_rx) = oneshot::channel();
            tx.send(HotPathRequest {
                blueprint: bp,
                oracle: stale_oracle(),
                resp_tx,
            })
            .await
            .unwrap();
            receivers.push(resp_rx);
        }

        for resp_rx in receivers {
            let resp = tokio::time::timeout(std::time::Duration::from_millis(500), resp_rx)
                .await
                .expect("timeout")
                .expect("channel closed");
            match resp.result {
                Err(e) => assert_eq!(e.drop_code(), Some(DropCode::MissOracle)),
                Ok(_) => panic!("no request in a stale-oracle burst may succeed"),
            }
        }
    }

    #[tokio::test]
    async fn concurrent_burst_with_sane_oracle_all_succeed() {
        // Control test for the one above.
        let (runner, tx) = HotPathRunner::new(HotPathConfig {
            channel_capacity: 128,
            ..HotPathConfig::default()
        });
        tokio::spawn(runner.run());

        let mut receivers = Vec::new();
        for i in 0u8..40 {
            let bp = make_bp(StrategyId::Sa, 100_000, i.wrapping_add(90));
            let (resp_tx, resp_rx) = oneshot::channel();
            tx.send(HotPathRequest {
                blueprint: bp,
                oracle: passing_oracle(),
                resp_tx,
            })
            .await
            .unwrap();
            receivers.push(resp_rx);
        }

        for resp_rx in receivers {
            let resp = tokio::time::timeout(std::time::Duration::from_millis(500), resp_rx)
                .await
                .expect("timeout")
                .expect("channel closed");
            assert!(
                resp.result.is_ok(),
                "sane-oracle burst: every request must succeed"
            );
        }
    }

    #[test]
    fn default_config_sensible() {
        let cfg = HotPathConfig::default();
        assert!(cfg.channel_capacity > 0);
        assert!(cfg.revm_trust_window_blocks >= 1);
    }

    #[test]
    fn constants_exported() {
        // FIX: assertions_on_constants → move into const blocks
        const { assert!(HOT_PATH_SLOTS > 0) }
        const { assert!(MICROTX_GAS_LIMIT > 0) }
        const { assert!(MICROTX_MAX_READS > 0) }
    }
}
