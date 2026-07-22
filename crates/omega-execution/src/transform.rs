// crates/omega-execution/src/transform.rs
//
// ExecutionBlueprint -> BundlePayload transform (Stage 4, spec §8).
//
// Four of BundlePayload's five fields are NOT direct copies from
// ExecutionBlueprint — see the spec's §8 table. This module implements
// exactly what's resolvable with real, existing code:
//   - priority_fee_gwei: direct copy (unambiguous).
//   - block_number: derived from a live current_block parameter supplied
//     by the caller (never fabricated — this stage has no chain access
//     of its own).
//   - max_timestamp: derived from bp.expiry_block using Arbitrum's known
//     ~250ms block time (the same figure already referenced in
//     omega_relay::dedup's RESTART_WINDOW_BLOCKS doc comment: "60 blocks
//     ~= 15s on Arbitrum (250ms/block)") and a (current_block,
//     current_block_timestamp) reference pair supplied by the caller.
//   - txs / bundle_hash: require a signed transaction, which requires a
//     TransactionSigner (see signer.rs) — genuinely not available yet;
//     this module depends on the trait, not a fabricated implementation.

use omega_core::types::blueprint::ExecutionBlueprint;
use omega_relay::BundlePayload;

use crate::error::ExecutionError;
use crate::signer::{SignedTransaction, TransactionSigner};

/// Arbitrum One's approximate block time, in milliseconds. Used only to
/// derive `max_timestamp` from `bp.expiry_block` — an estimate, not a
/// consensus-critical value; `min_timestamp`/`max_timestamp` on
/// `BundlePayload` are documented (client.rs) as optional relay hints,
/// not on-chain-enforced constraints.
pub const ARBITRUM_BLOCK_TIME_MS: u64 = 250;

/// Build the `BundlePayload` for `bp`, signing it via `signer`.
///
/// `current_block` / `current_block_timestamp_secs` must be a real,
/// caller-supplied (block, unix-timestamp) reference pair — this
/// function has no chain access of its own and will not fabricate one.
pub async fn build_bundle_payload(
    bp: &ExecutionBlueprint,
    chain_id: u64,
    current_block: u64,
    current_block_timestamp_secs: u64,
    signer: &dyn TransactionSigner,
) -> Result<BundlePayload, ExecutionError> {
    let SignedTransaction { raw_tx_hex } = signer.sign_transaction(bp, chain_id).await?;

    // bundle_hash: keccak256 of the serialised (signed) bundle — computed
    // from the actual signed tx bytes, per client.rs's own doc comment,
    // NOT bp.blueprint_hash (a distinct, pre-signing content hash — see
    // ExecutionBlueprint's own doc comment on why the two must not be
    // conflated).
    let tx_bytes = hex::decode(raw_tx_hex.trim_start_matches("0x")).map_err(|e| {
        ExecutionError::SigningFailed { detail: format!("signed tx is not valid hex: {e}") }
    })?;
    let bundle_hash_bytes = omega_security::keccak256(&tx_bytes);
    let bundle_hash = format!("0x{}", hex::encode(bundle_hash_bytes));

    let blocks_until_expiry = bp.expiry_block.saturating_sub(current_block);
    let max_timestamp = current_block_timestamp_secs
        .saturating_add(blocks_until_expiry.saturating_mul(ARBITRUM_BLOCK_TIME_MS) / 1000);

    Ok(BundlePayload {
        bundle_hash,
        txs: vec![raw_tx_hex],
        block_number: format!("0x{current_block:x}"),
        min_timestamp: None,
        max_timestamp: Some(max_timestamp),
        priority_fee_gwei: bp.priority_fee_gwei,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signer::{MockTransactionSigner, UnconfiguredSigner};
    use alloy_primitives::{Address, Bytes, B256, U256};
    use omega_core::types::blueprint::StrategyId;
    use omega_core::types::lane::{Lane, Simulator};
    use uuid::Uuid;

    fn sample_bp() -> ExecutionBlueprint {
        let signal_id = Uuid::from_bytes([0x07u8; 16]);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(StrategyId::Sa, 42161, 0, signal_id);
        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::from([0x07u8; 32]),
            chain_id: 42161,
            strategy_id: StrategyId::Sa,
            lane: Lane::Microtx,
            simulator: Simulator::Revm,
            signal_state_hash: B256::ZERO,
            state_version: 1,
            signal_id,
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::ZERO,
            flashloan_available: U256::ZERO,
            calldata: Bytes::new(),
            strategy_bytecode_hash: B256::ZERO,
            l2_exec_gas_estimate: 100_000,
            l1_data_gas_estimate: 0,
            extraction_gas: 21_000,
            expected_profit_net: U256::from(1_000_000u64),
            dynamic_min_profit: U256::from(100_000u64),
            l2_buffer_factor: 1.15,
            l1_data_buffer_factor: 1.10,
            slippage_bps: 50,
            base_fee_at_creation: 10,
            l1_data_fee_at_creation: 2,
            priority_fee_gwei: 42,
            price_impact_bps: None,
            ofa_compliant: false,
            expiry_block: 1_100,
            nonce: 0,
            confirmation_depth: 12,
            client_order_id,
            idempotency_key: B256::ZERO,
            relay_targets: vec![],
            zk_proof_commitment: None,
        };
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();
        bp
    }

    #[tokio::test]
    async fn priority_fee_is_direct_copy() {
        let bp = sample_bp();
        let signer = MockTransactionSigner { should_fail: false };
        let payload = build_bundle_payload(&bp, 42161, 1_000, 1_700_000_000, &signer)
            .await
            .unwrap();
        assert_eq!(payload.priority_fee_gwei, 42);
    }

    #[tokio::test]
    async fn block_number_reflects_current_block() {
        let bp = sample_bp();
        let signer = MockTransactionSigner { should_fail: false };
        let payload = build_bundle_payload(&bp, 42161, 12345, 1_700_000_000, &signer)
            .await
            .unwrap();
        assert_eq!(payload.block_number, "0x3039");
    }

    #[tokio::test]
    async fn max_timestamp_derived_from_expiry_block() {
        let bp = sample_bp(); // expiry_block = 1_100
        let signer = MockTransactionSigner { should_fail: false };
        let payload = build_bundle_payload(&bp, 42161, 1_000, 1_700_000_000, &signer)
            .await
            .unwrap();
        // 100 blocks * 250ms = 25_000ms = 25s
        assert_eq!(payload.max_timestamp, Some(1_700_000_025));
    }

    #[tokio::test]
    async fn signer_failure_propagates() {
        let bp = sample_bp();
        let signer = MockTransactionSigner { should_fail: true };
        let result = build_bundle_payload(&bp, 42161, 1_000, 1_700_000_000, &signer).await;
        assert!(matches!(result, Err(ExecutionError::SigningFailed { .. })));
    }

    #[tokio::test]
    async fn unconfigured_signer_fails_loudly_not_silently() {
        let bp = sample_bp();
        let signer = UnconfiguredSigner;
        let result = build_bundle_payload(&bp, 42161, 1_000, 1_700_000_000, &signer).await;
        assert!(matches!(result, Err(ExecutionError::NoTransactionSigner)));
    }

    #[tokio::test]
    async fn bundle_hash_derived_from_signed_tx_not_blueprint_hash() {
        // Regression guard for the specific distinction the spec's §8 table
        // calls out: bundle_hash must NOT equal blueprint_hash.
        let bp = sample_bp();
        let signer = MockTransactionSigner { should_fail: false };
        let payload = build_bundle_payload(&bp, 42161, 1_000, 1_700_000_000, &signer)
            .await
            .unwrap();
        let blueprint_hash_hex = format!("0x{}", hex::encode(bp.blueprint_hash.as_slice()));
        assert_ne!(
            payload.bundle_hash, blueprint_hash_hex,
            "bundle_hash must be derived from the signed tx bytes, not copied from blueprint_hash"
        );
    }
}