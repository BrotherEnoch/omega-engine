// crates/omega-strategies/src/registry.rs
//
// StrategyRegistry — maps StrategyId to a verified StrategyTrait implementation.
//
// ## Spec §8 — Strategy whitelist and bytecode verification
//
//   The registry performs two checks before any blueprint is simulated:
//
//   1. Bytecode hash verification: the deployed contract's runtime bytecode
//      hash must match `StrategyTrait::expected_bytecode_hash()`.  A mismatch
//      indicates a stale or incorrect deployment and drops the blueprint with
//      `DropCode::MissWhitelist`.
//
//   2. Phase gate: a strategy may only produce blueprints when
//      `active_phase >= strategy.phase_required()` (§1.1, §20).
//
// ## Immutability after deployment
//
//   The registry is append-only and cloned into an `arc_swap::ArcSwap<StrategyRegistry>`
//   at startup.  Governance upgrades (§13 versioned process) produce a new
//   registry instance that is atomically swapped in; the old instance is
//   retained until all in-flight blueprints referencing it complete.
//
// ## Thread safety
//
//   `StrategyRegistry` itself is `Send + Sync`.  The `ArcSwap` wrapper in
//   the orchestrator allows lock-free reads on every oracle tick.

use std::collections::HashMap;
use std::sync::Arc;

use omega_core::errors::{DropCode, OmegaError};
use omega_core::types::blueprint::StrategyId;
use omega_core::types::strategy::StrategyTrait;

// ─────────────────────────────────────────────────────────────────────────────
// RegistrationError
// ─────────────────────────────────────────────────────────────────────────────

/// Error returned when registering a strategy fails.
#[derive(Debug, thiserror::Error)]
pub enum RegistrationError {
    #[error("Strategy {0:?} is already registered — registry is append-only")]
    AlreadyRegistered(StrategyId),

    #[error("Strategy {0:?} requires phase {1} but active_phase is {2}")]
    PhaseTooLow(StrategyId, u8, u8),
}

// ─────────────────────────────────────────────────────────────────────────────
// StrategyRegistry
// ─────────────────────────────────────────────────────────────────────────────

/// Immutable (after build) mapping from `StrategyId` to `Arc<dyn StrategyTrait>`.
///
/// Built at startup via `StrategyRegistryBuilder`, then wrapped in an
/// `arc_swap::ArcSwap` for atomic hot-swap during governance upgrades.
#[derive(Clone)]
pub struct StrategyRegistry {
    strategies: HashMap<StrategyId, Arc<dyn StrategyTrait>>,
    active_phase: u8,
}

impl StrategyRegistry {
    /// Returns the strategy for `id` if it is registered AND phase-gated.
    ///
    /// Returns `None` when:
    ///   - `id` is not registered.
    ///   - The strategy's required phase exceeds `active_phase`.
    pub fn get(&self, id: StrategyId) -> Option<Arc<dyn StrategyTrait>> {
        let s = self.strategies.get(&id)?.clone();
        if s.strategy_id().phase_required() > self.active_phase {
            return None;
        }
        Some(s)
    }

    /// Returns all strategies eligible for the current active phase,
    /// sorted by descending priority (MEV=0 first, CNRY=255 last).
    pub fn active_strategies(&self) -> Vec<Arc<dyn StrategyTrait>> {
        let mut active: Vec<Arc<dyn StrategyTrait>> = self
            .strategies
            .values()
            .filter(|s| s.strategy_id().phase_required() <= self.active_phase)
            .cloned()
            .collect();
        active.sort_by_key(|s| s.priority());
        active
    }

    /// Verify a blueprint's bytecode hash matches the registered strategy.
    ///
    /// Called before simulation (§8).  Returns `Err(MissWhitelist)` on
    /// mismatch.
    pub fn verify_bytecode_hash(
        &self,
        id: StrategyId,
        blueprint_hash: alloy_primitives::B256,
    ) -> Result<(), OmegaError> {
        match self.strategies.get(&id) {
            None => Err(OmegaError::dropped(DropCode::MissWhitelist)),
            Some(s) => {
                if s.expected_bytecode_hash() != blueprint_hash {
                    tracing::error!(
                        strategy   = ?id,
                        expected   = %s.expected_bytecode_hash(),
                        actual     = %blueprint_hash,
                        "Bytecode hash mismatch — blueprint dropped (§8)",
                    );
                    Err(OmegaError::dropped(DropCode::MissWhitelist))
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Number of registered strategies.
    pub fn len(&self) -> usize {
        self.strategies.len()
    }

    /// Returns `true` when no strategies are registered.
    pub fn is_empty(&self) -> bool {
        self.strategies.is_empty()
    }

    /// Currently active phase.
    pub fn active_phase(&self) -> u8 {
        self.active_phase
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// StrategyRegistryBuilder
// ─────────────────────────────────────────────────────────────────────────────

/// Builder for `StrategyRegistry`.
///
/// Enforces uniqueness (each `StrategyId` registered once) and the phase
/// gate (strategies requiring a phase higher than `active_phase` can still
/// be registered — they simply produce no blueprints until the phase gate
/// advances).
#[derive(Default)]
pub struct StrategyRegistryBuilder {
    strategies: HashMap<StrategyId, Arc<dyn StrategyTrait>>,
    active_phase: u8,
}

impl StrategyRegistryBuilder {
    /// Create a builder for the given active phase.
    pub fn new(active_phase: u8) -> Self {
        Self {
            strategies: HashMap::new(),
            active_phase,
        }
    }

    /// Register a strategy implementation.
    ///
    /// Returns `Err(AlreadyRegistered)` if `strategy_id` is already present.
    pub fn register(mut self, strategy: Arc<dyn StrategyTrait>) -> Result<Self, RegistrationError> {
        let id = strategy.strategy_id();
        if self.strategies.contains_key(&id) {
            return Err(RegistrationError::AlreadyRegistered(id));
        }
        self.strategies.insert(id, strategy);
        Ok(self)
    }

    /// Build the immutable registry.
    pub fn build(self) -> StrategyRegistry {
        tracing::info!(
            strategy_count = self.strategies.len(),
            active_phase = self.active_phase,
            "StrategyRegistry built",
        );
        StrategyRegistry {
            strategies: self.strategies,
            active_phase: self.active_phase,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Bytes, B256, U256};
    use anyhow::Result;
    use async_trait::async_trait;
    use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
    use omega_core::types::lane::Lane;
    use omega_core::types::strategy::{OpScore, SignalState, SimResult};

    /// Minimal strategy stub for registry tests.
    struct FakeStrategy {
        id: StrategyId,
        bytecode_hash: B256,
        #[allow(dead_code)]
        phase_required: u8,
    }

    #[async_trait]
    impl StrategyTrait for FakeStrategy {
        fn strategy_id(&self) -> StrategyId {
            self.id
        }
        fn lane(&self) -> Lane {
            Lane::Normal
        }
        fn hot_path_eligible(&self) -> bool {
            false
        }
        fn gas_budget(&self) -> u64 {
            300_000
        }
        fn base_min_profit_wei(&self) -> U256 {
            U256::ZERO
        }
        fn expected_bytecode_hash(&self) -> B256 {
            self.bytecode_hash
        }
        async fn score(&self, _: &SignalState) -> Result<OpScore> {
            Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 0.0,
            })
        }
        async fn build_blueprint(&self, _: &SignalState) -> Result<ExecutionBlueprint> {
            anyhow::bail!("not implemented in test")
        }
        async fn simulate(&self, _: &ExecutionBlueprint) -> Result<SimResult> {
            anyhow::bail!("not implemented in test")
        }
        fn encode_calldata(&self, _: &ExecutionBlueprint) -> Bytes {
            Bytes::new()
        }
    }

    fn make_strategy(id: StrategyId, hash: u8) -> Arc<dyn StrategyTrait> {
        Arc::new(FakeStrategy {
            id,
            bytecode_hash: B256::from([hash; 32]),
            phase_required: id.phase_required(),
        })
    }

    #[test]
    fn register_and_get() {
        let reg = StrategyRegistryBuilder::new(3)
            .register(make_strategy(StrategyId::La, 0xAB))
            .unwrap()
            .build();

        assert!(reg.get(StrategyId::La).is_some());
        assert!(reg.get(StrategyId::Sa).is_none()); // not registered
    }

    #[test]
    fn phase_gate_blocks_higher_phase() {
        // active_phase=1, MEV requires phase 4
        let reg = StrategyRegistryBuilder::new(1)
            .register(make_strategy(StrategyId::Mev, 0x01))
            .unwrap()
            .build();
        // Registered but gated — get must return None
        assert!(reg.get(StrategyId::Mev).is_none());
    }

    #[test]
    fn phase_gate_allows_equal_phase() {
        let reg = StrategyRegistryBuilder::new(3)
            .register(make_strategy(StrategyId::La, 0x01))
            .unwrap()
            .build();
        assert!(reg.get(StrategyId::La).is_some());
    }

    #[test]
    fn duplicate_registration_returns_error() {
        let result = StrategyRegistryBuilder::new(3)
            .register(make_strategy(StrategyId::La, 0x01))
            .unwrap()
            .register(make_strategy(StrategyId::La, 0x02));
        assert!(matches!(
            result,
            Err(RegistrationError::AlreadyRegistered(_))
        ));
    }

    #[test]
    fn active_strategies_sorted_by_priority() {
        let reg = StrategyRegistryBuilder::new(4)
            .register(make_strategy(StrategyId::Sa, 0x01))
            .unwrap()
            .register(make_strategy(StrategyId::Mev, 0x02))
            .unwrap()
            .register(make_strategy(StrategyId::La, 0x03))
            .unwrap()
            .build();

        let active = reg.active_strategies();
        assert_eq!(active[0].strategy_id(), StrategyId::Mev); // priority 0
        assert_eq!(active[1].strategy_id(), StrategyId::La); // priority 1
        assert_eq!(active[2].strategy_id(), StrategyId::Sa); // priority 3
    }

    #[test]
    fn bytecode_verification_pass() {
        let hash = B256::from([0xAB; 32]);
        let reg = StrategyRegistryBuilder::new(3)
            .register(make_strategy(StrategyId::La, 0xAB))
            .unwrap()
            .build();
        assert!(reg.verify_bytecode_hash(StrategyId::La, hash).is_ok());
    }

    #[test]
    fn bytecode_verification_mismatch() {
        let wrong_hash = B256::from([0xFF; 32]);
        let reg = StrategyRegistryBuilder::new(3)
            .register(make_strategy(StrategyId::La, 0xAB))
            .unwrap()
            .build();
        let err = reg
            .verify_bytecode_hash(StrategyId::La, wrong_hash)
            .unwrap_err();
        assert!(matches!(err.drop_code(), Some(DropCode::MissWhitelist)));
    }

    #[test]
    fn bytecode_verification_unregistered() {
        let reg = StrategyRegistryBuilder::new(3).build();
        let err = reg
            .verify_bytecode_hash(StrategyId::La, B256::ZERO)
            .unwrap_err();
        assert!(matches!(err.drop_code(), Some(DropCode::MissWhitelist)));
    }
}

