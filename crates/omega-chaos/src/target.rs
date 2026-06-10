// crates/omega-chaos/src/target.rs
//
// ChaosTarget — injectable system state for chaos scenario execution.
//
// ## Purpose
//
//   Chaos scenarios need to inject faults into and observe responses from
//   the engine's health, relay, and oracle subsystems.  `ChaosTarget`
//   wraps real `omega_core` and `omega_health` types so scenarios exercise
//   actual production code paths, not mocks.
//
// ## Design
//
//   The target is constructed fresh for each scenario run (or shared
//   across a suite run).  It owns:
//     - Health layer controllers (all 16 LayerId variants)
//     - HaltFlag (system-wide emergency halt)
//     - FaultState (active fault conditions the scenario has injected)
//
//   Scenarios call `target.inject(fault)` to set a fault condition,
//   then `target.observe()` to read the resulting health states.
//   The runner clears faults between scenarios via `target.clear_faults()`.
//
// ## Thread safety
//
//   `ChaosTarget` is `Send + Sync` — scenarios may be run concurrently
//   in a test suite, each with its own `Arc<ChaosTarget>`.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use omega_core::{HealthState, LayerId};
use omega_health::{HaltFlag, LayerHealthImpl};

// ─────────────────────────────────────────────────────────────────────────────
// FaultKind
// ─────────────────────────────────────────────────────────────────────────────

/// A discrete fault condition that can be injected into the system.
///
/// Scenarios inject one or more `FaultKind`s into `ChaosTarget::inject()`.
/// The runner observes the system response and clears faults after verification.
///
/// `Eq` and `Hash` are intentionally NOT derived: two variants (`FlashCrash`
/// and `HighCompetition`) contain `f64` fields, which implement neither `Eq`
/// nor `Hash`.  All fault matching uses predicate closures via `has_fault()`,
/// and active faults are stored in a `Vec` — no `HashMap` key usage exists.
#[derive(Debug, Clone, PartialEq)]
pub enum FaultKind {
    /// Oracle feed is stale — `received_at` is more than `stale_secs` old.
    OracleStale { feed: String, stale_secs: u64 },
    /// Two oracle feeds diverge beyond the threshold.
    OracleDiverge {
        feed_a: String,
        feed_b: String,
        diverge_bps: u32,
    },
    /// Sequencer is not producing blocks.
    SequencerDown,
    /// Sequencer has just restarted after a gap — double-spend risk window.
    SequencerRestart { gap_blocks: u64 },
    /// Price dropped by `drop_pct` in one block.
    FlashCrash { asset: String, drop_pct: f64 },
    /// Base fee spiked to `fee_gwei`.
    GasSpike { fee_gwei: u64 },
    /// All relay endpoints are unresponsive.
    RelayTimeout,
    /// DAG has a circular dependency.
    DagCycle { cycle_description: String },
    /// ZK proof generation latency is `latency_ms` milliseconds.
    ZkProofDelay { latency_ms: u64 },
    /// Layer is forced to Halted (for cascade testing).
    LayerHalted { layer: LayerId, reason: String },
    /// revm cache is behind the chain head by `blocks_behind` blocks.
    RevmCacheStale { blocks_behind: u64 },
    /// Flashloan provider has zero available liquidity.
    FlashloanDry { provider: String },
    /// `win_pct`% of all scoring opportunities are won by competitors.
    HighCompetition { win_pct: f64 },
    /// RPC token bucket is at zero tokens — all requests throttled.
    RpcRateExhausted,
}

impl std::fmt::Display for FaultKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OracleStale { feed, stale_secs } => {
                write!(f, "OracleStale(feed={feed}, stale={stale_secs}s)")
            }
            Self::OracleDiverge { feed_a, feed_b, diverge_bps } => {
                write!(f, "OracleDiverge({feed_a}/{feed_b} {diverge_bps}bps)")
            }
            Self::SequencerDown => f.write_str("SequencerDown"),
            Self::SequencerRestart { gap_blocks } => {
                write!(f, "SequencerRestart(gap={gap_blocks}blk)")
            }
            Self::FlashCrash { asset, drop_pct } => {
                write!(f, "FlashCrash({asset} -{drop_pct:.0}%)")
            }
            Self::GasSpike { fee_gwei } => write!(f, "GasSpike({fee_gwei}gwei)"),
            Self::RelayTimeout => f.write_str("RelayTimeout"),
            Self::DagCycle { cycle_description } => write!(f, "DagCycle({cycle_description})"),
            Self::ZkProofDelay { latency_ms } => write!(f, "ZkProofDelay({latency_ms}ms)"),
            Self::LayerHalted { layer, reason } => write!(f, "LayerHalted({layer}: {reason})"),
            Self::RevmCacheStale { blocks_behind } => {
                write!(f, "RevmCacheStale({blocks_behind}blk behind)")
            }
            Self::FlashloanDry { provider } => write!(f, "FlashloanDry({provider})"),
            Self::HighCompetition { win_pct } => {
                write!(f, "HighCompetition({win_pct:.0}% competitor wins)")
            }
            Self::RpcRateExhausted => f.write_str("RpcRateExhausted"),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Observation
// ─────────────────────────────────────────────────────────────────────────────

/// A single system observation recorded during a scenario.
#[derive(Debug, Clone)]
pub struct Observation {
    /// When this was observed.
    pub elapsed: std::time::Duration,
    /// Human-readable description.
    pub message: String,
    /// Whether this observation is consistent with the expected response.
    pub expected: bool,
}

impl Observation {
    pub fn expected(elapsed: std::time::Duration, message: impl Into<String>) -> Self {
        Self { elapsed, message: message.into(), expected: true }
    }
    pub fn unexpected(elapsed: std::time::Duration, message: impl Into<String>) -> Self {
        Self { elapsed, message: message.into(), expected: false }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ChaosTarget
// ─────────────────────────────────────────────────────────────────────────────

/// Injectable system state for chaos scenario execution.
pub struct ChaosTarget {
    /// Health controllers for all 16 layers — real `LayerHealthImpl` instances.
    pub health_layers: HashMap<LayerId, Arc<LayerHealthImpl>>,
    /// System-wide halt flag.
    pub halt_flag: HaltFlag,
    /// Currently active fault conditions.
    active_faults: RwLock<Vec<FaultKind>>,
    /// Observations recorded during the current scenario.
    observations: RwLock<Vec<Observation>>,
    /// Scenario start time for elapsed calculation.
    start_time: Instant,
}

impl ChaosTarget {
    /// Construct a fresh target with all layers in `Healthy` state.
    pub fn new() -> Arc<Self> {
        // Use canonical v12 LayerId variants directly.
        // Back-compat aliases (SystemHealth, ExternalData, etc.) are associated
        // constants on LayerId — they cannot be glob-imported with `use LayerId::*`.
        // Using the canonical names avoids that limitation entirely.
        let layer_ids = [
            LayerId::Health,
            LayerId::Rpc,
            LayerId::Oracle,
            LayerId::Security,
            LayerId::Compliance,
            LayerId::Risk,
            LayerId::Dag,
            LayerId::Zk,
            LayerId::FlashLoan,
            LayerId::Relay,
            LayerId::GasWar,
            LayerId::LossAttribution,
            LayerId::AddressRotation,
            LayerId::Strategies,
            LayerId::HotPath,
            LayerId::Observability,
        ];

        let mut health_layers = HashMap::with_capacity(layer_ids.len());
        for id in layer_ids {
            health_layers.insert(id, LayerHealthImpl::new_bare(id));
        }

        Arc::new(Self {
            health_layers,
            halt_flag:     HaltFlag::new(),
            active_faults: RwLock::new(Vec::new()),
            observations:  RwLock::new(Vec::new()),
            start_time:    Instant::now(),
        })
    }

    // ── Fault injection ──────────────────────────────────────────────────────

    /// Inject a fault condition into the target.
    ///
    /// For `LayerHalted` faults the layer's health controller is
    /// immediately transitioned to `Halted`.  All other faults are
    /// recorded in `active_faults` for scenario logic to observe.
    pub fn inject(&self, fault: FaultKind) {
        tracing::info!(fault = %fault, "CHAOS: fault injected");

        match &fault {
            FaultKind::LayerHalted { layer, reason } => {
                if let Some(ctrl) = self.health_layers.get(layer) {
                    use omega_core::LayerHealth;
                    ctrl.set_state(HealthState::Halted, reason);
                }
            }
            FaultKind::SequencerDown | FaultKind::RelayTimeout => {
                use omega_core::LayerHealth;
                // Sequencer/relay faults degrade the Relay layer (v12 canonical name).
                if let Some(ctrl) = self.health_layers.get(&LayerId::Relay) {
                    ctrl.set_state(HealthState::Degraded, &fault.to_string());
                }
            }
            _ => {}
        }

        self.active_faults.write().unwrap().push(fault);
    }

    /// Remove all active faults — used to test recovery.
    pub fn clear_faults(&self) {
        tracing::info!("CHAOS: all faults cleared — recovery phase");
        self.active_faults.write().unwrap().clear();
    }

    /// Returns a snapshot of currently active faults.
    pub fn active_faults(&self) -> Vec<FaultKind> {
        self.active_faults.read().unwrap().clone()
    }

    /// Returns `true` when the given fault kind is currently active.
    pub fn has_fault(&self, predicate: impl Fn(&FaultKind) -> bool) -> bool {
        self.active_faults.read().unwrap().iter().any(predicate)
    }

    // ── Observation recording ─────────────────────────────────────────────────

    /// Record an observation.
    pub fn observe(&self, message: impl Into<String>, is_expected: bool) {
        let elapsed = self.start_time.elapsed();
        let obs = if is_expected {
            Observation::expected(elapsed, message)
        } else {
            Observation::unexpected(elapsed, message)
        };
        tracing::debug!(
            elapsed_ms = elapsed.as_millis(),
            expected   = obs.expected,
            msg        = %obs.message,
            "CHAOS: observation",
        );
        self.observations.write().unwrap().push(obs);
    }

    /// Drain and return all recorded observations.
    pub fn take_observations(&self) -> Vec<Observation> {
        std::mem::take(&mut self.observations.write().unwrap())
    }

    // ── Health state queries ──────────────────────────────────────────────────

    /// Current health state of a specific layer.
    pub fn layer_state(&self, id: LayerId) -> Option<HealthState> {
        use omega_core::LayerHealth;
        self.health_layers.get(&id).map(|l| l.state())
    }

    /// Returns `true` when all layers are in `Healthy` state.
    pub fn all_healthy(&self) -> bool {
        use omega_core::LayerHealth;
        self.health_layers.values().all(|l| l.state().is_healthy())
    }

    /// Returns all layers currently in `Halted` state.
    pub fn halted_layers(&self) -> Vec<LayerId> {
        use omega_core::LayerHealth;
        self.health_layers
            .iter()
            .filter(|(_, l)| l.state() == HealthState::Halted)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Transition a layer to `Healthy` (recovery simulation).
    pub fn recover_layer(&self, id: LayerId) {
        use omega_core::LayerHealth;
        if let Some(ctrl) = self.health_layers.get(&id) {
            ctrl.set_state(HealthState::Healthy, "chaos recovery");
        }
    }

    /// Recover all non-healthy layers (full system recovery simulation).
    pub fn recover_all(&self) {
        use omega_core::LayerHealth;
        for ctrl in self.health_layers.values() {
            if !ctrl.state().is_healthy() {
                ctrl.set_state(HealthState::Healthy, "chaos full-recovery");
            }
        }
        if self.halt_flag.is_halted() {
            self.halt_flag.clear(LayerId::SystemHealth, "chaos recovery");
        }
    }

    /// Reset all layers to Healthy and clear all faults and observations.
    pub fn reset(&self) {
        self.clear_faults();
        self.recover_all();
        self.observations.write().unwrap().clear();
    }

    /// Elapsed time since this target was constructed.
    pub fn elapsed(&self) -> std::time::Duration {
        self.start_time.elapsed()
    }
}

impl Default for ChaosTarget {
    fn default() -> Self {
        Arc::try_unwrap(Self::new()).unwrap_or_else(|_| unreachable!("new() returns a unique Arc"))
    }
}