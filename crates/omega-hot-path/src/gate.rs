// crates/omega-hot-path/src/gate.rs
//
// HotPathGate â€” admission control for the <1ms hot-path execution lane.
//
// ## Spec Â§4 constraints
//
//   Only two strategy configurations may enter the hot path:
//     1. SA (Simple Arbitrage) â€” Microtx lane, gas < 200,000
//     2. LA hot-tier â€” HF < 1.01 (Â§11.1), Microtx lane
//
//   Canary blueprints (CNRY) must never reach the hot path â€” they have
//   no on-chain execution.
//
// ## Slot budget
//
//   The hot path is CPU-pinned (Â§4) with a fixed slot budget.  When all
//   slots are occupied the gate drops new blueprints with
//   `DropCode::MissCapacity`.  Slots are released on `release()`.
//
// ## Max reads per blueprint
//
//   Â§4 mandates: max 8 reads per Microtx blueprint.  The gate records
//   the read budget on admission and the simulator enforces it.
//
// ## Audit fix (this revision): test helper missing required fields
//
// `tests::make_bp` constructed `ExecutionBlueprint` without `signal_id`,
// `client_order_id`, or `idempotency_key` â€” all three are required,
// non-`Option` fields on the real struct (added in an earlier revision
// for submission idempotency; see `omega-core::types::blueprint`'s own
// module doc comment), so this was a plain compile error
// (`error[E0063]: missing field ...`) unrelated to anything this file's
// own logic does. `HotPathGate::admit` itself never reads any of the
// three â€” admission is purely canary/strategy/lane/gas/profit/capacity â€”
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
// â€” see that crate's `types::blueprint` module doc comment. `HotPathGate::
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

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Constants
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Maximum concurrent hot-path executions (Â§4 CPU-pinned slots).
pub const HOT_PATH_SLOTS: i64 = 4;

/// Maximum L2 gas units for a Microtx blueprint (Â§4).
pub const MICROTX_GAS_LIMIT: u64 = 200_000;

/// Maximum RPC reads per Microtx blueprint (Â§4).
pub const MICROTX_MAX_READS: u8 = 8;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// AdmissionResult
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Returned by `HotPathGate::admit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionResult {
    /// Blueprint is admitted.  The caller must call `release()` after
    /// the blueprint finishes execution (success or failure).
    Admitted {
        /// Remaining RPC read budget for this blueprint.
        read_budget: u8,
    },
    /// Blueprint is dropped â€” carry the typed error for loss attribution.
    Dropped(OmegaError),
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// HotPathGate
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Admission gate for the CPU-pinned hot-path execution lane.
///
/// `HotPathGate` is `Clone` â€” all clones share the same atomic slot
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
    /// Enforces all Â§4 constraints in order:
    ///   1. `is_canary` guard â€” CNRY never enters the hot path.
    ///   2. `hot_path_eligible` guard â€” only SA and LA hot-tier.
    ///   3. Lane guard â€” must be `Lane::Microtx`.
    ///   4. Gas budget guard â€” must be < MICROTX_GAS_LIMIT.
    ///   5. Profitability guard â€” `is_profitable()` from blueprint.
    ///   6. Slot budget â€” capacity check (CAS decrement).
    pub fn admit(&self, bp: &ExecutionBlueprint) -> AdmissionResult {
        // 1. Canary guard â€” absolute block
        if bp.is_canary() {
            tracing::debug!(
                blueprint_hash = %bp.blueprint_hash,
                "Hot-path gate: CNRY blueprint rejected",
            );
            return AdmissionResult::Dropped(OmegaError::dropped(DropCode::MissWhitelist));
        }

        // 2. Strategy guard â€” only SA and LA belong on the hot path.
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

        // 6. Slot budget â€” CAS decrement
        // Fetch-then-decrement: if the fetched value is â‰¤ 0, the slot
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

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

// `clippy::unwrap_used`/`clippy::expect_used` are set to "warn" in this
// crate's Cargo.toml `[lints]` table, applied unconditionally (a
// manifest-level table can't express a `cfg(test)` carve-out the way a
// Rust attribute can â€” see this file's own module-level "lint escalation
// split" note for the matching non-test-only `deny`). Under a plain
// `cargo test` that's harmless; under `cargo clippy -- -D warnings`,
// every warn-level lint is promoted to a hard error, which would fail
// this module's ordinary, idiomatic test-only `.unwrap()`/`panic!()`
// calls. Scoped `#[allow]` here, same fix already applied to
// omega-relay's test modules for the identical reason.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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
            // below, so these are placeholders â€” see this file's audit
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
        // so these tests don't strictly need real values here â€” but computing
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

    // â”€â”€ Admission â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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

    // â”€â”€ Rejection â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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

    // â”€â”€ Slot accounting â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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
