// crates/omega-execution/src/signer.rs
//
// TransactionSigner — the genuinely missing piece identified in
// ExecutionPipelineSpecification.md §8. See that document and this
// trait's own doc comment for the full reasoning:
//
//   - `omega_security::BlueprintSigner` signs a blueprint's
//     *authorization* hash (EIP-191) for the on-chain Orchestrator's
//     ecrecover check. It does not sign an Ethereum/Arbitrum transaction
//     envelope (nonce, gas price, `to`/`value`/`data`, RLP encoding).
//   - `omega_simulation::SimulationSubmitter` DOES sign and send real
//     transactions, but only against a local Anvil fork with a
//     dev-funded Anvil test key, and is deliberately, structurally
//     walled off from any live transport (`reject_if_live_looking`,
//     construction only via `SimulationSubmitter::bound_to(&ForkHandle, ..)`).
//
// No production implementation of this trait exists in this crate, on
// purpose. Fabricating one would mean either inventing a fake signer
// (dangerous — it would silently "work" while producing garbage/unsigned
// output) or reusing SimulationSubmitter's dev key outside its intended
// sandbox (worse — that key's entire safety property is "never holds real
// value," and using it live would violate that on purpose).
//
// `ExecutionPipeline` is generic over `S: TransactionSigner` specifically
// so the rest of the pipeline (integrity check, kill switch, 15 pre-trade
// checks, idempotency dedup, DAG bookkeeping) is fully implemented and
// fully testable today, without this crate pretending the signing gap is
// closed. A production implementation (HSM/KMS-backed, or built around
// `omega_security::KeyManager` plus a real RLP transaction encoder) must
// be written and injected before this pipeline can run with
// `active_phase >= 1`. Until then, `UnconfiguredSigner` makes that
// explicit and loud instead of silent.

use async_trait::async_trait;
use omega_core::types::blueprint::ExecutionBlueprint;

use crate::error::ExecutionError;

/// A fully-signed, RLP-encoded transaction ready for
/// `eth_sendBundle`/`eth_sendRawTransaction`, hex-encoded with a `0x` prefix.
#[derive(Debug, Clone)]
pub struct SignedTransaction {
    pub raw_tx_hex: String,
}

/// Produces a signed transaction from an `ExecutionBlueprint`. See this
/// module's doc comment for why no production implementation exists yet.
#[async_trait]
pub trait TransactionSigner: Send + Sync {
    async fn sign_transaction(
        &self,
        bp: &ExecutionBlueprint,
        chain_id: u64,
    ) -> Result<SignedTransaction, ExecutionError>;
}

/// Always-fails signer — the honest default when no real `TransactionSigner`
/// has been wired in. `ExecutionPipeline` can be constructed and every stage
/// BEFORE signing (integrity, kill switch, 15 checks, idempotency) still
/// runs and is still fully testable with this signer installed; it never
/// silently pretends transaction signing works. Returns
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