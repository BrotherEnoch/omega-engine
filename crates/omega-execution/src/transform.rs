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
//
// ## Nonce diagnostics (Document 7 / 2026-10-10):
//
// `build_bundle_payload` now decodes the EIP-1559 envelope nonce directly
// from the signed raw tx bytes and logs it alongside `bp.nonce` at INFO on
// every call. A WARN fires when the two differ (expected when the envelope
// nonce counter is seeded; a sign that `bp.nonce` is still being embedded
// in the envelope when they are always equal). The `nonce` field of the
// returned `BundlePayload` prefers the decoded envelope nonce so Stage-7
// metrics track the real on-chain nonce rather than the blueprint replay
// counter.
//
// ## Clippy fix (2026-10-13)
//
// `decode_rlp_list_payload` used `first >= 0xc0 && first <= 0xf7`, which
// trips `clippy::manual_range_contains` under `-D warnings`. Replaced with
// `(0xc0..=0xf7).contains(&first)`; behaviour is identical.
//
// ## Audit fix (earlier revision): test helper missing flashloan/fee fields
//
// `tests::sample_bp` predates `ExecutionBlueprint` gaining
// `flashloan_provider_type`, `provider_contract`, `flashloan_token`, and
// `max_base_fee_gwei`. Nothing in `build_bundle_payload` reads any of
// the three flashloan-identity fields (this stage only touches
// `priority_fee_gwei`, `expiry_block`, and hands the whole blueprint to
// the signer), so `Balancer`/`Address::ZERO`/`Address::ZERO` are inert
// placeholders consistent with this helper's existing
// `flashloan_provider: Address::ZERO` "no flashloan" convention;
// `max_base_fee_gwei` is derived via the real
// `ExecutionBlueprint::derive_max_base_fee_gwei` helper rather than a
// hand-picked literal.

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

// ── EIP-1559 nonce extraction ─────────────────────────────────────────────────
//
// Best-effort decoding of the envelope nonce from a signed raw transaction.
// All functions return `None` on any parse failure; they are used for
// logging and the `BundlePayload.nonce` field only — never on the
// fail-closed trade path.

/// Best-effort EIP-1559 (type 0x02) nonce extraction from a signed raw tx.
/// Returns None on any parse failure — logging only, never fail-open on trade path.
fn nonce_from_eip1559_raw_tx(raw_tx_hex: &str) -> Option<u64> {
    let hex = raw_tx_hex.trim().trim_start_matches("0x");
    let bytes = hex::decode(hex).ok()?;
    if bytes.first() != Some(&0x02) {
        return None;
    }
    // 0x02 || RLP([chain_id, nonce, ...])
    let rlp = &bytes[1..];
    // list header
    let (payload, _) = decode_rlp_list_payload(rlp)?;
    // first item = chain_id (skip), second = nonce
    let (rest, _) = skip_rlp_item(payload)?;
    let (nonce_bytes, _) = take_rlp_bytes(rest)?;
    rlp_bytes_to_u64(nonce_bytes)
}

fn decode_rlp_list_payload(input: &[u8]) -> Option<(&[u8], &[u8])> {
    let first = *input.first()?;
    if (0xc0..=0xf7).contains(&first) {
        let len = (first - 0xc0) as usize;
        if input.len() < 1 + len {
            return None;
        }
        Some((&input[1..1 + len], &input[1 + len..]))
    } else if first >= 0xf8 {
        let len_of_len = (first - 0xf7) as usize;
        if input.len() < 1 + len_of_len {
            return None;
        }
        let mut len = 0usize;
        for b in &input[1..1 + len_of_len] {
            len = (len << 8) | *b as usize;
        }
        if input.len() < 1 + len_of_len + len {
            return None;
        }
        Some((
            &input[1 + len_of_len..1 + len_of_len + len],
            &input[1 + len_of_len + len..],
        ))
    } else {
        None
    }
}

fn skip_rlp_item(input: &[u8]) -> Option<(&[u8], &[u8])> {
    let (_, rest) = take_rlp_bytes(input)?;
    Some((rest, rest))
}

fn take_rlp_bytes(input: &[u8]) -> Option<(&[u8], &[u8])> {
    let first = *input.first()?;
    if first <= 0x7f {
        Some((&input[0..1], &input[1..]))
    } else if first <= 0xb7 {
        let len = (first - 0x80) as usize;
        if input.len() < 1 + len {
            return None;
        }
        Some((&input[1..1 + len], &input[1 + len..]))
    } else if first <= 0xbf {
        let len_of_len = (first - 0xb7) as usize;
        if input.len() < 1 + len_of_len {
            return None;
        }
        let mut len = 0usize;
        for b in &input[1..1 + len_of_len] {
            len = (len << 8) | *b as usize;
        }
        if input.len() < 1 + len_of_len + len {
            return None;
        }
        Some((
            &input[1 + len_of_len..1 + len_of_len + len],
            &input[1 + len_of_len + len..],
        ))
    } else {
        // list items not expected at this position for nonce
        None
    }
}

fn rlp_bytes_to_u64(b: &[u8]) -> Option<u64> {
    if b.len() > 8 {
        return None;
    }
    let mut v = 0u64;
    for byte in b {
        v = (v << 8) | *byte as u64;
    }
    Some(v)
}

// ── Bundle payload construction ───────────────────────────────────────────────

/// Build the `BundlePayload` for `bp`, signing it via `signer`.
///
/// `current_block` / `current_block_timestamp_secs` must be a real,
/// caller-supplied (block, unix-timestamp) reference pair — this
/// function has no chain access of its own and will not fabricate one.
///
/// Logs a nonce diagnostic at INFO on every call so the relationship between
/// the blueprint replay counter (`bp.nonce`) and the EIP-1559 envelope nonce
/// (the gas-paying account's pending nonce) is visible in the run log. A WARN
/// fires when they differ, which is the EXPECTED state once the envelope nonce
/// counter is seeded; the two being always equal is the regression indicator
/// (signer still embedding `bp.nonce` in the envelope).
pub async fn build_bundle_payload(
    bp: &ExecutionBlueprint,
    chain_id: u64,
    current_block: u64,
    current_block_timestamp_secs: u64,
    signer: &dyn TransactionSigner,
) -> Result<BundlePayload, ExecutionError> {
    let SignedTransaction { raw_tx_hex } = signer.sign_transaction(bp, chain_id).await?;

    let raw_tx_nonce = nonce_from_eip1559_raw_tx(&raw_tx_hex);
    let strategy_id = bp.strategy_id.to_string();

    tracing::info!(
        strategy = %strategy_id,
        bp_nonce = bp.nonce,
        envelope_or_raw_nonce = ?raw_tx_nonce,
        bp_hash = %bp.blueprint_hash,
        chain_id,
        "sign→bundle: nonce diagnostic (bp_nonce vs raw_tx_nonce; must match envelope counter)"
    );

    if let Some(raw_n) = raw_tx_nonce {
        if raw_n != bp.nonce {
            tracing::warn!(
                strategy = %strategy_id,
                bp_nonce = bp.nonce,
                raw_tx_nonce = raw_n,
                bp_hash = %bp.blueprint_hash,
                "nonce mismatch: raw tx nonce != blueprint nonce — envelope counter is in use (expected when seeded)"
            );
        }
    }

    let tx_bytes = hex::decode(raw_tx_hex.trim_start_matches("0x")).map_err(|e| {
        ExecutionError::SigningFailed {
            detail: format!("signed tx is not valid hex: {e}"),
        }
    })?;
    let bundle_hash_bytes = omega_security::keccak256(&tx_bytes);
    let bundle_hash = format!("0x{}", hex::encode(bundle_hash_bytes));

    let blocks_until_expiry = bp.expiry_block.saturating_sub(current_block);
    let max_timestamp = current_block_timestamp_secs
        .saturating_add(blocks_until_expiry.saturating_mul(ARBITRUM_BLOCK_TIME_MS) / 1000);

    let expected_profit_net_wei: u128 = bp.expected_profit_net.try_into().unwrap_or(u128::MAX);
    let blueprint_hash = format!("0x{}", hex::encode(bp.blueprint_hash.as_slice()));

    // Prefer the nonce actually embedded in the signed envelope for Stage-7 / metrics.
    let payload_nonce = raw_tx_nonce.unwrap_or(bp.nonce);

    Ok(BundlePayload {
        bundle_hash,
        txs: vec![raw_tx_hex],
        block_number: format!("0x{current_block:x}"),
        min_timestamp: None,
        max_timestamp: Some(max_timestamp),
        priority_fee_gwei: bp.priority_fee_gwei,
        strategy_id,
        nonce: payload_nonce,
        expected_profit_net_wei,
        blueprint_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signer::{MockTransactionSigner, UnconfiguredSigner};
    use alloy_primitives::{Address, Bytes, B256, U256};
    use omega_core::types::blueprint::StrategyId;
    use omega_core::types::flashloan_provider::FlashloanProviderType;
    use omega_core::types::lane::{Lane, Simulator};
    use uuid::Uuid;

    fn sample_bp() -> ExecutionBlueprint {
        let signal_id = Uuid::from_bytes([0x07u8; 16]);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(StrategyId::Sa, 42161, 0, signal_id);
        let base_fee_at_creation = 10;
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
            // See this file's module-level "Audit fix: test helper
            // missing flashloan/fee fields" note: build_bundle_payload
            // never reads these three, so they mirror this helper's
            // existing flashloan_provider: Address::ZERO "no flashloan"
            // convention.
            flashloan_provider_type: FlashloanProviderType::Balancer,
            provider_contract: Address::ZERO,
            flashloan_token: Address::ZERO,
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
            base_fee_at_creation,
            l1_data_fee_at_creation: 2,
            priority_fee_gwei: 42,
            // Derived via the real ExecutionBlueprint helper — see this
            // file's module-level audit note.
            max_base_fee_gwei: ExecutionBlueprint::derive_max_base_fee_gwei(
                base_fee_at_creation,
                3.0,
            ),
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

    // ── Nonce diagnostic helpers ──────────────────────────────────────────────

    #[test]
    fn nonce_from_eip1559_raw_tx_returns_none_for_non_eip1559() {
        // A legacy (type 0x00) tx prefix — not 0x02, so must return None.
        assert!(nonce_from_eip1559_raw_tx("0xf8...").is_none());
        assert!(nonce_from_eip1559_raw_tx("0x").is_none());
        assert!(nonce_from_eip1559_raw_tx("").is_none());
    }

    #[test]
    fn nonce_from_eip1559_raw_tx_returns_none_for_garbage() {
        assert!(nonce_from_eip1559_raw_tx("0xdeadbeef").is_none());
        assert!(nonce_from_eip1559_raw_tx("not hex at all").is_none());
    }

    #[test]
    fn nonce_from_eip1559_raw_tx_decodes_known_mainnet_tx() {
        // Real mainnet EIP-1559 tx from the signer.rs golden vector: nonce = 86964 (0x153b4).
        // Prefix 0x02 then the RLP list. Chain 1, nonce 86964.
        let raw = "02f8b401830153b4843b9aca008507f3be98328301d4c094dac17f958d2ee523a2206206994597c13d831ec780b844a9059cbb000000000000000000000000622779096805724b38c42b51989ddca32d671a000000000000000000000000000000000000000000000000000000000022df0080c001a0236084da36000fb2c7373cfa78e8f1bc9d8eb081dc240630c8024aa06fc39f96a030bdc5cd4e1f5f6abbb36c3b004270b68724cc46c56ad5847c99f8ced9c4112d";
        let nonce = nonce_from_eip1559_raw_tx(raw);
        assert_eq!(nonce, Some(86964));
    }

    #[tokio::test]
    async fn payload_nonce_falls_back_to_bp_nonce_when_decode_fails() {
        // MockTransactionSigner returns a hex-encoded blueprint_hash (not a real
        // RLP tx), so nonce_from_eip1559_raw_tx will return None and the payload
        // nonce must fall back to bp.nonce.
        let mut bp = sample_bp();
        bp.nonce = 7;
        let signer = MockTransactionSigner { should_fail: false };
        let payload = build_bundle_payload(&bp, 42161, 1_000, 1_700_000_000, &signer)
            .await
            .unwrap();
        assert_eq!(payload.nonce, 7);
    }
}
