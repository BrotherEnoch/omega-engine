# apply_blueprint_fixes.ps1
# Run from C:\Users\silve\Documents\omega-engine
# Writes all five corrected ExecutionBlueprint call-site files directly.
$ErrorActionPreference = 'Stop'

Write-Host 'Writing crates\omega-dag\src\tests.rs...'
$content_0 = @'
// crates/omega-dag/src/tests.rs

use alloy_primitives::{Address, B256, U256};
use omega_core::errors::DropCode;
use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
use omega_core::types::flashloan_provider::FlashloanProviderType;
use omega_core::types::lane::{Lane, Simulator};
use uuid::Uuid;

use crate::scheduler::ExecutionDag;
use crate::types::{DagConfig, DagError};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn test_config() -> DagConfig {
    DagConfig {
        microtx_slots: 4,
        normal_slots: 8,
        eviction_log_capacity: 1_000,
    }
}

fn make_bp(hash_byte: u8, strategy: StrategyId, lane: Lane) -> ExecutionBlueprint {
    let mut hash = B256::ZERO;
    hash.0[0] = hash_byte;
    // signal_id/client_order_id/idempotency_key: these DAG scheduling
    // tests exercise admission/dependency/eviction logic only, never
    // verify_hash()/verify_idempotency_key(), so — same as
    // blueprint_hash above — these are deterministic placeholders keyed
    // off hash_byte for uniqueness across test cases, not integrity-
    // checked values. Do not copy this pattern into code that DOES rely
    // on hash/idempotency integrity.
    let signal_id = Uuid::from_bytes([hash_byte; 16]);
    let client_order_id = ExecutionBlueprint::derive_client_order_id(strategy, 42161, 0, signal_id);
    ExecutionBlueprint {
        blueprint_hash: hash,
        chain_id: 42161,
        strategy_id: strategy,
        lane,
        simulator: Simulator::Revm,
        signal_state_hash: B256::ZERO,
        state_version: 1,
        signal_id,
        flashloan_provider: Address::ZERO,
        flashloan_amount: U256::ZERO,
        flashloan_available: U256::ZERO,
        // No flashloan is actually sourced by these scheduler-only test
        // blueprints (flashloan_provider is the legacy ZERO sentinel
        // above); provider_contract/flashloan_token mirror that with
        // ZERO, and flashloan_provider_type picks an arbitrary variant
        // since ExecutionBlueprint has no "none" discriminant for it —
        // none of these DAG tests read this field.
        flashloan_provider_type: FlashloanProviderType::Balancer,
        provider_contract: Address::ZERO,
        flashloan_token: Address::ZERO,
        calldata: Default::default(),
        strategy_bytecode_hash: B256::ZERO,
        l2_exec_gas_estimate: 21_000,
        l1_data_gas_estimate: 0,
        extraction_gas: 21_000,
        expected_profit_net: U256::from(1_000_000_u64),
        dynamic_min_profit: U256::from(100_000_u64),
        l2_buffer_factor: 1.15,
        l1_data_buffer_factor: 1.10,
        slippage_bps: 100,
        base_fee_at_creation: 10,
        l1_data_fee_at_creation: 2,
        priority_fee_gwei: 10,
        max_base_fee_gwei: ExecutionBlueprint::derive_max_base_fee_gwei(10, 3.0),
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

// ─────────────────────────────────────────────────────────────────────────────
// Admit / complete
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn admit_and_complete_basic() {
    let mut dag = ExecutionDag::new(test_config());
    let bp = make_bp(1, StrategyId::Sa, Lane::Microtx);
    let hash = bp.blueprint_hash;

    dag.admit(bp, &[]).unwrap();
    assert_eq!(dag.microtx_count(), 1);
    assert!(dag.contains(&hash));

    let unblocked = dag.complete(hash);
    assert!(unblocked.is_empty(), "no successors");
    assert_eq!(dag.microtx_count(), 0);
    assert!(!dag.contains(&hash));
}

#[test]
fn ready_returns_nodes_with_no_deps() {
    let mut dag = ExecutionDag::new(test_config());
    let a = make_bp(1, StrategyId::Sa, Lane::Microtx);
    let b = make_bp(2, StrategyId::Sa, Lane::Microtx);
    let a_hash = a.blueprint_hash;
    let b_hash = b.blueprint_hash;

    dag.admit(a, &[]).unwrap();
    dag.admit(b, &[a_hash]).unwrap();

    let ready = dag.ready();
    assert_eq!(ready.len(), 1);
    assert!(ready.contains(&a_hash));
    assert!(!ready.contains(&b_hash));
}

#[test]
fn completing_dep_unblocks_successor() {
    let mut dag = ExecutionDag::new(test_config());
    let a = make_bp(1, StrategyId::Sa, Lane::Microtx);
    let b = make_bp(2, StrategyId::Sa, Lane::Microtx);
    let a_hash = a.blueprint_hash;
    let b_hash = b.blueprint_hash;

    dag.admit(a, &[]).unwrap();
    dag.admit(b, &[a_hash]).unwrap();

    let unblocked = dag.complete(a_hash);
    assert_eq!(unblocked, vec![b_hash]);
}

#[test]
fn multi_dep_node_only_ready_when_all_deps_complete() {
    let mut dag = ExecutionDag::new(test_config());
    let a = make_bp(1, StrategyId::Sa, Lane::Microtx);
    let b = make_bp(2, StrategyId::Sa, Lane::Microtx);
    let c = make_bp(3, StrategyId::Sa, Lane::Microtx);
    let a_hash = a.blueprint_hash;
    let b_hash = b.blueprint_hash;
    let c_hash = c.blueprint_hash;

    dag.admit(a, &[]).unwrap();
    dag.admit(b, &[]).unwrap();
    dag.admit(c, &[a_hash, b_hash]).unwrap();

    let ready = dag.ready();
    assert!(!ready.contains(&c_hash));

    let after_a = dag.complete(a_hash);
    assert!(!after_a.contains(&c_hash));

    let after_b = dag.complete(b_hash);
    assert!(after_b.contains(&c_hash));
}

// ─────────────────────────────────────────────────────────────────────────────
// Cycle / dependency detection
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn dependency_on_unknown_hash_returns_cycle_error() {
    let mut dag = ExecutionDag::new(test_config());
    let mut bad_hash = B256::ZERO;
    bad_hash.0[0] = 0xFF;

    let d = make_bp(4, StrategyId::Msa, Lane::Normal);
    // scheduler maps DependencyNotFound → Cycle(String)
    assert!(matches!(dag.admit(d, &[bad_hash]), Err(DagError::Cycle(_))));
}

#[test]
fn three_node_dag_no_cycle() {
    let mut dag = ExecutionDag::new(test_config());
    let a = make_bp(1, StrategyId::Msa, Lane::Normal);
    let b = make_bp(2, StrategyId::Msa, Lane::Normal);
    let c = make_bp(3, StrategyId::Msa, Lane::Normal);
    let a_hash = a.blueprint_hash;
    let b_hash = b.blueprint_hash;

    dag.admit(a, &[]).unwrap();
    dag.admit(b, &[a_hash]).unwrap();
    dag.admit(c, &[b_hash]).unwrap();

    assert_eq!(dag.node_count(), 3);
}

// ─────────────────────────────────────────────────────────────────────────────
// Capacity
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn capacity_full_rejects_same_priority_blueprint() {
    let config = DagConfig {
        microtx_slots: 2,
        normal_slots: 8,
        eviction_log_capacity: 100,
    };
    let mut dag = ExecutionDag::new(config);

    dag.admit(make_bp(1, StrategyId::Sa, Lane::Microtx), &[])
        .unwrap();
    dag.admit(make_bp(2, StrategyId::Sa, Lane::Microtx), &[])
        .unwrap();

    // DagError::LaneFull — not CapacityFull (doesn't exist in types.rs)
    let result = dag.admit(make_bp(3, StrategyId::Sa, Lane::Microtx), &[]);
    assert!(matches!(result, Err(DagError::LaneFull { .. })));
}

#[test]
fn higher_priority_evicts_lower() {
    let config = DagConfig {
        microtx_slots: 1,
        normal_slots: 8,
        eviction_log_capacity: 100,
    };
    let mut dag = ExecutionDag::new(config);

    let sa = make_bp(1, StrategyId::Sa, Lane::Microtx);
    let sa_hash = sa.blueprint_hash;
    dag.admit(sa, &[]).unwrap();
    assert_eq!(dag.microtx_count(), 1);

    let la = make_bp(2, StrategyId::La, Lane::Microtx);
    dag.admit(la, &[]).unwrap();

    assert!(!dag.contains(&sa_hash));
    assert_eq!(dag.microtx_count(), 1);
    assert_eq!(dag.evictions().len(), 1);
}

#[test]
fn eviction_record_correct_strategy_info() {
    let config = DagConfig {
        microtx_slots: 1,
        normal_slots: 8,
        eviction_log_capacity: 100,
    };
    let mut dag = ExecutionDag::new(config);

    dag.admit(make_bp(1, StrategyId::Sa, Lane::Microtx), &[])
        .unwrap();
    dag.admit(make_bp(2, StrategyId::La, Lane::Microtx), &[])
        .unwrap();

    let eviction = &dag.evictions()[0];
    // evicted_strat is String, not StrategyId — verify it names SA
    assert!(
        eviction.evicted_strat.contains("SA"),
        "expected SA in evicted_strat, got: {}",
        eviction.evicted_strat
    );
    // caused_by names the incoming strategy
    assert!(
        eviction.caused_by.contains("LA"),
        "expected LA in caused_by, got: {}",
        eviction.caused_by
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// CNRY exemption
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn cnry_does_not_consume_slots() {
    let config = DagConfig {
        microtx_slots: 1,
        normal_slots: 1,
        eviction_log_capacity: 100,
    };
    let mut dag = ExecutionDag::new(config);

    dag.admit(make_bp(1, StrategyId::Sa, Lane::Microtx), &[])
        .unwrap();
    dag.admit(make_bp(2, StrategyId::Msa, Lane::Normal), &[])
        .unwrap();

    dag.admit(make_bp(3, StrategyId::Cnry, Lane::Microtx), &[])
        .unwrap();
    dag.admit(make_bp(4, StrategyId::Cnry, Lane::Normal), &[])
        .unwrap();

    assert_eq!(dag.microtx_count(), 1);
    assert_eq!(dag.normal_count(), 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Duplicate rejection
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn duplicate_blueprint_rejected() {
    let mut dag = ExecutionDag::new(test_config());
    let bp = make_bp(1, StrategyId::Sa, Lane::Microtx);

    dag.admit(bp.clone(), &[]).unwrap();
    assert!(matches!(dag.admit(bp, &[]), Err(DagError::Cycle(_))));
}

// ─────────────────────────────────────────────────────────────────────────────
// Snapshot — use actual DagSnapshot field names from types.rs
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn snapshot_reflects_state() {
    let mut dag = ExecutionDag::new(test_config());
    dag.admit(make_bp(1, StrategyId::Sa, Lane::Microtx), &[])
        .unwrap();
    dag.admit(make_bp(2, StrategyId::Msa, Lane::Normal), &[])
        .unwrap();

    let snap = dag.snapshot();
    assert_eq!(snap.microtx_used, 1);
    assert_eq!(snap.normal_used, 1);
    assert_eq!(snap.total_admitted, 2);
    assert_eq!(dag.ready().len(), 2); // no deps → both ready
}

#[test]
fn snapshot_eviction_count_after_eviction() {
    let config = DagConfig {
        microtx_slots: 1,
        normal_slots: 8,
        eviction_log_capacity: 100,
    };
    let mut dag = ExecutionDag::new(config);

    dag.admit(make_bp(1, StrategyId::Sa, Lane::Microtx), &[])
        .unwrap();
    dag.admit(make_bp(2, StrategyId::La, Lane::Microtx), &[])
        .unwrap();

    let snap = dag.snapshot();
    assert_eq!(snap.total_evicted, 1); // not eviction_count
}

// ─────────────────────────────────────────────────────────────────────────────
// OmegaError mapping
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn cycle_error_maps_to_miss_dag_cycle() {
    let err = DagError::Cycle("test".to_string());
    assert!(matches!(
        err.to_omega_error(),
        omega_core::errors::OmegaError::Dropped {
            code: DropCode::MissDagCycle
        }
    ));
}

#[test]
fn lane_full_microtx_maps_to_miss_capacity() {
    let err = DagError::LaneFull {
        lane: Lane::Microtx,
        capacity: 4,
    };
    assert!(matches!(
        err.to_omega_error(),
        omega_core::errors::OmegaError::Dropped {
            code: DropCode::MissCapacity
        }
    ));
}

#[test]
fn lane_full_normal_maps_to_miss_capacity_normal() {
    let err = DagError::LaneFull {
        lane: Lane::Normal,
        capacity: 8,
    };
    assert!(matches!(
        err.to_omega_error(),
        omega_core::errors::OmegaError::Dropped {
            code: DropCode::MissCapacityNormal
        }
    ));
}

'@
Set-Content -Path 'crates\omega-dag\src\tests.rs' -Value $content_0 -Encoding UTF8 -NoNewline

Write-Host 'Writing crates\omega-strategies\src\cnry.rs...'
$content_1 = @'
// crates/omega-strategies/src/cnry.rs
//
// Canary (CNRY) — Phase 0 signal validator (spec §1.1).
//
// ## Audit note (this revision)
//
// `build_blueprint` here never actually constructs an `ExecutionBlueprint`
// — it always returns `Err(MissWhitelist)` before reaching that point, by
// design (CNRY must never produce a real blueprint). So none of the
// `blueprint_hash` inconsistency issues fixed in sa.rs/la.rs/msa.rs/mev.rs
// apply to this file's production code. The only change needed here is
// keeping the one test-only blueprint literal below in sync with
// `ExecutionBlueprint`'s required field set, since that struct now also
// requires `flashloan_provider_type`, `provider_contract`,
// `flashloan_token`, and `max_base_fee_gwei` at every construction site
// (added in `omega-core` to support real flashloan provider/pool
// selection — see that crate's `types::blueprint` module doc comment).
// CNRY never sources a flashloan, so these are ZERO/placeholder values
// here, same treatment as the pre-existing `signal_id`/`client_order_id`/
// `idempotency_key` placeholders below.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use alloy_primitives::{Bytes, B256, U256};
use anyhow::Result;
use async_trait::async_trait;

use omega_core::errors::{DropCode, OmegaError};
use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
use omega_core::types::lane::Lane;
use omega_core::types::strategy::{OpScore, SignalState, SimResult, StrategyTrait};
use omega_core::{GasConfig, OmegaConfig};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

const CNRY_GAS_BUDGET: u64 = 0;
const CNRY_BYTECODE_HASH: B256 = B256::ZERO;
const CNRY_SPREAD_WEI: u128 = 200_000_000_000_000_000; // 0.2 ETH

// ─────────────────────────────────────────────────────────────────────────────
// CnryStrategy
// ─────────────────────────────────────────────────────────────────────────────

pub struct CnryStrategy {
    chain_id: u64,
    scored_count: AtomicU64,
    gas: GasConfig,
}

impl CnryStrategy {
    pub fn new(chain_id: u64, config: &OmegaConfig) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            scored_count: AtomicU64::new(0),
            gas: config.gas.clone(),
        })
    }

    pub fn scored_count(&self) -> u64 {
        self.scored_count.load(Ordering::Relaxed)
    }

    fn compute_score(&self, signal: &SignalState) -> OpScore {
        let fee_pressure = signal.base_fee_gwei as f64 / 50.0;
        if fee_pressure > 1.0 {
            return OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 1.0,
            };
        }

        let spread_wei = U256::from(CNRY_SPREAD_WEI);
        let l2_cost_gwei = (200_000_f64
            * self.gas.l2_buffer_factor
            * (signal.base_fee_gwei as f64
                + self.gas.max_priority_fee_gwei as f64 * self.gas.conservative_fee_fraction))
            as u64;
        let cost_wei = U256::from(l2_cost_gwei).saturating_mul(U256::from(1_000_000_000_u64));

        if spread_wei <= cost_wei {
            return OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 0.5,
            };
        }

        let net = spread_wei.saturating_sub(cost_wei);
        let competition_prob = 0.35_f64;
        let score = (1.0 - competition_prob) * (net.saturating_to::<u128>() as f64 / 1e15).min(1.0);

        OpScore {
            score: score.clamp(0.0, 1.0),
            expected_profit: net,
            competition_prob,
        }
    }
}

#[async_trait]
impl StrategyTrait for CnryStrategy {
    fn strategy_id(&self) -> StrategyId {
        StrategyId::Cnry
    }
    fn lane(&self) -> Lane {
        Lane::Microtx
    }
    fn hot_path_eligible(&self) -> bool {
        false
    }
    fn gas_budget(&self) -> u64 {
        CNRY_GAS_BUDGET
    }
    fn expected_bytecode_hash(&self) -> B256 {
        CNRY_BYTECODE_HASH
    }
    fn is_canary(&self) -> bool {
        true
    }

    fn base_min_profit_wei(&self) -> U256 {
        U256::ZERO
    }

    async fn score(&self, signal: &SignalState) -> Result<OpScore> {
        let op = self.compute_score(signal);
        self.scored_count.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(
            chain_id = self.chain_id,
            block_number = signal.block_number,
            score = op.score,
            "CNRY scored opportunity",
        );
        Ok(op)
    }

    async fn build_blueprint(&self, _signal: &SignalState) -> Result<ExecutionBlueprint> {
        Err(anyhow::anyhow!(OmegaError::dropped(
            DropCode::MissWhitelist
        )))
    }

    async fn simulate(&self, _bp: &ExecutionBlueprint) -> Result<SimResult> {
        Err(anyhow::anyhow!(OmegaError::dropped(
            DropCode::MissWhitelist
        )))
    }

    fn encode_calldata(&self, _bp: &ExecutionBlueprint) -> Bytes {
        Bytes::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    // Address was missing — fixes E0433 at lines 261 and 299
    use alloy_primitives::Address;
    use omega_core::types::flashloan_provider::FlashloanProviderType;
    use omega_core::OmegaConfig;
    use uuid::Uuid;

    fn make_strategy() -> Arc<CnryStrategy> {
        CnryStrategy::new(42161, &OmegaConfig::default())
    }

    fn signal(base_fee: u64) -> SignalState {
        SignalState {
            state_version: 1,
            chain_id: 42161,
            block_number: 1_000_000,
            base_fee_gwei: base_fee,
            l1_data_fee_gwei: 2,
            state_hash: B256::from([0x01; 32]),
        }
    }

    /// Test-only blueprint literal. CNRY's real `build_blueprint` never
    /// constructs one of these (see module doc comment), so signal_id/
    /// client_order_id/idempotency_key here are just placeholders to
    /// satisfy the struct's field list — not integrity-checked values.
    /// Do not copy this pattern into code that DOES rely on
    /// verify_hash()/verify_idempotency_key(). Same treatment applies to
    /// the flashloan_provider_type/provider_contract/flashloan_token/
    /// max_base_fee_gwei fields added in this revision: CNRY never
    /// sources a flashloan, so these are ZERO/arbitrary placeholders.
    fn test_blueprint() -> ExecutionBlueprint {
        let signal_id = Uuid::from_bytes([0x00u8; 16]);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(StrategyId::Cnry, 42161, 0, signal_id);
        ExecutionBlueprint {
            blueprint_hash: B256::ZERO,
            chain_id: 42161,
            strategy_id: StrategyId::Cnry,
            lane: Lane::Microtx,
            simulator: omega_core::types::lane::Simulator::Revm,
            signal_state_hash: B256::ZERO,
            state_version: 0,
            signal_id,
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::ZERO,
            flashloan_available: U256::ZERO,
            flashloan_provider_type: FlashloanProviderType::Balancer,
            provider_contract: Address::ZERO,
            flashloan_token: Address::ZERO,
            calldata: Bytes::new(),
            strategy_bytecode_hash: B256::ZERO,
            l2_exec_gas_estimate: 0,
            l1_data_gas_estimate: 0,
            extraction_gas: 0,
            expected_profit_net: U256::ZERO,
            dynamic_min_profit: U256::ZERO,
            l2_buffer_factor: 1.0,
            l1_data_buffer_factor: 1.0,
            slippage_bps: 0,
            base_fee_at_creation: 0,
            l1_data_fee_at_creation: 0,
            priority_fee_gwei: 0,
            max_base_fee_gwei: 0,
            price_impact_bps: None,
            ofa_compliant: false,
            expiry_block: 0,
            nonce: 0,
            confirmation_depth: 12,
            client_order_id,
            idempotency_key: B256::ZERO,
            relay_targets: vec![],
            zk_proof_commitment: None,
        }
    }

    #[test]
    fn is_canary() {
        let s = make_strategy();
        assert!(s.is_canary());
        assert_eq!(s.strategy_id(), StrategyId::Cnry);
        assert!(!s.hot_path_eligible());
        assert_eq!(s.gas_budget(), 0);
    }

    #[tokio::test]
    async fn score_low_fee_positive() {
        let s = make_strategy();
        let op = s.score(&signal(5)).await.unwrap();
        assert!(op.score > 0.0);
        assert_eq!(s.scored_count(), 1);
    }

    #[tokio::test]
    async fn score_high_fee_zero() {
        let s = make_strategy();
        let op = s.score(&signal(100)).await.unwrap();
        assert_eq!(op.score, 0.0);
    }

    #[tokio::test]
    async fn build_blueprint_blocked() {
        let s = make_strategy();
        let err = s.build_blueprint(&signal(5)).await;
        assert!(err.is_err(), "CNRY must never build blueprints");
    }

    #[tokio::test]
    async fn simulate_blocked() {
        let s = make_strategy();
        let bp = test_blueprint();
        assert!(s.simulate(&bp).await.is_err(), "CNRY must never simulate");
    }

    #[test]
    fn encode_calldata_empty() {
        let s = make_strategy();
        let bp = test_blueprint();
        assert!(s.encode_calldata(&bp).is_empty());
    }
}

'@
Set-Content -Path 'crates\omega-strategies\src\cnry.rs' -Value $content_1 -Encoding UTF8 -NoNewline

Write-Host 'Writing crates\omega-hot-path\src\gate.rs...'
$content_2 = @'
// crates/omega-hot-path/src/gate.rs
//
// HotPathGate — admission control for the <1ms hot-path execution lane.
//
// ## Spec §4 constraints
//
//   Only two strategy configurations may enter the hot path:
//     1. SA (Simple Arbitrage) — Microtx lane, gas < 200,000
//     2. LA hot-tier — HF < 1.01 (§11.1), Microtx lane
//
//   Canary blueprints (CNRY) must never reach the hot path — they have
//   no on-chain execution.
//
// ## Slot budget
//
//   The hot path is CPU-pinned (§4) with a fixed slot budget.  When all
//   slots are occupied the gate drops new blueprints with
//   `DropCode::MissCapacity`.  Slots are released on `release()`.
//
// ## Max reads per blueprint
//
//   §4 mandates: max 8 reads per Microtx blueprint.  The gate records
//   the read budget on admission and the simulator enforces it.
//
// ## Audit fix (this revision): test helper missing required fields
//
// `tests::make_bp` constructed `ExecutionBlueprint` without `signal_id`,
// `client_order_id`, or `idempotency_key` — all three are required,
// non-`Option` fields on the real struct (added in an earlier revision
// for submission idempotency; see `omega-core::types::blueprint`'s own
// module doc comment), so this was a plain compile error
// (`error[E0063]: missing field ...`) unrelated to anything this file's
// own logic does. `HotPathGate::admit` itself never reads any of the
// three — admission is purely canary/strategy/lane/gas/profit/capacity —
// so this is exactly and only a test-construction fix, not a behavior
// change. `signal_id` is generated the same way every other test
// blueprint in this workspace generates one (`Uuid::from_bytes`), and
// `client_order_id`/`idempotency_key` are derived/computed the same way
// `ExecutionBlueprint`'s own doc comments specify for every other
// legitimate construction site.
//
// ## Audit fix (this revision, 2): test helper missing flashloan
// provider/token + max_base_fee_gwei fields
//
// `omega-core` added four more required fields to `ExecutionBlueprint`
// (`flashloan_provider_type`, `provider_contract`, `flashloan_token`,
// `max_base_fee_gwei`) to support real flashloan provider/pool selection
// — see that crate's `types::blueprint` module doc comment. `HotPathGate::
// admit` reads none of them (same reasoning as the fix above: admission
// is canary/strategy/lane/gas/profit/capacity only), so this is again a
// test-construction-only fix. SA/LA hot-path blueprints in this test file
// don't source a flashloan, so these are ZERO/placeholder values, same
// treatment as `provider_contract`/`flashloan_token` placeholders used
// elsewhere in this workspace's other test-only blueprint helpers.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use omega_core::errors::{DropCode, OmegaError};
use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
use omega_core::types::lane::Lane;

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum concurrent hot-path executions (§4 CPU-pinned slots).
pub const HOT_PATH_SLOTS: i64 = 4;

/// Maximum L2 gas units for a Microtx blueprint (§4).
pub const MICROTX_GAS_LIMIT: u64 = 200_000;

/// Maximum RPC reads per Microtx blueprint (§4).
pub const MICROTX_MAX_READS: u8 = 8;

// ─────────────────────────────────────────────────────────────────────────────
// AdmissionResult
// ─────────────────────────────────────────────────────────────────────────────

/// Returned by `HotPathGate::admit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionResult {
    /// Blueprint is admitted.  The caller must call `release()` after
    /// the blueprint finishes execution (success or failure).
    Admitted {
        /// Remaining RPC read budget for this blueprint.
        read_budget: u8,
    },
    /// Blueprint is dropped — carry the typed error for loss attribution.
    Dropped(OmegaError),
}

// ─────────────────────────────────────────────────────────────────────────────
// HotPathGate
// ─────────────────────────────────────────────────────────────────────────────

/// Admission gate for the CPU-pinned hot-path execution lane.
///
/// `HotPathGate` is `Clone` — all clones share the same atomic slot
/// counter so the budget is global across all tasks.
#[derive(Clone, Debug)]
pub struct HotPathGate {
    /// Available slots.  Starts at `HOT_PATH_SLOTS` and is decremented
    /// on each `admit` (when successful) and incremented on `release`.
    available: Arc<AtomicI64>,
}

impl HotPathGate {
    /// Create a new gate with `HOT_PATH_SLOTS` available slots.
    pub fn new() -> Self {
        Self {
            available: Arc::new(AtomicI64::new(HOT_PATH_SLOTS)),
        }
    }

    /// Attempt to admit a blueprint to the hot path.
    ///
    /// Enforces all §4 constraints in order:
    ///   1. `is_canary` guard — CNRY never enters the hot path.
    ///   2. `hot_path_eligible` guard — only SA and LA hot-tier.
    ///   3. Lane guard — must be `Lane::Microtx`.
    ///   4. Gas budget guard — must be < MICROTX_GAS_LIMIT.
    ///   5. Profitability guard — `is_profitable()` from blueprint.
    ///   6. Slot budget — capacity check (CAS decrement).
    pub fn admit(&self, bp: &ExecutionBlueprint) -> AdmissionResult {
        // 1. Canary guard — absolute block
        if bp.is_canary() {
            tracing::debug!(
                blueprint_hash = %bp.blueprint_hash,
                "Hot-path gate: CNRY blueprint rejected",
            );
            return AdmissionResult::Dropped(OmegaError::dropped(DropCode::MissWhitelist));
        }

        // 2. Strategy guard — only SA and LA belong on the hot path.
        if !matches!(bp.strategy_id, StrategyId::Sa | StrategyId::La) {
            tracing::debug!(
                blueprint_hash = %bp.blueprint_hash,
                strategy_id    = %bp.strategy_id,
                "Hot-path gate: non-hot-path strategy rejected",
            );
            return AdmissionResult::Dropped(OmegaError::dropped(DropCode::MissCapacity));
        }

        // 3. Lane guard
        if bp.lane != Lane::Microtx {
            tracing::debug!(
                blueprint_hash = %bp.blueprint_hash,
                lane           = ?bp.lane,
                "Hot-path gate: non-Microtx lane rejected",
            );
            return AdmissionResult::Dropped(OmegaError::dropped(DropCode::MissCapacity));
        }

        // 4. Gas budget guard
        if bp.l2_exec_gas_estimate >= MICROTX_GAS_LIMIT {
            tracing::debug!(
                blueprint_hash = %bp.blueprint_hash,
                gas            = bp.l2_exec_gas_estimate,
                limit          = MICROTX_GAS_LIMIT,
                "Hot-path gate: gas budget exceeded",
            );
            return AdmissionResult::Dropped(OmegaError::dropped(DropCode::MissGas));
        }

        // 5. Profitability guard
        if !bp.is_profitable() {
            tracing::debug!(
                blueprint_hash  = %bp.blueprint_hash,
                profit          = %bp.expected_profit_net,
                min_profit      = %bp.dynamic_min_profit,
                "Hot-path gate: blueprint unprofitable",
            );
            return AdmissionResult::Dropped(OmegaError::dropped(DropCode::MissProfit));
        }

        // 6. Slot budget — CAS decrement
        // Fetch-then-decrement: if the fetched value is ≤ 0, the slot
        // is not available and we must not proceed.
        let prev = self.available.fetch_sub(1, Ordering::AcqRel);
        if prev <= 0 {
            // Undo the decrement so the counter stays accurate
            self.available.fetch_add(1, Ordering::AcqRel);
            tracing::debug!(
                blueprint_hash = %bp.blueprint_hash,
                "Hot-path gate: no slots available (MissCapacity)",
            );
            return AdmissionResult::Dropped(OmegaError::dropped(DropCode::MissCapacity));
        }

        tracing::debug!(
            blueprint_hash = %bp.blueprint_hash,
            strategy_id    = %bp.strategy_id,
            gas            = bp.l2_exec_gas_estimate,
            remaining_slots = prev - 1,
            "Hot-path gate: admitted",
        );

        AdmissionResult::Admitted {
            read_budget: MICROTX_MAX_READS,
        }
    }

    /// Release a previously admitted slot.
    ///
    /// Must be called exactly once per successful `admit` (whether the
    /// blueprint succeeded, was dropped mid-execution, or expired).
    /// Double-release is safe (the counter is bounded below by the
    /// capacity guard in `admit`) but indicates a logic error and is
    /// logged at WARN.
    pub fn release(&self) {
        let prev = self.available.fetch_add(1, Ordering::AcqRel);
        if prev >= HOT_PATH_SLOTS {
            tracing::warn!(
                counter = prev + 1,
                max = HOT_PATH_SLOTS,
                "Hot-path gate: release called with no matching admit (counter exceeds capacity)",
            );
        }
    }

    /// Current number of available slots.
    pub fn available_slots(&self) -> i64 {
        self.available.load(Ordering::Acquire).max(0)
    }

    /// Current number of occupied slots.
    pub fn occupied_slots(&self) -> i64 {
        (HOT_PATH_SLOTS - self.available.load(Ordering::Acquire)).max(0)
    }
}

impl Default for HotPathGate {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256, U256};
    use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
    use omega_core::types::flashloan_provider::FlashloanProviderType;
    use omega_core::types::lane::{Lane, Simulator};
    use uuid::Uuid;

    fn make_bp(strategy: StrategyId, lane: Lane, gas: u64, profit: u128) -> ExecutionBlueprint {
        let signal_id = Uuid::from_bytes([7u8; 16]);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(strategy, 42161, 0, signal_id);
        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::from([1u8; 32]),
            chain_id: 42161,
            strategy_id: strategy,
            lane,
            simulator: Simulator::Revm,
            signal_state_hash: B256::ZERO,
            state_version: 1,
            signal_id,
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::ZERO,
            flashloan_available: U256::ZERO,
            // None of these test blueprints source a real flashloan
            // (flashloan_provider is the legacy ZERO sentinel above);
            // HotPathGate::admit never reads any of the four fields
            // below, so these are placeholders — see this file's audit
            // note.
            flashloan_provider_type: FlashloanProviderType::Balancer,
            provider_contract: Address::ZERO,
            flashloan_token: Address::ZERO,
            calldata: Default::default(),
            strategy_bytecode_hash: B256::ZERO,
            l2_exec_gas_estimate: gas,
            l1_data_gas_estimate: 0,
            extraction_gas: 21_000,
            expected_profit_net: U256::from(profit),
            dynamic_min_profit: U256::from(100_000_u64),
            l2_buffer_factor: 1.15,
            l1_data_buffer_factor: 1.10,
            slippage_bps: 100,
            base_fee_at_creation: 10,
            l1_data_fee_at_creation: 2,
            priority_fee_gwei: 10,
            max_base_fee_gwei: ExecutionBlueprint::derive_max_base_fee_gwei(10, 3.0),
            price_impact_bps: None,
            ofa_compliant: false,
            expiry_block: 2_000_000,
            nonce: 0,
            confirmation_depth: 12,
            client_order_id,
            idempotency_key: B256::ZERO, // placeholder; overwritten below
            relay_targets: vec!["relay_a".into()],
            zk_proof_commitment: None,
        };
        // HotPathGate::admit never calls verify_hash()/verify_idempotency_key(),
        // so these tests don't strictly need real values here — but computing
        // them for real (rather than leaving B256::ZERO) costs nothing and
        // means this helper produces a genuinely well-formed blueprint,
        // consistent with every other test helper in this workspace that
        // constructs one.
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();
        bp
    }

    fn valid_bp() -> ExecutionBlueprint {
        make_bp(StrategyId::Sa, Lane::Microtx, 100_000, 1_000_000)
    }

    // ── Admission ─────────────────────────────────────────────────────────

    #[test]
    fn valid_sa_microtx_is_admitted() {
        let gate = HotPathGate::new();
        assert!(matches!(
            gate.admit(&valid_bp()),
            AdmissionResult::Admitted { .. }
        ));
    }

    #[test]
    fn la_hot_tier_is_admitted() {
        let gate = HotPathGate::new();
        let bp = make_bp(StrategyId::La, Lane::Microtx, 150_000, 2_000_000);
        assert!(matches!(gate.admit(&bp), AdmissionResult::Admitted { .. }));
    }

    #[test]
    fn read_budget_is_microtx_max_reads() {
        let gate = HotPathGate::new();
        match gate.admit(&valid_bp()) {
            AdmissionResult::Admitted { read_budget } => {
                assert_eq!(read_budget, MICROTX_MAX_READS);
            }
            other => panic!("expected Admitted, got {other:?}"),
        }
    }

    // ── Rejection ─────────────────────────────────────────────────────────

    #[test]
    fn canary_is_rejected_with_miss_whitelist() {
        let gate = HotPathGate::new();
        let bp = make_bp(StrategyId::Cnry, Lane::Microtx, 50_000, 1_000_000);
        match gate.admit(&bp) {
            AdmissionResult::Dropped(e) => {
                assert_eq!(e.drop_code(), Some(DropCode::MissWhitelist));
            }
            other => panic!("expected Dropped, got {other:?}"),
        }
    }

    #[test]
    fn normal_lane_is_rejected() {
        let gate = HotPathGate::new();
        let bp = make_bp(StrategyId::Sa, Lane::Normal, 100_000, 1_000_000);
        match gate.admit(&bp) {
            AdmissionResult::Dropped(e) => {
                assert_eq!(e.drop_code(), Some(DropCode::MissCapacity));
            }
            other => panic!("expected Dropped, got {other:?}"),
        }
    }

    #[test]
    fn msa_strategy_is_rejected() {
        let gate = HotPathGate::new();
        let bp = make_bp(StrategyId::Msa, Lane::Microtx, 100_000, 1_000_000);
        match gate.admit(&bp) {
            AdmissionResult::Dropped(e) => {
                assert_eq!(e.drop_code(), Some(DropCode::MissCapacity));
            }
            other => panic!("expected Dropped, got {other:?}"),
        }
    }

    #[test]
    fn gas_over_limit_is_rejected() {
        let gate = HotPathGate::new();
        let bp = make_bp(StrategyId::Sa, Lane::Microtx, MICROTX_GAS_LIMIT, 1_000_000);
        match gate.admit(&bp) {
            AdmissionResult::Dropped(e) => {
                assert_eq!(e.drop_code(), Some(DropCode::MissGas));
            }
            other => panic!("expected Dropped, got {other:?}"),
        }
    }

    #[test]
    fn below_min_profit_is_rejected() {
        let gate = HotPathGate::new();
        // profit (50) < min_profit (100_000)
        let bp = make_bp(StrategyId::Sa, Lane::Microtx, 100_000, 50);
        match gate.admit(&bp) {
            AdmissionResult::Dropped(e) => {
                assert_eq!(e.drop_code(), Some(DropCode::MissProfit));
            }
            other => panic!("expected Dropped, got {other:?}"),
        }
    }

    // ── Slot accounting ───────────────────────────────────────────────────

    #[test]
    fn slots_decrease_on_admit_increase_on_release() {
        let gate = HotPathGate::new();
        assert_eq!(gate.available_slots(), HOT_PATH_SLOTS);
        gate.admit(&valid_bp());
        assert_eq!(gate.available_slots(), HOT_PATH_SLOTS - 1);
        gate.release();
        assert_eq!(gate.available_slots(), HOT_PATH_SLOTS);
    }

    #[test]
    fn capacity_full_rejects_with_miss_capacity() {
        let gate = HotPathGate::new();
        // Fill all slots
        for _ in 0..HOT_PATH_SLOTS {
            gate.admit(&valid_bp());
        }
        assert_eq!(gate.available_slots(), 0);

        // Next admit must be rejected
        match gate.admit(&valid_bp()) {
            AdmissionResult::Dropped(e) => {
                assert_eq!(e.drop_code(), Some(DropCode::MissCapacity));
            }
            other => panic!("expected Dropped(MissCapacity), got {other:?}"),
        }
    }

    #[test]
    fn release_restores_slot_after_full() {
        let gate = HotPathGate::new();
        for _ in 0..HOT_PATH_SLOTS {
            gate.admit(&valid_bp());
        }
        gate.release();
        // Should be admissible again
        assert!(matches!(
            gate.admit(&valid_bp()),
            AdmissionResult::Admitted { .. }
        ));
    }

    #[test]
    fn capacity_rejected_does_not_consume_slot() {
        let gate = HotPathGate::new();
        // Fill to capacity
        for _ in 0..HOT_PATH_SLOTS {
            gate.admit(&valid_bp());
        }
        let before = gate.available_slots();
        gate.admit(&valid_bp()); // rejected
        assert_eq!(
            gate.available_slots(),
            before,
            "rejected admission must not decrement slot counter"
        );
    }

    #[test]
    fn clone_shares_slot_counter() {
        let gate_a = HotPathGate::new();
        let gate_b = gate_a.clone();
        gate_a.admit(&valid_bp());
        assert_eq!(
            gate_b.available_slots(),
            HOT_PATH_SLOTS - 1,
            "clone must share the atomic slot counter"
        );
    }
}

'@
Set-Content -Path 'crates\omega-hot-path\src\gate.rs' -Value $content_2 -Encoding UTF8 -NoNewline

Write-Host 'Writing crates\omega-hot-path\src\simulator.rs...'
$content_3 = @'
// crates/omega-hot-path/src/simulator.rs
//
// MicrotxSimulator — in-process revm execution for the hot path (§4).
//
// ## Spec §4 constraints
//
//   Target latency: <1ms per blueprint.
//   Simulator: revm (in-process, zero-copy).
//   Max gas: < 200,000 per blueprint.
//   Max reads: 8 per blueprint (enforced by the read budget from HotPathGate).
//
// ## Simulation model
//
//   The simulator does not call the actual revm EVM — omega-hot-path has
//   no dependency on the `revm` crate (which requires omega-strategies).
//   Instead it implements the SimResult contract that callers (the
//   orchestrator, loss attribution) depend on:
//
//     - `profit_net`: derived from `blueprint.expected_profit_net`.
//     - `gas_used`:   derived from `blueprint.l2_exec_gas_estimate`.
//     - `simulator`:  always "revm".
//     - `success`:    `true` unless the gas estimate or profit checks fail.
//
//   In the full engine the orchestrator holds an `Arc<RevmCacheManager>`
//   from omega-strategies and calls into it; the hot-path crate exposes
//   the interface contract only.
//
// ## ZK commitment
//
//   For blueprints that require a ZK proof (§15), the simulator returns
//   a `SimResult` flagged with `requires_zk_commitment = true`.  The
//   orchestrator then routes through the ZK layer before relay submission.
//   Hot-path blueprints (SA, LA hot-tier) use T1 ZK which operates inline
//   and does not block the <1ms budget significantly.
//
// ## Audit fix (this revision): oracle freshness, price sanity, and
// slippage protection were entirely absent from this lane
//
// Before this revision, `simulate()` had zero dependency on oracle data
// and never read `bp.slippage_bps` at all. A blueprint could go
// admission → simulation → success on this sub-millisecond lane with a
// stale oracle, a non-sane or wildly-diverged price, or a slippage
// tolerance the system never configured for its strategy — entirely
// independent of whether `omega-execution::ExecutionPipeline`'s 16-check
// pipeline (`omega_risk::checks::run_all_checks`) would have caught the
// identical condition, because the hot path never goes through that
// pipeline at all. That pipeline is Stage 2c of `ExecutionPipeline::
// execute`, which this crate has no relationship to; this <1ms lane is a
// structurally separate execution route.
//
// Fixed by giving `simulate()` a mandatory `&OracleSnapshot` parameter
// and running four checks before any success can be reported:
//   - `omega_risk::checks::oracle_freshness_check` — all three oracle
//     feeds stale → reject (check 7's logic).
//   - `omega_risk::checks::oracle_hierarchy_check` — Chainlink and Pyth
//     both fresh but disagree beyond threshold → reject (check 8's logic).
//   - `omega_risk::checks::oracle_price_sanity_check` — a relied-upon
//     price is non-finite/non-positive, or the fresh spot price has
//     diverged too far from a fresh TWAP → reject (check 16's logic,
//     `DropCode::MissFlashCrash`).
//   - `omega_risk::checks::slippage_check` — `bp.slippage_bps` exceeds
//     the per-strategy configured maximum → reject (check 9's logic).
//
// These call the SAME `pub` functions `omega_risk::checks` exposes for
// exactly this purpose (see that crate's module doc comment) rather than
// a second, hot-path-local reimplementation — one source of truth for
// each threshold, callable from both execution routes.
//
// This makes `oracle` a required, non-optional parameter deliberately:
// there is no safe default for "I don't have live oracle data" other
// than failing every one of the checks above, which passing a
// synthetic/empty snapshot would not reliably do (e.g. a
// default-initialized snapshot might read as "fresh" with zero ages).
// The caller — whoever constructs a `HotPathRequest` — must assemble a
// live snapshot at request time, the same requirement
// `omega-execution::ExecutionPipeline::execute` already places on its
// `CheckContext` parameter.
//
// Placement: these four checks run after `Expired` (cheap, no
// allocation, already established the blueprint is still live) and
// before the read-budget/profit calculation that follows — rejecting
// stale/unsafe market data before doing any further work on it.
//
// Not touched by this fix, left exactly as before: `metrics.record_miss()`
// is NOT called inside `simulate()` for the new checks, matching the
// existing pattern for `WrongSimulator`/`GasLimitExceeded`/
// `ReadBudgetExhausted` (which also don't call it here) — `HotPathRunner::
// run` calls `metrics.record_miss()` exactly once for every `Err` variant
// in its match. (`Expired` and `Unprofitable` DO call it here as well as
// in `lib.rs`, a pre-existing double-count inconsistency in this file
// unrelated to oracle/price/slippage — left alone as out of scope for
// this change.)
//
// ## Audit fix (this revision, 2): test helpers missing flashloan
// provider/token + max_base_fee_gwei fields
//
// `omega-core` added four more required fields to `ExecutionBlueprint`
// (`flashloan_provider_type`, `provider_contract`, `flashloan_token`,
// `max_base_fee_gwei`). `MicrotxSimulator::simulate` reads none of them —
// this crate's own flashloan feasibility handling (if any) lives
// elsewhere in the pipeline, not on this <1ms lane — so this is a
// test-construction-only fix, same category as the oracle/slippage test
// helper additions already in this file.

use std::time::Instant;

use alloy_primitives::U256;
use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
// Simulator is defined in omega_core::types::lane, not blueprint.
// blueprint re-exports it via `use` but does not make it pub from that path.
use omega_core::types::lane::Simulator;
use omega_core::types::strategy::SimResult;
use omega_risk::checks::{
    oracle_freshness_check, oracle_hierarchy_check, oracle_price_sanity_check, slippage_check,
};
use omega_risk::context::{
    OracleSnapshot, MAX_SLIPPAGE_BPS_LA, MAX_SLIPPAGE_BPS_MEV, MAX_SLIPPAGE_BPS_MSA,
    MAX_SLIPPAGE_BPS_SA,
};

use crate::gate::MICROTX_GAS_LIMIT;
use crate::metrics::HotPathMetrics;

// ─────────────────────────────────────────────────────────────────────────────
// SimulationError
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum SimulationError {
    /// Blueprint's simulator field is not revm — wrong path.
    #[error("Hot-path simulator requires Simulator::Revm; got {actual:?}")]
    WrongSimulator { actual: Simulator },

    /// Gas estimate exceeds the Microtx limit.
    #[error("Gas estimate {gas} ≥ MICROTX_GAS_LIMIT {limit}")]
    GasLimitExceeded { gas: u64, limit: u64 },

    /// Blueprint already expired at the current block.
    #[error("Blueprint expired at block {expiry}; current block {current}")]
    Expired { expiry: u64, current: u64 },

    /// All oracle feeds are stale — see
    /// `omega_risk::checks::oracle_freshness_check`.
    #[error("Oracle data is stale: no fresh Chainlink, Pyth, or TWAP feed")]
    OracleStale,

    /// Chainlink and Pyth are both fresh but diverge beyond the
    /// configured threshold — see
    /// `omega_risk::checks::oracle_hierarchy_check`.
    #[error("Oracle feeds diverge beyond threshold: Chainlink and Pyth disagree")]
    OracleDiverged,

    /// An active oracle price is non-sane (non-finite or non-positive),
    /// or the fresh spot price has diverged too far from a fresh TWAP —
    /// see `omega_risk::checks::oracle_price_sanity_check`.
    #[error("Oracle price sanity check failed: non-sane price or spot/TWAP divergence")]
    PriceSanityViolation,

    /// The blueprint's requested slippage tolerance exceeds the
    /// configured maximum for its strategy — see
    /// `omega_risk::checks::slippage_check`.
    #[error("Slippage {slippage_bps} bps exceeds strategy max {max_bps} bps")]
    SlippageExceeded { slippage_bps: u16, max_bps: u16 },

    /// Simulation produced zero or negative profit (after gas deduction).
    #[error("Simulation produced unprofitable result: profit_net={profit_net}")]
    Unprofitable { profit_net: U256 },

    /// Read budget exhausted — callee tried to make more than 8 RPC reads.
    #[error("Read budget exhausted: {used} reads > {budget} budget")]
    ReadBudgetExhausted { used: u8, budget: u8 },
}

// ─────────────────────────────────────────────────────────────────────────────
// HotPathSimResult
// ─────────────────────────────────────────────────────────────────────────────

/// Extended simulation result for hot-path blueprints.
#[derive(Debug, Clone)]
pub struct HotPathSimResult {
    /// Core simulation result for loss attribution and relay submission.
    pub inner: SimResult,
    /// Wall-clock latency of the simulation in microseconds.
    pub latency_us: u64,
    /// Number of RPC reads consumed during simulation.
    pub reads_used: u8,
    /// Whether this blueprint requires a ZK commitment before relay (§15).
    pub requires_zk_commitment: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-strategy slippage cap selection
// ─────────────────────────────────────────────────────────────────────────────

/// Selects the configured slippage cap for a blueprint's strategy.
///
/// `HotPathGate::admit` only ever lets `StrategyId::Sa` and
/// `StrategyId::La` reach `simulate()` (§4 — canary, MSA, and MEV are all
/// rejected at admission). The `Cnry`/`Msa`/`Mev` arm below exists purely
/// as defense-in-depth so this function can never be asked to return "no
/// limit" — if `simulate()` is ever called directly for one of those
/// strategies (bypassing the gate — e.g. a future caller, or a test),
/// this applies the SMALLEST of all four configured per-strategy caps
/// rather than guessing which single one was intended.
fn strategy_max_slippage_bps(id: StrategyId) -> u16 {
    match id {
        StrategyId::Sa => MAX_SLIPPAGE_BPS_SA,
        StrategyId::La => MAX_SLIPPAGE_BPS_LA,
        StrategyId::Cnry | StrategyId::Msa | StrategyId::Mev => MAX_SLIPPAGE_BPS_SA
            .min(MAX_SLIPPAGE_BPS_LA)
            .min(MAX_SLIPPAGE_BPS_MSA)
            .min(MAX_SLIPPAGE_BPS_MEV),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// MicrotxSimulator
// ─────────────────────────────────────────────────────────────────────────────

/// In-process simulation executor for the Microtx hot path.
#[derive(Debug, Clone)]
pub struct MicrotxSimulator {
    current_block: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl MicrotxSimulator {
    pub fn new(initial_block: u64) -> Self {
        Self {
            current_block: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(initial_block)),
        }
    }

    pub fn set_block(&self, block: u64) {
        self.current_block
            .store(block, std::sync::atomic::Ordering::Release);
    }

    /// Simulate a Microtx blueprint.
    ///
    /// `oracle` must be a live snapshot assembled by the caller at
    /// request time — see this file's module-level audit note for why
    /// there is no safe default and why the four oracle/slippage checks
    /// below cannot be skipped.
    pub fn simulate(
        &self,
        bp: &ExecutionBlueprint,
        read_budget: u8,
        oracle: &OracleSnapshot,
        metrics: &HotPathMetrics,
    ) -> Result<HotPathSimResult, SimulationError> {
        let t0 = Instant::now();

        if bp.simulator != Simulator::Revm {
            return Err(SimulationError::WrongSimulator {
                actual: bp.simulator,
            });
        }

        if bp.l2_exec_gas_estimate >= MICROTX_GAS_LIMIT {
            return Err(SimulationError::GasLimitExceeded {
                gas: bp.l2_exec_gas_estimate,
                limit: MICROTX_GAS_LIMIT,
            });
        }

        let current = self
            .current_block
            .load(std::sync::atomic::Ordering::Acquire);
        if bp.is_expired(current) {
            metrics.record_miss();
            return Err(SimulationError::Expired {
                expiry: bp.expiry_block,
                current,
            });
        }

        // Oracle freshness / hierarchy / price sanity / slippage — see
        // this file's module-level audit note for why these four checks
        // exist here at all and why they must run unconditionally before
        // any success path.
        if oracle_freshness_check(oracle).is_some() {
            return Err(SimulationError::OracleStale);
        }
        if oracle_hierarchy_check(oracle).is_some() {
            return Err(SimulationError::OracleDiverged);
        }
        if oracle_price_sanity_check(oracle).is_some() {
            return Err(SimulationError::PriceSanityViolation);
        }
        let max_slippage_bps = strategy_max_slippage_bps(bp.strategy_id);
        if slippage_check(bp.slippage_bps, max_slippage_bps).is_some() {
            return Err(SimulationError::SlippageExceeded {
                slippage_bps: bp.slippage_bps,
                max_bps: max_slippage_bps,
            });
        }

        let reads_used: u8 = self.estimate_reads(bp).min(read_budget);

        if reads_used > read_budget {
            return Err(SimulationError::ReadBudgetExhausted {
                used: reads_used,
                budget: read_budget,
            });
        }

        let gas_used = (bp.l2_exec_gas_estimate as f64 * 0.90) as u64;
        let l2_cost_wei = gas_used as u128 * bp.base_fee_at_creation as u128 * 1_000_000_000;

        let profit_net = if bp.expected_profit_net > U256::from(l2_cost_wei) {
            bp.expected_profit_net - U256::from(l2_cost_wei)
        } else {
            U256::ZERO
        };

        if profit_net == U256::ZERO {
            metrics.record_miss();
            return Err(SimulationError::Unprofitable { profit_net });
        }

        let latency_us = t0.elapsed().as_micros() as u64;

        let result = HotPathSimResult {
            inner: SimResult {
                profit_net,
                gas_used,
                simulator: "revm".to_string(),
                success: true,
            },
            latency_us,
            reads_used,
            requires_zk_commitment: bp.zk_proof_commitment.is_some(),
        };

        metrics.record_success(latency_us, profit_net);

        tracing::debug!(
            blueprint_hash = %bp.blueprint_hash,
            latency_us,
            gas_used,
            profit_net     = %profit_net,
            reads_used,
            "MicrotxSimulator: simulation complete",
        );

        Ok(result)
    }

    fn estimate_reads(&self, bp: &ExecutionBlueprint) -> u8 {
        let fraction = bp.l2_exec_gas_estimate as f64 / MICROTX_GAS_LIMIT as f64;
        (fraction * 8.0).ceil() as u8
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
    use omega_core::types::flashloan_provider::FlashloanProviderType;
    use omega_core::types::lane::Lane;
    use uuid::Uuid;

    use crate::metrics::HotPathMetrics;

    fn make_bp(gas: u64, profit: u128, expiry: u64, sim: Simulator) -> ExecutionBlueprint {
        make_bp_with_slippage(gas, profit, expiry, sim, StrategyId::Sa, 20)
    }

    /// Full constructor allowing strategy and slippage to vary, needed by
    /// the new slippage tests below. `slippage_bps` defaults to 20 in
    /// `make_bp` — comfortably under `MAX_SLIPPAGE_BPS_SA` (30) — since a
    /// hardcoded 100 (the value this file previously used before slippage
    /// was actually enforced) would now fail every test's slippage check.
    ///
    /// `flashloan_provider_type`/`provider_contract`/`flashloan_token`/
    /// `max_base_fee_gwei`: none of these blueprints source a real
    /// flashloan and `MicrotxSimulator::simulate` reads none of the four
    /// — see this file's audit note — so these are ZERO/placeholder
    /// values, same treatment as `idempotency_key` below.
    fn make_bp_with_slippage(
        gas: u64,
        profit: u128,
        expiry: u64,
        sim: Simulator,
        strategy_id: StrategyId,
        slippage_bps: u16,
    ) -> ExecutionBlueprint {
        let signal_id = Uuid::from_bytes([3u8; 16]);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(strategy_id, 42161, 0, signal_id);
        ExecutionBlueprint {
            blueprint_hash: B256::from([2u8; 32]),
            chain_id: 42161,
            strategy_id,
            lane: Lane::Microtx,
            simulator: sim,
            signal_state_hash: B256::ZERO,
            state_version: 1,
            signal_id,
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::ZERO,
            flashloan_available: U256::ZERO,
            flashloan_provider_type: FlashloanProviderType::Balancer,
            provider_contract: Address::ZERO,
            flashloan_token: Address::ZERO,
            calldata: Bytes::default(),
            strategy_bytecode_hash: B256::ZERO,
            l2_exec_gas_estimate: gas,
            l1_data_gas_estimate: 0,
            extraction_gas: 21_000,
            expected_profit_net: U256::from(profit),
            dynamic_min_profit: U256::from(100_000_u64),
            l2_buffer_factor: 1.15,
            l1_data_buffer_factor: 1.10,
            slippage_bps,
            base_fee_at_creation: 10,
            l1_data_fee_at_creation: 2,
            priority_fee_gwei: 10,
            max_base_fee_gwei: ExecutionBlueprint::derive_max_base_fee_gwei(10, 3.0),
            price_impact_bps: None,
            ofa_compliant: false,
            expiry_block: expiry,
            nonce: 0,
            confirmation_depth: 12,
            client_order_id,
            idempotency_key: B256::ZERO,
            relay_targets: vec!["relay_a".into()],
            zk_proof_commitment: None,
        }
    }

    fn sim() -> MicrotxSimulator {
        MicrotxSimulator::new(1_000_000)
    }
    fn metrics() -> HotPathMetrics {
        HotPathMetrics::new()
    }

    /// A live, sane, mutually-consistent oracle snapshot — every check
    /// this file adds should pass against this.
    fn passing_oracle() -> OracleSnapshot {
        OracleSnapshot {
            chainlink_price: 2000.0,
            pyth_price: 2001.0, // ~0.05% divergence from Chainlink — within threshold
            twap_price: 1999.0, // ~0.05% divergence from spot — well within flash-crash threshold
            chainlink_age_s: 10,
            pyth_age_s: 10,
            twap_age_s: 60,
        }
    }

    /// All three feeds stale — must fail `oracle_freshness_check`.
    fn stale_oracle() -> OracleSnapshot {
        OracleSnapshot {
            chainlink_price: 2000.0,
            pyth_price: 2001.0,
            twap_price: 1999.0,
            chainlink_age_s: 100, // > 45s
            pyth_age_s: 100,      // > 45s
            twap_age_s: 200,      // > 120s
        }
    }

    #[test]
    fn successful_simulation_returns_revm_simulator() {
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let r = sim()
            .simulate(&bp, 8, &passing_oracle(), &metrics())
            .unwrap();
        assert_eq!(r.inner.simulator, "revm");
        assert!(r.inner.success);
        assert!(r.inner.gas_used > 0 && r.inner.gas_used < 100_000);
    }

    #[test]
    fn latency_is_recorded() {
        let bp = make_bp(
            50_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let r = sim()
            .simulate(&bp, 8, &passing_oracle(), &metrics())
            .unwrap();
        assert!(r.latency_us < 100_000);
    }

    #[test]
    fn wrong_simulator_returns_error() {
        let bp = make_bp(100_000, 10_000_000, 2_000_000, Simulator::Anvil);
        let err = sim()
            .simulate(&bp, 8, &passing_oracle(), &metrics())
            .unwrap_err();
        assert!(matches!(err, SimulationError::WrongSimulator { .. }));
    }

    #[test]
    fn expired_blueprint_returns_error() {
        let s = MicrotxSimulator::new(2_000_001);
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let e = s
            .simulate(&bp, 8, &passing_oracle(), &metrics())
            .unwrap_err();
        assert!(matches!(e, SimulationError::Expired { .. }));
    }

    #[test]
    fn gas_over_limit_returns_error() {
        let bp = make_bp(MICROTX_GAS_LIMIT, 10_000_000, 2_000_000, Simulator::Revm);
        let err = sim()
            .simulate(&bp, 8, &passing_oracle(), &metrics())
            .unwrap_err();
        assert!(matches!(err, SimulationError::GasLimitExceeded { .. }));
    }

    #[test]
    fn set_block_updates_expiry_check() {
        let s = sim();
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        assert!(s.simulate(&bp, 8, &passing_oracle(), &metrics()).is_ok());
        s.set_block(3_000_000);
        assert!(matches!(
            s.simulate(&bp, 8, &passing_oracle(), &metrics()),
            Err(SimulationError::Expired { .. })
        ));
    }

    // ── Oracle freshness ──────────────────────────────────────────────────

    #[test]
    fn stale_oracle_is_rejected() {
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let err = sim()
            .simulate(&bp, 8, &stale_oracle(), &metrics())
            .unwrap_err();
        assert!(matches!(err, SimulationError::OracleStale));
    }

    #[test]
    fn only_twap_fresh_is_not_stale_but_has_no_divergence_to_check() {
        // All three feeds fresh->stale combinations are exercised in
        // omega-risk's own test suite; this confirms the hot path reaches
        // the SAME conclusion for the identical snapshot shape (only TWAP
        // fresh), rather than diverging in behavior between the two
        // execution routes.
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let mut oracle = passing_oracle();
        oracle.chainlink_age_s = 100;
        oracle.pyth_age_s = 100;
        // twap_age_s stays fresh (60s)
        assert!(sim().simulate(&bp, 8, &oracle, &metrics()).is_ok());
    }

    // ── Oracle hierarchy (Chainlink vs Pyth divergence) ──────────────────

    #[test]
    fn chainlink_pyth_divergence_is_rejected() {
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let mut oracle = passing_oracle();
        oracle.chainlink_price = 2000.0;
        oracle.pyth_price = 2010.0; // 0.5% > 0.4% threshold
        let err = sim().simulate(&bp, 8, &oracle, &metrics()).unwrap_err();
        assert!(matches!(err, SimulationError::OracleDiverged));
    }

    // ── Price sanity / flash-crash guard ─────────────────────────────────

    #[test]
    fn non_positive_price_on_fresh_feed_is_rejected() {
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let mut oracle = passing_oracle();
        oracle.chainlink_price = 0.0;
        let err = sim().simulate(&bp, 8, &oracle, &metrics()).unwrap_err();
        assert!(matches!(err, SimulationError::PriceSanityViolation));
    }

    #[test]
    fn nan_price_on_fresh_feed_is_rejected() {
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let mut oracle = passing_oracle();
        oracle.twap_price = f64::NAN;
        let err = sim().simulate(&bp, 8, &oracle, &metrics()).unwrap_err();
        assert!(matches!(err, SimulationError::PriceSanityViolation));
    }

    #[test]
    fn spot_twap_flash_crash_divergence_is_rejected() {
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let mut oracle = passing_oracle();
        oracle.chainlink_price = 2000.0;
        oracle.pyth_price = 2000.0; // agree with chainlink, so check 8 passes
        oracle.twap_price = 1000.0; // 100% divergence from spot
        let err = sim().simulate(&bp, 8, &oracle, &metrics()).unwrap_err();
        assert!(matches!(err, SimulationError::PriceSanityViolation));
    }

    // ── Slippage ──────────────────────────────────────────────────────────

    #[test]
    fn slippage_over_strategy_max_is_rejected() {
        // SA's cap is MAX_SLIPPAGE_BPS_SA (30) — 50 exceeds it.
        let bp = make_bp_with_slippage(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
            StrategyId::Sa,
            50,
        );
        let err = sim()
            .simulate(&bp, 8, &passing_oracle(), &metrics())
            .unwrap_err();
        assert!(matches!(
            err,
            SimulationError::SlippageExceeded {
                slippage_bps: 50,
                max_bps: MAX_SLIPPAGE_BPS_SA
            }
        ));
    }

    #[test]
    fn slippage_exactly_at_strategy_max_passes() {
        let bp = make_bp_with_slippage(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
            StrategyId::Sa,
            MAX_SLIPPAGE_BPS_SA,
        );
        assert!(sim()
            .simulate(&bp, 8, &passing_oracle(), &metrics())
            .is_ok());
    }

    #[test]
    fn la_strategy_uses_la_slippage_cap_not_sa() {
        // LA's cap (MAX_SLIPPAGE_BPS_LA = 100) is looser than SA's (30) —
        // a slippage_bps of 60 must pass for LA even though it would fail
        // for SA, proving the strategy-specific cap is actually selected
        // and not hardcoded to SA's.
        let bp = make_bp_with_slippage(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
            StrategyId::La,
            60,
        );
        assert!(sim()
            .simulate(&bp, 8, &passing_oracle(), &metrics())
            .is_ok());
    }

    // ── Concurrency: cannot be bypassed under concurrent execution ──────

    #[test]
    fn concurrent_calls_with_stale_oracle_all_rejected() {
        let s = std::sync::Arc::new(sim());
        let bp = std::sync::Arc::new(make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        ));
        let oracle = std::sync::Arc::new(stale_oracle());

        let mut handles = Vec::new();
        for _ in 0..50 {
            let s = std::sync::Arc::clone(&s);
            let bp = std::sync::Arc::clone(&bp);
            let oracle = std::sync::Arc::clone(&oracle);
            handles.push(std::thread::spawn(move || {
                matches!(
                    s.simulate(&bp, 8, &oracle, &metrics()),
                    Err(SimulationError::OracleStale)
                )
            }));
        }

        let mut all_rejected = true;
        for h in handles {
            all_rejected &= h.join().expect("test thread must not panic");
        }
        assert!(
            all_rejected,
            "every concurrent call against a stale oracle must be rejected — none may slip through"
        );
    }

    #[test]
    fn concurrent_calls_with_sane_oracle_all_succeed() {
        // Control test for the one above: proves the concurrency
        // harness itself isn't what's causing rejections — a sane,
        // fresh, mutually-consistent oracle snapshot must let every
        // concurrent call through.
        let s = std::sync::Arc::new(sim());
        let bp = std::sync::Arc::new(make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        ));
        let oracle = std::sync::Arc::new(passing_oracle());

        let mut handles = Vec::new();
        for _ in 0..50 {
            let s = std::sync::Arc::clone(&s);
            let bp = std::sync::Arc::clone(&bp);
            let oracle = std::sync::Arc::clone(&oracle);
            handles.push(std::thread::spawn(move || {
                s.simulate(&bp, 8, &oracle, &metrics()).is_ok()
            }));
        }

        let mut all_ok = true;
        for h in handles {
            all_ok &= h.join().expect("test thread must not panic");
        }
        assert!(
            all_ok,
            "every concurrent call against a sane oracle must succeed"
        );
    }
}

'@
Set-Content -Path 'crates\omega-hot-path\src\simulator.rs' -Value $content_3 -Encoding UTF8 -NoNewline

Write-Host 'Writing crates\omega-hot-path\src\lib.rs...'
$content_4 = @'
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
//
// ## Audit fix (this revision): test helper missing flashloan
// provider/token + max_base_fee_gwei fields
//
// `omega-core` added four more required fields to `ExecutionBlueprint`
// (`flashloan_provider_type`, `provider_contract`, `flashloan_token`,
// `max_base_fee_gwei`). Nothing in `HotPathRunner::run` or anything it
// calls reads these — same reasoning as gate.rs's and simulator.rs's own
// audit notes — so `tests::make_bp` below is a test-construction-only
// fix, same category as the pre-existing `signal_id`/`client_order_id`/
// `idempotency_key` fix noted in that helper's own doc comment.

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
    use omega_core::types::flashloan_provider::FlashloanProviderType;
    use omega_core::types::lane::{Lane, Simulator};
    use uuid::Uuid;

    fn make_bp(strategy: StrategyId, gas: u64, hash_byte: u8) -> ExecutionBlueprint {
        let mut hash = B256::ZERO;
        hash.0[0] = hash_byte;
        // signal_id/client_order_id/idempotency_key: these hot-path gate
        // tests exercise admission/simulation logic only, never
        // verify_hash()/verify_idempotency_key() — same placeholder
        // caveat as omega-dag's test helper. flashloan_provider_type/
        // provider_contract/flashloan_token/max_base_fee_gwei: none of
        // these blueprints source a real flashloan and
        // `HotPathRunner::run` reads none of the four — see this file's
        // audit note — so these are ZERO/placeholder values too.
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
            flashloan_provider_type: FlashloanProviderType::Balancer,
            provider_contract: Address::ZERO,
            flashloan_token: Address::ZERO,
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
            max_base_fee_gwei: ExecutionBlueprint::derive_max_base_fee_gwei(10, 3.0),
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

'@
Set-Content -Path 'crates\omega-hot-path\src\lib.rs' -Value $content_4 -Encoding UTF8 -NoNewline

Write-Host ''
Write-Host 'Verifying...'
$check = Select-String -Path 'crates\omega-dag\src\tests.rs' -Pattern 'flashloan_provider_type' -Quiet
if ($check) { Write-Host '  OK: crates\omega-dag\src\tests.rs' } else { Write-Host '  MISSING: crates\omega-dag\src\tests.rs' -ForegroundColor Red }
$check = Select-String -Path 'crates\omega-strategies\src\cnry.rs' -Pattern 'flashloan_provider_type' -Quiet
if ($check) { Write-Host '  OK: crates\omega-strategies\src\cnry.rs' } else { Write-Host '  MISSING: crates\omega-strategies\src\cnry.rs' -ForegroundColor Red }
$check = Select-String -Path 'crates\omega-hot-path\src\gate.rs' -Pattern 'flashloan_provider_type' -Quiet
if ($check) { Write-Host '  OK: crates\omega-hot-path\src\gate.rs' } else { Write-Host '  MISSING: crates\omega-hot-path\src\gate.rs' -ForegroundColor Red }
$check = Select-String -Path 'crates\omega-hot-path\src\simulator.rs' -Pattern 'flashloan_provider_type' -Quiet
if ($check) { Write-Host '  OK: crates\omega-hot-path\src\simulator.rs' } else { Write-Host '  MISSING: crates\omega-hot-path\src\simulator.rs' -ForegroundColor Red }
$check = Select-String -Path 'crates\omega-hot-path\src\lib.rs' -Pattern 'flashloan_provider_type' -Quiet
if ($check) { Write-Host '  OK: crates\omega-hot-path\src\lib.rs' } else { Write-Host '  MISSING: crates\omega-hot-path\src\lib.rs' -ForegroundColor Red }