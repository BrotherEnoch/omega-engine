ï»¿// crates/omega-chaos/src/lib.rs
//
// omega-chaos â€” 14-scenario chaos test harness for the Omega Engine (spec Â§9).
//
// ## Architectural role (Â§22.1)
//
//   omega-chaos â† all (test only)
//
//   This crate is a workspace member but is NEVER imported by any
//   production crate.  It depends on omega-core and omega-health so it
//   can inject real fault conditions and observe actual health FSM
//   transitions, not mocked ones.
//
// ## Module map
//
//   scenarios.rs â€” `ScenarioId`, `ScenarioResult`, `ScenarioConfig`, and
//                  the 14 pure scenario functions (S1â€“S14).  Each function
//                  receives a `&mut ChaosTarget` and returns `ScenarioResult`.
//
//   target.rs    â€” `ChaosTarget`: lightweight wrapper around real omega-health
//                  and omega-core types.  Provides fault injection primitives
//                  (inject_oracle_stale, inject_gas_spike, etc.) that manipulate
//                  actual `LayerHealthImpl` state machines.
//
//   runner.rs    â€” `ChaosRunner`: orchestrates scenario execution, parallelism,
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

// â”€â”€ Re-exports â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub use runner::{ChaosReport, ChaosRunner};
// ScenarioOutcome is defined in scenarios â€” import directly rather than
// re-exporting through runner (where it is only an internal import, not pub).
pub use scenarios::{ScenarioConfig, ScenarioId, ScenarioOutcome, ScenarioResult};
pub use target::{ChaosTarget, FaultKind, Observation};