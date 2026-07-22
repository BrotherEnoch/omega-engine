// crates/omega-dag/src/tests.rs
//
// Test suite for omega-dag â€” extracted from scheduler.rs so that
// scheduler.rs stays under 600 lines while the test coverage is
// complete.
//
// Run with: cargo test -p omega-dag

use alloy_primitives::{Address, B256, U256};
use omega_core::errors::DropCode;
use omega_core::types::blueprint::{ExecutionBlueprint, Simulator, StrategyId};
use omega_core::types::lane::Lane;

use crate::scheduler::{DagConfig, DagError, ExecutionDag};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Helpers
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn test_config() -> DagConfig {
    DagConfig {
        microtx_slots: 4,
        normal_slots:  8,
    }
}

fn make_bp(hash_byte: u8, strategy: StrategyId, lane: Lane) -> ExecutionBlueprint {
    let mut hash = B256::ZERO;
    hash.0[0]    = hash_byte;
    ExecutionBlueprint {
        blueprint_hash:          hash,
        chain_id:                42161,
        strategy_id:             strategy,
        lane,
        simulator:               Simulator::Revm,
        signal_state_hash:       B256::ZERO,
        state_version:           1,
        flashloan_provider:      Address::ZERO,
        flashloan_amount:        U256::ZERO,
        flashloan_available:     U256::ZERO,
        calldata:                Default::default(),
        strategy_bytecode_hash:  B256::ZERO,
        l2_exec_gas_estimate:    21_000,
        l1_data_gas_estimate:    0,
        extraction_gas:          21_000,
        expected_profit_net:     U256::from(1_000_000_u64),
        dynamic_min_profit:      U256::from(100_000_u64),
        l2_buffer_factor:        1.15,
        l1_data_buffer_factor:   1.10,
        slippage_bps:            100,
        base_fee_at_creation:    10,
        l1_data_fee_at_creation: 2,
        priority_fee_gwei:       10,
        price_impact_bps:        None,
        ofa_compliant:           false,
        expiry_block:            1_001,
        nonce:                   0,
        confirmation_depth:      12,
        relay_targets:           vec!["relay_a".into()],
        zk_proof_commitment:     None,
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Admit / complete
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn admit_and_complete_basic() {
    let mut dag = ExecutionDag::new(test_config());
    let bp      = make_bp(1, StrategyId::Sa, Lane::Microtx);
    let hash    = bp.blueprint_hash;

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

    // c has two deps â€” not ready yet
    let ready = dag.ready();
    assert!(!ready.contains(&c_hash));

    // Complete a â€” c still blocked on b
    let after_a = dag.complete(a_hash);
    assert!(!after_a.contains(&c_hash));

    // Complete b â€” c now unblocked
    let after_b = dag.complete(b_hash);
    assert!(after_b.contains(&c_hash));
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Cycle detection
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn dependency_on_unknown_hash_returns_error() {
    let mut dag = ExecutionDag::new(test_config());
    let mut never_hash = B256::ZERO;
    never_hash.0[0]    = 0xFF;

    let d = make_bp(4, StrategyId::Msa, Lane::Normal);
    assert!(matches!(
        dag.admit(d, &[never_hash]),
        Err(DagError::DependencyNotFound { .. })
    ));
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

    // Linear chain aâ†’bâ†’c â€” no cycle
    assert_eq!(dag.node_count(), 3);
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Capacity
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn capacity_full_rejects_same_priority_blueprint() {
    let config  = DagConfig { microtx_slots: 2, normal_slots: 8 };
    let mut dag = ExecutionDag::new(config);

    dag.admit(make_bp(1, StrategyId::Sa, Lane::Microtx), &[]).unwrap();
    dag.admit(make_bp(2, StrategyId::Sa, Lane::Microtx), &[]).unwrap();
    // Third SA blueprint â€” no lower-priority candidate to evict (same prio)
    let result = dag.admit(make_bp(3, StrategyId::Sa, Lane::Microtx), &[]);
    assert!(matches!(result, Err(DagError::CapacityFull { .. })));
}

#[test]
fn higher_priority_evicts_lower() {
    let config  = DagConfig { microtx_slots: 1, normal_slots: 8 };
    let mut dag = ExecutionDag::new(config);

    // Fill Microtx with a low-priority SA blueprint
    let sa      = make_bp(1, StrategyId::Sa, Lane::Microtx);
    let sa_hash = sa.blueprint_hash;
    dag.admit(sa, &[]).unwrap();
    assert_eq!(dag.microtx_count(), 1);

    // Admit a high-priority LA blueprint â€” should evict SA
    let la = make_bp(2, StrategyId::La, Lane::Microtx);
    dag.admit(la, &[]).unwrap();

    // SA was evicted; LA is now the only node
    assert!(!dag.contains(&sa_hash));
    assert_eq!(dag.microtx_count(), 1);
    assert_eq!(dag.evictions().len(), 1);
}

#[test]
fn eviction_record_contains_correct_strategy_id() {
    let config  = DagConfig { microtx_slots: 1, normal_slots: 8 };
    let mut dag = ExecutionDag::new(config);

    dag.admit(make_bp(1, StrategyId::Sa, Lane::Microtx), &[]).unwrap();
    dag.admit(make_bp(2, StrategyId::La, Lane::Microtx), &[]).unwrap();

    let eviction = &dag.evictions()[0];
    assert_eq!(eviction.evicted_strategy, StrategyId::Sa);
    assert_eq!(eviction.admitted_strategy, StrategyId::La);
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// CNRY exemption
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn cnry_does_not_consume_slots() {
    let config  = DagConfig { microtx_slots: 1, normal_slots: 1 };
    let mut dag = ExecutionDag::new(config);

    // Fill both lanes
    dag.admit(make_bp(1, StrategyId::Sa,  Lane::Microtx), &[]).unwrap();
    dag.admit(make_bp(2, StrategyId::Msa, Lane::Normal),  &[]).unwrap();

    // CNRY should still be admissible in both lanes (no slot consumption)
    dag.admit(make_bp(3, StrategyId::Cnry, Lane::Microtx), &[]).unwrap();
    dag.admit(make_bp(4, StrategyId::Cnry, Lane::Normal),  &[]).unwrap();

    // Slot counts unchanged by CNRY
    assert_eq!(dag.microtx_count(), 1);
    assert_eq!(dag.normal_count(),  1);
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Duplicate rejection
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn duplicate_blueprint_rejected() {
    let mut dag = ExecutionDag::new(test_config());
    let bp   = make_bp(1, StrategyId::Sa, Lane::Microtx);
    let hash = bp.blueprint_hash;

    dag.admit(bp.clone(), &[]).unwrap();
    assert!(matches!(
        dag.admit(bp, &[]),
        Err(DagError::Duplicate { hash: h }) if h == hash
    ));
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Snapshot
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn snapshot_reflects_state() {
    let mut dag = ExecutionDag::new(test_config());
    dag.admit(make_bp(1, StrategyId::Sa,  Lane::Microtx), &[]).unwrap();
    dag.admit(make_bp(2, StrategyId::Msa, Lane::Normal),  &[]).unwrap();

    let snap = dag.snapshot();
    assert_eq!(snap.microtx_live, 1);
    assert_eq!(snap.normal_live,  1);
    assert_eq!(snap.total_nodes,  2);
    assert_eq!(snap.ready_count,  2); // no deps â†’ both ready
}

#[test]
fn snapshot_eviction_rate_after_eviction() {
    let config  = DagConfig { microtx_slots: 1, normal_slots: 8 };
    let mut dag = ExecutionDag::new(config);

    dag.admit(make_bp(1, StrategyId::Sa, Lane::Microtx), &[]).unwrap();
    dag.admit(make_bp(2, StrategyId::La, Lane::Microtx), &[]).unwrap();

    let snap = dag.snapshot();
    assert_eq!(snap.eviction_count, 1);
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// OmegaError mapping
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn cycle_error_maps_to_miss_dag_cycle() {
    let err = DagError::Cycle { hash: B256::ZERO };
    assert!(matches!(
        err.to_omega_error(),
        omega_core::errors::OmegaError::Dropped {
            code: DropCode::MissDagCycle
        }
    ));
}

#[test]
fn capacity_error_maps_to_miss_capacity() {
    use omega_core::types::lane::Lane;
    let err = DagError::CapacityFull { lane: Lane::Microtx };
    assert!(matches!(
        err.to_omega_error(),
        omega_core::errors::OmegaError::Dropped {
            code: DropCode::MissCapacity
        }
    ));
}

#[test]
fn normal_capacity_error_maps_to_miss_capacity_normal() {
    use omega_core::types::lane::Lane;
    let err = DagError::CapacityFull { lane: Lane::Normal };
    assert!(matches!(
        err.to_omega_error(),
        omega_core::errors::OmegaError::Dropped {
            code: DropCode::MissCapacityNormal
        }
    ));
}