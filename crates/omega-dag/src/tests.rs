// crates/omega-dag/src/tests.rs

use alloy_primitives::{Address, B256, U256};
use omega_core::errors::DropCode;
use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
use omega_core::types::flashloan_provider::FlashloanProviderType;
use omega_core::types::lane::{Lane, Simulator};
use uuid::Uuid;

use crate::scheduler::ExecutionDag;
use crate::types::{DagConfig, DagError};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Helpers
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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
    // verify_hash()/verify_idempotency_key(), so â€” same as
    // blueprint_hash above â€” these are deterministic placeholders keyed
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
        // since ExecutionBlueprint has no "none" discriminant for it â€”
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

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Admit / complete
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Cycle / dependency detection
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn dependency_on_unknown_hash_returns_cycle_error() {
    let mut dag = ExecutionDag::new(test_config());
    let mut bad_hash = B256::ZERO;
    bad_hash.0[0] = 0xFF;

    let d = make_bp(4, StrategyId::Msa, Lane::Normal);
    // scheduler maps DependencyNotFound â†’ Cycle(String)
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

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Capacity
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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

    // DagError::LaneFull â€” not CapacityFull (doesn't exist in types.rs)
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
    // evicted_strat is String, not StrategyId â€” verify it names SA
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

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// CNRY exemption
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Duplicate rejection
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[test]
fn duplicate_blueprint_rejected() {
    let mut dag = ExecutionDag::new(test_config());
    let bp = make_bp(1, StrategyId::Sa, Lane::Microtx);

    dag.admit(bp.clone(), &[]).unwrap();
    assert!(matches!(dag.admit(bp, &[]), Err(DagError::Cycle(_))));
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Snapshot â€” use actual DagSnapshot field names from types.rs
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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
    assert_eq!(dag.ready().len(), 2); // no deps â†’ both ready
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

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// OmegaError mapping
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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
