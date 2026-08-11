// crates/omega-execution/src/signer.rs
//
// TransactionSigner — the genuinely missing piece identified in
// ExecutionPipelineSpecification.md §8.
//
// ## Status as of this revision
//
// `KeyManagerTransactionSigner` below is a REAL, PARTIAL implementation —
// not a placeholder like `UnconfiguredSigner`, but not yet capable of
// producing a transaction that will succeed on-chain either. It correctly
// does the parts that are grounded in code actually read in this
// investigation:
//   - Obtains the active signing key from `omega_security::KeyManager`
//     (confirmed API: `active_secret_key() -> Option<SecretKey>`).
//   - Provides a spec-conformant EIP-1559 RLP encoder (field order and
//     signing-digest construction checked directly against EIP-1559's own
//     text: `keccak256(0x02 || rlp([chain_id, nonce, max_priority_fee_per_gas,
//     max_fee_per_gas, gas_limit, destination, amount, data, access_list]))`
//     for the unsigned form, with `signature_y_parity, signature_r,
//     signature_s` appended for the signed form). NOT yet checked against a
//     known signed-transaction test vector (none was available in this
//     investigation) — treat as spec-conformant-by-inspection, not
//     byte-verified, until it is run against one.
//
// It deliberately STOPS SHORT of building `blueprintCalldata` / the outer
// `execute(blueprintCalldata, sig)` calldata, and returns
// `ExecutionError::SigningFailed` with a specific, named reason if asked to
// sign, because that step requires things not yet available anywhere in
// this workspace as read so far:
//
//   1. An ABI encoder for `abi.encode(uint64, uint64, bytes32, uint8,
//      address, address, bytes, uint256, uint256)` (blueprintCalldata) and
//      `abi.encode(address, uint64, bytes)` (the domain-separation wrapper
//      OmegaOrchestrator.execute() hashes) — no `alloy-sol-types` / `ethabi`
//      / equivalent crate has been seen anywhere in this workspace's
//      dependency graph so far. Hand-rolling ABI encoding here, by guessing
//      at padding/offset rules, is exactly the kind of fabrication this
//      crate has avoided elsewhere (see resolve_flashloan_provider_id in
//      pipeline.rs for the same policy applied to a smaller case).
//   2. The `StrategyId -> bytes32 strategyId` mapping the Orchestrator's
//      `strategy_registry` actually uses. `ExecutionBlueprint::nonce_key()`
//      produces a bytes32 from `strategy_id`, but folds in `chain_id` too
//      and is documented as being for nonce-namespacing specifically — not
//      confirmed to be the same value used at `registerStrategy()` time.
//   3. `ExecutionBlueprint` now carries `flashloan_provider_type` /
//      `provider_contract` / `flashloan_token` as of a later revision (see
//      omega-core::types::blueprint's own doc comment) — this closes part
//      of what was previously flagged here as a genuine schema gap
//      (`providerType`/`flashloanToken` distinct from `providerContract`).
//      What remains unconfirmed is whether the Orchestrator's on-chain
//      `execute()` ABI expects exactly these three values in exactly this
//      shape; that confirmation, not the schema gap itself, is still
//      outstanding.
//   4. `minNetProfit`'s exact source (`dynamic_min_profit` verbatim, or
//      something else) was not confirmed against any file read.
//
// Fabricating any of 1-4 would produce a signer that compiles, "looks"
// correct, and either reverts on-chain every time (best case) or — if one
// of the guesses happens to be subtly wrong in a way that doesn't revert —
// produces a transaction that does something other than what was intended,
// for a contract that moves real flashloaned funds. That's a strictly worse
// failure mode than `UnconfiguredSigner`'s loud upfront refusal, so this
// type fails loudly at the same point instead, with a specific reason
// rather than a generic one.
//
// `omega_security::BlueprintSigner::sign_raw_hash()` (added in the same
// revision as this file) is the correct primitive for the *authorization*
// signature once `bpHash` can actually be computed — see that method's doc
// comment for why `BlueprintSigner::sign()` (EIP-191 prefixed) is NOT
// usable for this purpose, a real, confirmed mismatch against
// OmegaOrchestrator.sol's `bpHash.recover(sig)`.
//
// `omega_simulation::SimulationSubmitter` DOES sign and send real
// transactions, but only against a local Anvil fork with a dev-funded Anvil
// test key, and is deliberately, structurally walled off from any live
// transport (`reject_if_live_looking`, construction only via
// `SimulationSubmitter::bound_to(&ForkHandle, ..)`) — not a substitute for
// this type.
//
// `ExecutionPipeline` is generic over `S: TransactionSigner` specifically
// so the rest of the pipeline (integrity check, kill switch, 15 pre-trade
// checks, idempotency dedup, DAG bookkeeping) is fully implemented and
// fully testable today, independent of this gap.
//
// ## Audit fix (this revision): test helper missing flashloan/token fields
//
// `tests::sample_bp` predates `ExecutionBlueprint` gaining
// `flashloan_provider_type`, `provider_contract`, and `flashloan_token`
// (it already included `max_base_fee_gwei` — see that field's own line
// below, unchanged). Nothing in `KeyManagerTransactionSigner::
// sign_transaction`'s current, real code path reads any of the three
// (see `build_execute_calldata`'s doc comment: it fails loudly before
// touching blueprint fields beyond what's already read), so
// `Balancer`/`Address::ZERO`/`Address::ZERO` are inert placeholders
// consistent with this helper's existing
// `flashloan_provider: Address::ZERO` "no flashloan" convention.
//
// ## Audit fix (this revision, 2): clippy::too_many_arguments
//
// `encode_eip1559_unsigned` takes 8 positional arguments — one over
// clippy's default `too-many-arguments` threshold of 7 — and
// `cargo clippy --workspace --all-targets -- -D warnings` failed on it.
// Its sibling `encode_eip1559_signed` (11 arguments — the same 8 plus
// `y_parity`/`r`/`s`) already carries
// `#[allow(clippy::too_many_arguments)]` for the identical reason: both
// functions' argument lists are mandated by EIP-1559's own field order
// (see this file's "EIP-1559 RLP encoding helpers" section doc comment
// — chain_id, nonce, max_priority_fee_per_gas, max_fee_per_gas,
// gas_limit, destination, amount, data, [access_list]), so splitting
// them into a struct would obscure the 1:1 correspondence to the spec
// this code is deliberately preserving, not simplify anything. Same
// justification, same fix, applied to the sibling function that was
// missing it.

use std::sync::Arc;

use alloy_primitives::{Address, Bytes, U256};
use async_trait::async_trait;
use omega_core::types::blueprint::ExecutionBlueprint;
use omega_security::key_manager::KeyManager;
use secp256k1::{Message, Secp256k1};

use crate::error::ExecutionError;

/// A fully-signed, RLP-encoded transaction ready for
/// `eth_sendBundle`/`eth_sendRawTransaction`, hex-encoded with a `0x` prefix.
#[derive(Debug, Clone)]
pub struct SignedTransaction {
    pub raw_tx_hex: String,
}

/// Produces a signed transaction from an `ExecutionBlueprint`. See this
/// module's doc comment for why no COMPLETE production implementation
/// exists yet.
#[async_trait]
pub trait TransactionSigner: Send + Sync {
    async fn sign_transaction(
        &self,
        bp: &ExecutionBlueprint,
        chain_id: u64,
    ) -> Result<SignedTransaction, ExecutionError>;
}

/// Always-fails signer — the honest default when no `TransactionSigner` at
/// all has been wired in. `ExecutionPipeline` can be constructed and every
/// stage BEFORE signing (integrity, kill switch, 15 checks, idempotency)
/// still runs and is still fully testable with this signer installed; it
/// never silently pretends transaction signing works. Returns
/// `ExecutionError::NoTransactionSigner` every time — never partial
/// success, never a fabricated signature.
pub struct UnconfiguredSigner;

#[async_trait]
impl TransactionSigner for UnconfiguredSigner {
    async fn sign_transaction(
        &self,
        _bp: &ExecutionBlueprint,
        _chain_id: u64,
    ) -> Result<SignedTransaction, ExecutionError> {
        Err(ExecutionError::NoTransactionSigner)
    }
}

// ── Partial production implementation ───────────────────────────────────────

/// Transaction-envelope signer backed by `omega_security::KeyManager`.
///
/// Deliberately a DIFFERENT key manager instance than the one behind
/// `omega_security::BlueprintSigner` may use — the transaction's `from`
/// (gas-paying, funded EOA) and the blueprint's authorizing
/// `execution_key`/`pending_key` are two independent concerns as far as
/// `OmegaOrchestrator.execute()` is concerned: the contract checks `sig`
/// against `execution_key`, not against `msg.sender` / the tx signer. A
/// relayer pattern where a separate hot wallet pays gas is legitimate and
/// this type does not assume otherwise.
///
/// See this module's top doc comment for exactly what this type can and
/// cannot do as of this revision.
pub struct KeyManagerTransactionSigner {
    /// Signs the OUTER transaction envelope (pays gas). NOT necessarily the
    /// same key as the blueprint's on-chain execution_key.
    tx_key_manager: Arc<KeyManager>,
    /// The deployed `OmegaOrchestrator` address — every transaction this
    /// signer produces calls into this contract's `execute()`. Must come
    /// from real deployment configuration; this type does not default it
    /// or accept the zero address.
    orchestrator: Address,
    secp: Secp256k1<secp256k1::All>,
}

impl KeyManagerTransactionSigner {
    /// # Panics
    /// Panics if `orchestrator == Address::ZERO` — a zero target is never a
    /// valid deployment configuration and should fail at construction time,
    /// not on the first signing attempt under load.
    pub fn new(tx_key_manager: Arc<KeyManager>, orchestrator: Address) -> Self {
        assert_ne!(
            orchestrator,
            Address::ZERO,
            "KeyManagerTransactionSigner requires a real deployed OmegaOrchestrator address, \
             not the zero address"
        );
        Self {
            tx_key_manager,
            orchestrator,
            secp: Secp256k1::new(),
        }
    }

    /// The address that will appear as `from` on every transaction this
    /// signer produces. Useful for pre-funding checks / logging without
    /// exposing key material.
    pub fn active_address(&self) -> [u8; 20] {
        self.tx_key_manager.active_address()
    }

    /// Build the ABI-encoded outer calldata for
    /// `OmegaOrchestrator.execute(bytes blueprintCalldata, bytes sig)` and
    /// the on-chain-authorization signature over it.
    ///
    /// NOT IMPLEMENTED — see this module's top doc comment, items 1-4. This
    /// is factored out as its own method (rather than inlined into
    /// `sign_transaction`) so that once an ABI encoder, the strategyId
    /// mapping, and confirmation of the Orchestrator's exact expected ABI
    /// shape for the flashloan-identity fields are in hand, only this
    /// method needs a real body — the RLP/envelope-signing logic in
    /// `sign_transaction` below does not need to change.
    fn build_execute_calldata(&self, _bp: &ExecutionBlueprint) -> Result<Bytes, ExecutionError> {
        Err(ExecutionError::SigningFailed {
            detail: "KeyManagerTransactionSigner::build_execute_calldata is not implemented: \
                requires (1) an ABI encoder for blueprintCalldata / execute()'s outer calldata, \
                not present anywhere in this workspace's dependency graph as of this revision, \
                (2) confirmation of the on-chain strategy_registry's bytes32 strategyId derivation, \
                (3) confirmation of the Orchestrator's exact expected on-chain ABI shape for \
                ExecutionBlueprint's flashloan_provider_type/provider_contract/flashloan_token \
                fields (the schema itself now exists on ExecutionBlueprint, but the ABI \
                encoding contract for it has not been confirmed against any file read), and \
                (4) confirmation of minNetProfit's source field. See signer.rs's module doc \
                comment for the full list. Fabricating any of these for a contract that moves \
                real flashloaned funds is refused by design — same policy as \
                pipeline.rs::resolve_flashloan_provider_id."
                .to_string(),
        })
    }
}

#[async_trait]
impl TransactionSigner for KeyManagerTransactionSigner {
    async fn sign_transaction(
        &self,
        bp: &ExecutionBlueprint,
        chain_id: u64,
    ) -> Result<SignedTransaction, ExecutionError> {
        // Fails loudly here, before touching key material, per this
        // module's top doc comment. Everything below this point is dead
        // code until build_execute_calldata has a real implementation, but
        // is left in place (rather than deleted) so the RLP/signing path
        // is reviewable and ready to wire up once calldata construction is
        // resolved.
        let data = self.build_execute_calldata(bp)?;

        let secret_key =
            self.tx_key_manager
                .active_secret_key()
                .ok_or(ExecutionError::SigningFailed {
                    detail: "tx_key_manager has no active signing key".into(),
                })?;

        let gas_limit = bp.total_l2_gas_budget();
        let max_priority_fee_per_gas =
            U256::from(bp.priority_fee_gwei).saturating_mul(U256::from(1_000_000_000u64));
        // NOTE: this fee formula is NOT a confirmed policy decision — see
        // this module's top doc comment. It's a placeholder conservative
        // estimate (base_fee_at_creation + 2x priority, converted to wei)
        // used only to exercise the RLP-encoding path in tests; whoever
        // owns the gas/fee policy should confirm or replace this before
        // any real transaction is built from it.
        let max_fee_per_gas = U256::from(bp.base_fee_at_creation)
            .saturating_add(U256::from(bp.priority_fee_gwei).saturating_mul(U256::from(2u64)))
            .saturating_mul(U256::from(1_000_000_000u64));

        let unsigned_rlp = encode_eip1559_unsigned(
            chain_id,
            bp.nonce,
            max_priority_fee_per_gas,
            max_fee_per_gas,
            gas_limit,
            self.orchestrator,
            U256::ZERO,
            &data,
        );

        let digest = omega_security::keccak256(&unsigned_rlp);
        let msg =
            Message::from_digest_slice(&digest).map_err(|e| ExecutionError::SigningFailed {
                detail: format!("invalid signing digest: {e}"),
            })?;

        let (recovery_id, compact) = self
            .secp
            .sign_ecdsa_recoverable(&msg, &secret_key)
            .serialize_compact();
        let y_parity = recovery_id.to_i32() as u8;

        let signed_rlp = encode_eip1559_signed(
            chain_id,
            bp.nonce,
            max_priority_fee_per_gas,
            max_fee_per_gas,
            gas_limit,
            self.orchestrator,
            U256::ZERO,
            &data,
            y_parity,
            &compact[..32],
            &compact[32..],
        );

        let raw_tx_hex = format!("0x{}", hex::encode(&signed_rlp));

        tracing::debug!(
            blueprint_hash = %bp.blueprint_hash,
            chain_id,
            nonce = bp.nonce,
            gas_limit,
            "transaction signed"
        );

        Ok(SignedTransaction { raw_tx_hex })
    }
}

// ── EIP-1559 RLP encoding helpers ───────────────────────────────────────────
//
// Field order and signing-digest construction checked directly against
// EIP-1559's specification text (eips.ethereum.org/EIPS/eip-1559):
//
//   unsigned: 0x02 || rlp([chain_id, nonce, max_priority_fee_per_gas,
//             max_fee_per_gas, gas_limit, destination, amount, data,
//             access_list])
//   signing digest: keccak256(unsigned bytes above)
//   signed:   0x02 || rlp([..same 9 fields.., signature_y_parity,
//             signature_r, signature_s])
//
// NOT yet checked against a known signed-transaction test vector (none was
// available in this investigation) — see this file's top doc comment.
// self-contained rather than pulling in alloy's consensus/signer feature
// set, since that dependency question was itself unresolved (see top doc
// comment, item 1) and this encoder does not need the full ABI/consensus
// surface, only RLP.
//
// Both `encode_eip1559_unsigned` and `encode_eip1559_signed` carry
// `#[allow(clippy::too_many_arguments)]` — see this file's module-level
// "Audit fix (this revision, 2)" note for why: their argument lists are
// dictated 1:1 by EIP-1559's own field order, not an accident of API
// design that a struct would clean up.

#[allow(clippy::too_many_arguments)]
fn encode_eip1559_unsigned(
    chain_id: u64,
    nonce: u64,
    max_priority_fee: U256,
    max_fee: U256,
    gas_limit: u64,
    to: Address,
    value: U256,
    data: &[u8],
) -> Vec<u8> {
    let mut payload = Vec::new();
    let list = rlp_list(&[
        rlp_u64(chain_id),
        rlp_u64(nonce),
        rlp_u256(max_priority_fee),
        rlp_u256(max_fee),
        rlp_u64(gas_limit),
        rlp_address(to),
        rlp_u256(value),
        rlp_bytes(data),
        rlp_list(&[]), // empty access list
    ]);
    payload.push(0x02);
    payload.extend_from_slice(&list);
    payload
}

#[allow(clippy::too_many_arguments)]
fn encode_eip1559_signed(
    chain_id: u64,
    nonce: u64,
    max_priority_fee: U256,
    max_fee: U256,
    gas_limit: u64,
    to: Address,
    value: U256,
    data: &[u8],
    y_parity: u8,
    r: &[u8],
    s: &[u8],
) -> Vec<u8> {
    let mut payload = Vec::new();
    let list = rlp_list(&[
        rlp_u64(chain_id),
        rlp_u64(nonce),
        rlp_u256(max_priority_fee),
        rlp_u256(max_fee),
        rlp_u64(gas_limit),
        rlp_address(to),
        rlp_u256(value),
        rlp_bytes(data),
        rlp_list(&[]), // access list
        rlp_u64(y_parity as u64),
        rlp_bytes(r),
        rlp_bytes(s),
    ]);
    payload.push(0x02);
    payload.extend_from_slice(&list);
    payload
}

fn rlp_u64(v: u64) -> Vec<u8> {
    if v == 0 {
        return vec![0x80];
    }
    let bytes = v.to_be_bytes();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(7);
    rlp_bytes(&bytes[start..])
}

fn rlp_u256(v: U256) -> Vec<u8> {
    let bytes = v.to_be_bytes::<32>();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(31);
    if start == 31 && bytes[31] == 0 {
        return vec![0x80];
    }
    rlp_bytes(&bytes[start..])
}

fn rlp_address(addr: Address) -> Vec<u8> {
    rlp_bytes(addr.as_slice())
}

fn rlp_bytes(b: &[u8]) -> Vec<u8> {
    if b.len() == 1 && b[0] < 0x80 {
        return b.to_vec();
    }
    if b.len() < 56 {
        let mut out = Vec::with_capacity(1 + b.len());
        out.push(0x80 + b.len() as u8);
        out.extend_from_slice(b);
        out
    } else {
        let len_bytes = (b.len() as u64).to_be_bytes();
        let start = len_bytes.iter().position(|&x| x != 0).unwrap_or(7);
        let mut out = Vec::with_capacity(1 + (8 - start) + b.len());
        out.push(0xb7 + (8 - start) as u8);
        out.extend_from_slice(&len_bytes[start..]);
        out.extend_from_slice(b);
        out
    }
}

fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
    let total: usize = items.iter().map(|i| i.len()).sum();
    let mut out = Vec::new();
    if total < 56 {
        out.push(0xc0 + total as u8);
    } else {
        let len_bytes = (total as u64).to_be_bytes();
        let start = len_bytes.iter().position(|&x| x != 0).unwrap_or(7);
        out.push(0xf7 + (8 - start) as u8);
        out.extend_from_slice(&len_bytes[start..]);
    }
    for item in items {
        out.extend_from_slice(item);
    }
    out
}

/// Test-only fake — compiled only under `cfg(test)`, the same pattern
/// `omega_relay::client::MockRelayClient` already uses for exactly this
/// reason (a fake that's structurally impossible to link into a
/// production binary). Never gate this behind a `test-utils` feature that
/// a production build could accidentally enable.
#[cfg(test)]
pub struct MockTransactionSigner {
    pub should_fail: bool,
}

#[cfg(test)]
#[async_trait]
impl TransactionSigner for MockTransactionSigner {
    async fn sign_transaction(
        &self,
        bp: &ExecutionBlueprint,
        _chain_id: u64,
    ) -> Result<SignedTransaction, ExecutionError> {
        if self.should_fail {
            return Err(ExecutionError::SigningFailed {
                detail: "mock signer configured to fail".into(),
            });
        }
        // Deterministic fake hex derived from the blueprint hash — good
        // enough to exercise the transform/submission stages in tests;
        // never used outside #[cfg(test)].
        Ok(SignedTransaction {
            raw_tx_hex: format!("0x{}", hex::encode(bp.blueprint_hash.as_slice())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omega_core::types::flashloan_provider::FlashloanProviderType;
    use omega_security::key_manager::KeyManager;
    use secp256k1::SecretKey;

    fn make_km(byte: u8) -> Arc<KeyManager> {
        Arc::new(KeyManager::from_secret_key(
            SecretKey::from_slice(&[byte; 32]).unwrap(),
        ))
    }

    // ── Construction guard ────────────────────────────────────────────────

    #[test]
    #[should_panic(expected = "requires a real deployed OmegaOrchestrator address")]
    fn zero_orchestrator_address_panics_at_construction() {
        let km = make_km(0x01);
        let _ = KeyManagerTransactionSigner::new(km, Address::ZERO);
    }

    #[test]
    fn active_address_matches_key_manager() {
        let km = make_km(0x02);
        let expected = km.active_address();
        let signer = KeyManagerTransactionSigner::new(km, Address::from([0x01; 20]));
        assert_eq!(signer.active_address(), expected);
    }

    // ── sign_transaction currently fails loudly, not silently ──────────────

    #[tokio::test]
    async fn sign_transaction_fails_with_specific_reason_not_silently() {
        // Regression guard: this must remain a loud, specific,
        // SigningFailed error — never a fabricated signature, and never a
        // generic/uninformative failure — until build_execute_calldata is
        // actually implemented. If this test starts failing because
        // sign_transaction now succeeds, build_execute_calldata's doc
        // comment and this module's top doc comment need to be updated
        // together with the implementation, not left stale.
        let km = make_km(0x03);
        let signer = KeyManagerTransactionSigner::new(km, Address::from([0x01; 20]));
        let bp = sample_bp();

        let result = signer.sign_transaction(&bp, 42161).await;
        match result {
            Err(ExecutionError::SigningFailed { detail }) => {
                assert!(
                    detail.contains("build_execute_calldata"),
                    "error must clearly identify the unimplemented step, got: {detail}"
                );
            }
            other => panic!("expected ExecutionError::SigningFailed, got {other:?}"),
        }
    }

    // ── RLP encoder — structural checks against the EIP-1559 spec text ─────

    #[test]
    fn rlp_bytes_single_byte_below_0x80_is_itself() {
        assert_eq!(rlp_bytes(&[0x00]), vec![0x00]);
        assert_eq!(rlp_bytes(&[0x7f]), vec![0x7f]);
    }

    #[test]
    fn rlp_bytes_empty_is_0x80() {
        assert_eq!(rlp_bytes(&[]), vec![0x80]);
    }

    #[test]
    fn rlp_bytes_short_string_prefix() {
        // "dog" (3 bytes) -> 0x83 'd' 'o' 'g' — classic RLP spec example.
        assert_eq!(rlp_bytes(b"dog"), vec![0x83, b'd', b'o', b'g']);
    }

    #[test]
    fn rlp_u64_zero_is_0x80() {
        assert_eq!(rlp_u64(0), vec![0x80]);
    }

    #[test]
    fn rlp_u64_strips_leading_zero_bytes() {
        // 1 -> single byte 0x01, which is < 0x80, so it's encoded as itself.
        assert_eq!(rlp_u64(1), vec![0x01]);
    }

    #[test]
    fn rlp_list_empty_is_0xc0() {
        assert_eq!(rlp_list(&[]), vec![0xc0]);
    }

    #[test]
    fn unsigned_eip1559_payload_starts_with_type_byte_0x02() {
        let payload = encode_eip1559_unsigned(
            42161,
            0,
            U256::from(1_000_000_000u64),
            U256::from(2_000_000_000u64),
            100_000,
            Address::from([0x11; 20]),
            U256::ZERO,
            &[],
        );
        assert_eq!(
            payload[0], 0x02,
            "EIP-1559 typed transactions must be prefixed with 0x02"
        );
        // Byte after the type prefix must be a valid RLP list header
        // (>= 0xc0), per EIP-2718's own differentiation rule from legacy
        // transactions.
        assert!(payload[1] >= 0xc0);
    }

    #[test]
    fn signed_eip1559_payload_includes_signature_fields() {
        let unsigned = encode_eip1559_unsigned(
            1,
            5,
            U256::from(1u64),
            U256::from(2u64),
            21_000,
            Address::from([0x22; 20]),
            U256::ZERO,
            &[0xde, 0xad],
        );
        let signed = encode_eip1559_signed(
            1,
            5,
            U256::from(1u64),
            U256::from(2u64),
            21_000,
            Address::from([0x22; 20]),
            U256::ZERO,
            &[0xde, 0xad],
            1,
            &[0xaa; 32],
            &[0xbb; 32],
        );
        assert!(
            signed.len() > unsigned.len(),
            "signed payload must be strictly larger (it has 3 extra fields)"
        );
        assert_eq!(signed[0], 0x02);
    }

    fn sample_bp() -> ExecutionBlueprint {
        use alloy_primitives::{Bytes, B256};
        use omega_core::types::blueprint::StrategyId;
        use omega_core::types::lane::{Lane, Simulator};
        use uuid::Uuid;

        let signal_id = Uuid::from_bytes([0x01; 16]);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(StrategyId::Sa, 42161, 0, signal_id);
        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO,
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
            // missing flashloan/token fields" note: sign_transaction's
            // current real code path (everything up to and including
            // build_execute_calldata's loud failure) never reads these
            // three, so they mirror this helper's existing
            // flashloan_provider: Address::ZERO "no flashloan"
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
            base_fee_at_creation: 10,
            l1_data_fee_at_creation: 2,
            priority_fee_gwei: 5,
            max_base_fee_gwei: 30,
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
}