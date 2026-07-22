// crates/omega-chaos/src/scenarios.rs
//
// Chaos scenario definitions â€” 14 scenarios covering all system fault classes
// (spec Â§9).
//
// ## Purpose
//
//   Each scenario exercises a distinct failure mode that production
//   engineers have identified as a risk to the engine.  Scenarios run
//   against a lightweight `ChaosTarget` that wraps real omega-health
//   and omega-core types so the faults exercise actual code paths.
//
// ## The 14 scenarios (spec Â§9)
//
//   S1  OracleStale         â€” primary oracle stops updating for 45+ seconds
//   S2  OracleDiverge       â€” Chainlink and Pyth diverge beyond 0.4%
//   S3  SequencerDown       â€” Arbitrum sequencer unresponsive for 10 blocks
//   S4  SequencerRestart    â€” sequencer returns after 10-block gap (double-spend risk Â§11.3)
//   S5  FlashCrash          â€” price drops 20% in one block
//   S6  GasSpike            â€” base fee jumps from 10 â†’ 500 gwei
//   S7  RelayTimeout        â€” all relays stop responding (no submission path)
//   S8  DagCycle            â€” circular dependency injected into blueprint DAG
//   S9  ZkProofDelay        â€” ZK proof generation takes 200ms instead of <20ms
//   S10 HealthCascade       â€” SystemHealth â†’ Halted cascades to all layers
//   S11 RevmCacheStale      â€” revm cache diverges from on-chain state
//   S12 FlashloanLiquidity  â€” flashloan provider reports 0 available liquidity
//   S13 HighCompetition     â€” 95% of opportunities are won by competitors
//   S14 RpcRateExhaust      â€” RPC budget saturated; rate limiter throttling 100%
//
// ## Scenario structure
//
//   Each scenario is a pure function that accepts a `&mut ChaosTarget`
//   and returns a `ScenarioResult`.  This makes scenarios composable:
//   they can be chained, repeated, and combined in `ChaosRunner`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ScenarioId
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Identifier for one of the 14 chaos scenarios (spec Â§9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioId {
    OracleStale,
    OracleDiverge,
    SequencerDown,
    SequencerRestart,
    FlashCrash,
    GasSpike,
    RelayTimeout,
    DagCycle,
    ZkProofDelay,
    HealthCascade,
    RevmCacheStale,
    FlashloanLiquidity,
    HighCompetition,
    RpcRateExhaust,
}

impl ScenarioId {
    /// All 14 scenario IDs in spec Â§9 order.
    pub const ALL: &'static [Self] = &[
        Self::OracleStale,
        Self::OracleDiverge,
        Self::SequencerDown,
        Self::SequencerRestart,
        Self::FlashCrash,
        Self::GasSpike,
        Self::RelayTimeout,
        Self::DagCycle,
        Self::ZkProofDelay,
        Self::HealthCascade,
        Self::RevmCacheStale,
        Self::FlashloanLiquidity,
        Self::HighCompetition,
        Self::RpcRateExhaust,
    ];

    /// Human-readable description of this scenario.
    pub fn description(self) -> &'static str {
        match self {
            Self::OracleStale        => "Primary oracle stops updating for 45+ seconds",
            Self::OracleDiverge      => "Chainlink and Pyth diverge beyond 0.4%",
            Self::SequencerDown      => "Arbitrum sequencer unresponsive for 10 blocks",
            Self::SequencerRestart   => "Sequencer returns after 10-block gap (double-spend risk)",
            Self::FlashCrash         => "Price drops 20% in one block",
            Self::GasSpike           => "Base fee jumps from 10 to 500 gwei",
            Self::RelayTimeout       => "All relays stop responding",
            Self::DagCycle           => "Circular dependency injected into blueprint DAG",
            Self::ZkProofDelay       => "ZK proof generation takes 200ms instead of <20ms",
            Self::HealthCascade      => "SystemHealth Halted cascades to all 14 layers",
            Self::RevmCacheStale     => "revm cache diverges from on-chain state",
            Self::FlashloanLiquidity => "Flashloan provider reports zero available liquidity",
            Self::HighCompetition    => "95% of opportunities won by competitors",
            Self::RpcRateExhaust     => "RPC budget saturated; rate limiter throttling 100%",
        }
    }

    /// Expected system response â€” what the engine MUST do under this fault.
    pub fn expected_response(self) -> &'static str {
        match self {
            Self::OracleStale        => "Fall back to secondary oracle; emit MissOracle if all stale",
            Self::OracleDiverge      => "Emit MissOracleDiverge; halt strategy scoring",
            Self::SequencerDown      => "Pause relay submissions; Healthâ†’Degraded; auto-resume on reconnect",
            Self::SequencerRestart   => "Dedup guard prevents double-spend; resume within 60 blocks",
            Self::FlashCrash         => "Emit MissFlashCrash; halt LA scoring; recover when price stabilises",
            Self::GasSpike           => "Emit MissGasSpike; pause aggressive/emergency bundles",
            Self::RelayTimeout       => "Relay layerâ†’Degraded; retry with backoff; no blueprint dropped silently",
            Self::DagCycle           => "Emit MissDagCycle; drop blueprint; DAG integrity maintained",
            Self::ZkProofDelay       => "ZK layerâ†’Degraded; queue blueprints; no timeout panic",
            Self::HealthCascade      => "All dependent layers halt within 200ms; HaltFlag set",
            Self::RevmCacheStale     => "SimulationStateMismatch recorded; cache refresh triggered",
            Self::FlashloanLiquidity => "MissFlashloan drop; no submission attempt; try next provider",
            Self::HighCompetition    => "ML model adjusts multipliers; no infinite retry loop",
            Self::RpcRateExhaust     => "Backpressure applied; waits for token; no panic or crash",
        }
    }
}

impl std::fmt::Display for ScenarioId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::OracleStale        => "S1_OracleStale",
            Self::OracleDiverge      => "S2_OracleDiverge",
            Self::SequencerDown      => "S3_SequencerDown",
            Self::SequencerRestart   => "S4_SequencerRestart",
            Self::FlashCrash         => "S5_FlashCrash",
            Self::GasSpike           => "S6_GasSpike",
            Self::RelayTimeout       => "S7_RelayTimeout",
            Self::DagCycle           => "S8_DagCycle",
            Self::ZkProofDelay       => "S9_ZkProofDelay",
            Self::HealthCascade      => "S10_HealthCascade",
            Self::RevmCacheStale     => "S11_RevmCacheStale",
            Self::FlashloanLiquidity => "S12_FlashloanLiquidity",
            Self::HighCompetition    => "S13_HighCompetition",
            Self::RpcRateExhaust     => "S14_RpcRateExhaust",
        };
        f.write_str(s)
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ScenarioConfig
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Runtime configuration for a single scenario execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioConfig {
    /// Which scenario to run.
    pub id: ScenarioId,

    /// How long the fault condition is held before recovery is triggered.
    /// The runner checks system behaviour during this window.
    #[serde(with = "duration_secs")]
    pub fault_duration: Duration,

    /// Maximum time allowed for the system to reach the expected response.
    /// Scenario FAILS if the system has not responded correctly by this deadline.
    #[serde(with = "duration_secs")]
    pub response_deadline: Duration,

    /// Whether recovery (fault removal) is tested after the fault phase.
    /// When `true`, the runner verifies the system returns to Healthy after
    /// the fault is cleared.
    pub test_recovery: bool,
}

impl ScenarioConfig {
    /// Default configuration for a given scenario.
    pub fn default_for(id: ScenarioId) -> Self {
        let (fault_s, deadline_s) = match id {
            ScenarioId::OracleStale        => (60,  5),
            ScenarioId::OracleDiverge      => (10,  2),
            ScenarioId::SequencerDown      => (30, 10),
            ScenarioId::SequencerRestart   => (30, 10),
            ScenarioId::FlashCrash         => (5,   2),
            ScenarioId::GasSpike           => (10,  2),
            ScenarioId::RelayTimeout       => (20,  5),
            ScenarioId::DagCycle           => (1,   1),
            ScenarioId::ZkProofDelay       => (5,   2),
            ScenarioId::HealthCascade      => (5,   1),
            ScenarioId::RevmCacheStale     => (10,  3),
            ScenarioId::FlashloanLiquidity => (5,   2),
            ScenarioId::HighCompetition    => (60, 10),
            ScenarioId::RpcRateExhaust     => (10,  3),
        };
        Self {
            id,
            fault_duration:    Duration::from_secs(fault_s),
            response_deadline: Duration::from_secs(deadline_s),
            test_recovery:     true,
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ScenarioOutcome
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Whether a scenario passed or failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioOutcome {
    /// System responded correctly within the deadline.
    Pass,
    /// System did not respond correctly within the deadline.
    Fail,
    /// Scenario was skipped (e.g. dependency not available).
    Skipped,
}

impl std::fmt::Display for ScenarioOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pass    => f.write_str("PASS"),
            Self::Fail    => f.write_str("FAIL"),
            Self::Skipped => f.write_str("SKIP"),
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ScenarioResult
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Result of a single scenario execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    /// Which scenario ran.
    pub id:               ScenarioId,
    /// Pass / fail / skip.
    pub outcome:          ScenarioOutcome,
    /// UTC timestamp when the scenario started.
    pub started_at:       DateTime<Utc>,
    /// How long the scenario took (fault + recovery phases).
    #[serde(with = "duration_secs")]
    pub elapsed:          Duration,
    /// Observed system behaviour during the fault window.
    pub observations:     Vec<String>,
    /// Failure reason when `outcome == Fail`.
    pub failure_reason:   Option<String>,
    /// Whether recovery was verified (when `test_recovery == true`).
    pub recovery_verified: Option<bool>,
}

impl ScenarioResult {
    /// Construct a passing result.
    pub fn pass(
        id:           ScenarioId,
        started_at:   DateTime<Utc>,
        elapsed:      Duration,
        observations: Vec<String>,
        recovery_ok:  Option<bool>,
    ) -> Self {
        Self {
            id,
            outcome:           ScenarioOutcome::Pass,
            started_at,
            elapsed,
            observations,
            failure_reason:    None,
            recovery_verified: recovery_ok,
        }
    }

    /// Construct a failing result.
    pub fn fail(
        id:           ScenarioId,
        started_at:   DateTime<Utc>,
        elapsed:      Duration,
        observations: Vec<String>,
        reason:       impl Into<String>,
    ) -> Self {
        Self {
            id,
            outcome:           ScenarioOutcome::Fail,
            started_at,
            elapsed,
            observations,
            failure_reason:    Some(reason.into()),
            recovery_verified: None,
        }
    }

    /// Construct a skipped result.
    pub fn skipped(id: ScenarioId, reason: impl Into<String>) -> Self {
        Self {
            id,
            outcome:           ScenarioOutcome::Skipped,
            started_at:        Utc::now(),
            elapsed:           Duration::ZERO,
            observations:      vec![reason.into()],
            failure_reason:    None,
            recovery_verified: None,
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Serde helpers
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

mod duration_secs {
    use std::time::Duration;
    use serde::{Deserializer, Serializer, Deserialize};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_secs(u64::deserialize(d)?))
    }
}