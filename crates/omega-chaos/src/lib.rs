// crates/omega-chaos/src/lib.rs
//
// omega-chaos — 14-scenario chaos test harness for the Omega Engine (spec §9).
//
// ## Architectural role (§22.1)
//
//   omega-chaos ← all (test only)
//
//   This crate is a workspace member but is NEVER imported by any
//   production crate.  It depends on omega-core and omega-health so it
//   can inject real fault conditions and observe actual health FSM
//   transitions, not mocked ones.
//
// ## Module map
//
//   scenarios.rs — `ScenarioId`, `ScenarioResult`, `ScenarioConfig`, and
//                  the 14 pure scenario functions (S1–S14).  Each function
//                  receives a `&mut ChaosTarget` and returns `ScenarioResult`.
//
//   target.rs    — `ChaosTarget`: lightweight wrapper around real omega-health
//                  and omega-core types.  Provides fault injection primitives
//                  (inject_oracle_stale, inject_gas_spike, etc.) that manipulate
//                  actual `LayerHealthImpl` state machines.
//
//   runner.rs    — `ChaosRunner`: orchestrates scenario execution, parallelism,
//                  timing, and produces a `ChaosReport` with pass/fail status
//                  for each of the 14 scenarios.
//
// ## Usage
//
//   ```rust
//   use omega_chaos::{ChaosRunner, ChaosRunnerConfig};
//
//   #[tokio::test]
//   async fn all_14_chaos_scenarios() {
//       let cfg    = ChaosRunnerConfig::default();
//       let runner = ChaosRunner::new(cfg);
//       let report = runner.run_all().await.expect("chaos run");
//       assert!(report.all_pass(), "{}", report.summary());
//   }
//   ```

pub mod runner;
pub mod scenarios;
pub mod target;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use runner::{ChaosReport, ChaosRunner};
// ScenarioOutcome is defined in scenarios — import directly rather than
// re-exporting through runner (where it is only an internal import, not pub).
pub use scenarios::{ScenarioConfig, ScenarioId, ScenarioOutcome, ScenarioResult};
pub use target::{ChaosTarget, FaultKind, Observation};
