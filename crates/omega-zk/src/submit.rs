// crates/omega-zk/src/submit.rs
//
// On-chain OmegaVault.submitProof pathway (closes the "no relayer/keeper" gap).
//
// ## Role
//
//   Off-chain we already:
//     1. compute public_inputs_hash (binding.rs)
//     2. prove (prover.rs / worker.rs)
//     3. verify (verifier.rs)
//
//   This module:
//     * ABI-encodes `submitProof(bytes32,bytes32,bytes)` against OmegaVault.sol.
//     * Carries a `VerifiedProofSubmission` for any tx-signing path.
//     * Provides `PendingProofBuffer` for verified proofs awaiting submission.
//     * Encodes the SP1StarkVerifierAdapter proof blob correctly:
//         starkProof = abi.encode(bytes publicValues, bytes proofBytes)
//       where publicValues = abi.encode(bytes32 blueprintHash, bytes32 publicInputsHash)
//       (see contracts/src/verifiers/SP1StarkVerifierAdapter.sol header, decision 2).
//
// ## Fail closed
//
//   * Empty proof bytes → reject encoding.
//   * Buffer full → refuse push.
//   * This module does NOT broadcast transactions.
//
// ## FIX (this revision): PendingProofBuffer::drain is now bounded
//
//   `drain(&self)` used to unconditionally take everything off the queue.
//   The keeper task in src/main.rs (the 5s-tick loop that signs and
//   broadcasts submitProof calls) was already calling it as
//   `buf.drain(8)` — a deliberate per-tick cap so proof submission stays
//   throttled even if proofs accumulate faster than the keeper can
//   broadcast them — which meant that call site never actually compiled
//   against the old zero-argument signature (E0061: "this method takes 0
//   arguments but 1 argument was supplied"). Rather than dropping the cap
//   from the caller and unbounding the keeper's per-tick batch size,
//   `drain` now takes `max: usize` and only pulls up to that many entries,
//   leaving the remainder in the buffer for the next tick. This matches
//   `buf.drain(8)` exactly — no change needed at that call site.

use std::collections::VecDeque;
use std::sync::Mutex;

use sha3::{Digest, Keccak256};

use crate::error::ZkError;
use crate::prover::ZkProof;

// ─────────────────────────────────────────────────────────────────────────────
// Selector
// ─────────────────────────────────────────────────────────────────────────────

/// First 4 bytes of keccak256("submitProof(bytes32,bytes32,bytes)").
pub fn submit_proof_selector() -> [u8; 4] {
    let mut h = Keccak256::new();
    h.update(b"submitProof(bytes32,bytes32,bytes)");
    let full = h.finalize();
    let mut out = [0u8; 4];
    out.copy_from_slice(&full[..4]);
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// SP1 adapter proof blob
// ─────────────────────────────────────────────────────────────────────────────

/// Build the `publicValues` bytes the SP1 guest commits and the adapter decodes:
///   abi.encode(bytes32 blueprintHash, bytes32 publicInputsHash)
/// which is simply the two 32-byte words concatenated (static types).
pub fn encode_sp1_public_values(
    blueprint_hash: &[u8; 32],
    public_inputs_hash: &[u8; 32],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(blueprint_hash);
    out.extend_from_slice(public_inputs_hash);
    out
}

/// Build the opaque `bytes proof` argument expected by
/// `SP1StarkVerifierAdapter.verify` / `OmegaVault.submitProof`:
///   abi.encode(bytes publicValues, bytes proofBytes)
pub fn encode_sp1_adapter_proof_blob(
    public_values: &[u8],
    proof_bytes: &[u8],
) -> Vec<u8> {
    fn pad32(n: usize) -> usize {
        (32 - (n % 32)) % 32
    }

    let pv_len = public_values.len();
    let pb_len = proof_bytes.len();
    let pv_data_size = 32 + pv_len + pad32(pv_len);
    let offset0: u64 = 64;
    let offset1: u64 = 64 + pv_data_size as u64;

    let mut out = Vec::with_capacity(64 + pv_data_size + 32 + pb_len + pad32(pb_len));

    let mut w = [0u8; 32];
    w[24..32].copy_from_slice(&offset0.to_be_bytes());
    out.extend_from_slice(&w);
    w = [0u8; 32];
    w[24..32].copy_from_slice(&offset1.to_be_bytes());
    out.extend_from_slice(&w);

    w = [0u8; 32];
    w[24..32].copy_from_slice(&(pv_len as u64).to_be_bytes());
    out.extend_from_slice(&w);
    out.extend_from_slice(public_values);
    out.extend(std::iter::repeat_n(0u8, pad32(pv_len)));

    w = [0u8; 32];
    w[24..32].copy_from_slice(&(pb_len as u64).to_be_bytes());
    out.extend_from_slice(&w);
    out.extend_from_slice(proof_bytes);
    out.extend(std::iter::repeat_n(0u8, pad32(pb_len)));

    out
}

/// public values from the two hashes + raw proof bytes → opaque blob for
/// `submitProof` when the Vault is wired to `SP1StarkVerifierAdapter`.
pub fn encode_sp1_stark_proof_arg(
    blueprint_hash: &[u8; 32],
    public_inputs_hash: &[u8; 32],
    proof_bytes: &[u8],
) -> Result<Vec<u8>, ZkError> {
    if proof_bytes.is_empty() {
        return Err(ZkError::InvalidProofLength {
            min_bytes: 1,
            got_bytes: 0,
        });
    }
    let public_values = encode_sp1_public_values(blueprint_hash, public_inputs_hash);
    Ok(encode_sp1_adapter_proof_blob(&public_values, proof_bytes))
}

// ─────────────────────────────────────────────────────────────────────────────
// VerifiedProofSubmission
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct VerifiedProofSubmission {
    pub blueprint_hash: [u8; 32],
    pub public_inputs_hash: [u8; 32],
    /// Opaque third argument to `submitProof`. For SP1StarkVerifierAdapter this MUST be
    /// `abi.encode(publicValues, proofBytes)` — use `from_verified_proof_sp1`.
    pub stark_proof: Vec<u8>,
    pub net_profit_wei: u128,
    pub chain_id: u64,
    pub strategy_id: String,
    pub generation_ms: u64,
}

impl VerifiedProofSubmission {
    /// Pack for SP1StarkVerifierAdapter (production on-chain path).
    pub fn from_verified_proof_sp1(proof: &ZkProof) -> Result<Self, ZkError> {
        let stark_proof = encode_sp1_stark_proof_arg(
            &proof.blueprint_hash,
            &proof.public_inputs_hash,
            &proof.proof_bytes,
        )?;
        Ok(Self {
            blueprint_hash: proof.blueprint_hash,
            public_inputs_hash: proof.public_inputs_hash,
            stark_proof,
            net_profit_wei: proof.net_profit_wei,
            chain_id: proof.chain_id,
            strategy_id: proof.strategy_id.clone(),
            generation_ms: proof.generation_ms,
        })
    }

    /// Pack raw proof bytes without SP1 wrapping (Winterfell / mock verifiers only).
    pub fn from_verified_proof_raw(proof: &ZkProof) -> Result<Self, ZkError> {
        if proof.proof_bytes.is_empty() {
            return Err(ZkError::InvalidProofLength {
                min_bytes: 1,
                got_bytes: 0,
            });
        }
        Ok(Self {
            blueprint_hash: proof.blueprint_hash,
            public_inputs_hash: proof.public_inputs_hash,
            stark_proof: proof.proof_bytes.clone(),
            net_profit_wei: proof.net_profit_wei,
            chain_id: proof.chain_id,
            strategy_id: proof.strategy_id.clone(),
            generation_ms: proof.generation_ms,
        })
    }

    /// Default: SP1 adapter encoding (production Vault wiring).
    pub fn from_verified_proof(proof: &ZkProof) -> Result<Self, ZkError> {
        Self::from_verified_proof_sp1(proof)
    }

    pub fn encode_calldata(&self) -> Result<Vec<u8>, ZkError> {
        encode_submit_proof_calldata(
            &self.blueprint_hash,
            &self.public_inputs_hash,
            &self.stark_proof,
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ABI encode submitProof(bytes32,bytes32,bytes)
// ─────────────────────────────────────────────────────────────────────────────

pub fn encode_submit_proof_calldata(
    blueprint_hash: &[u8; 32],
    public_inputs_hash: &[u8; 32],
    stark_proof: &[u8],
) -> Result<Vec<u8>, ZkError> {
    if stark_proof.is_empty() {
        return Err(ZkError::InvalidProofLength {
            min_bytes: 1,
            got_bytes: 0,
        });
    }

    let selector = submit_proof_selector();
    let mut out = Vec::with_capacity(4 + 32 + 32 + 32 + 32 + stark_proof.len() + 32);

    out.extend_from_slice(&selector);
    out.extend_from_slice(blueprint_hash);
    out.extend_from_slice(public_inputs_hash);

    let mut offset_word = [0u8; 32];
    offset_word[31] = 96;
    out.extend_from_slice(&offset_word);

    let mut len_word = [0u8; 32];
    let len = stark_proof.len() as u64;
    len_word[24..32].copy_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&len_word);

    out.extend_from_slice(stark_proof);
    let pad = (32 - (stark_proof.len() % 32)) % 32;
    out.extend(std::iter::repeat_n(0u8, pad));

    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// PendingProofBuffer
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct PendingProofBuffer {
    inner: Mutex<VecDeque<VerifiedProofSubmission>>,
    capacity: usize,
}

impl PendingProofBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(capacity.min(1024))),
            capacity: capacity.max(1),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn push(&self, item: VerifiedProofSubmission) -> Result<(), ZkError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| ZkError::Internal(anyhow::anyhow!("PendingProofBuffer lock poisoned")))?;
        if g.len() >= self.capacity {
            return Err(ZkError::SubmissionBufferFull {
                capacity: self.capacity,
            });
        }
        g.push_back(item);
        Ok(())
    }

    pub fn pop(&self) -> Option<VerifiedProofSubmission> {
        self.inner.lock().ok()?.pop_front()
    }

    /// Removes and returns up to `max` entries from the front of the buffer,
    /// oldest first. Leaves any remainder in place for the next call — this
    /// is what keeps the keeper task's per-tick submitProof batch bounded
    /// (see this file's module-level FIX note) rather than draining an
    /// unbounded backlog in a single tick. Passing `usize::MAX` recovers the
    /// old "drain everything" behavior.
    pub fn drain(&self, max: usize) -> Vec<VerifiedProofSubmission> {
        self.inner
            .lock()
            .map(|mut g| {
                let n = g.len().min(max);
                g.drain(..n).collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sp1_public_values_are_64_bytes() {
        let pv = encode_sp1_public_values(&[1u8; 32], &[2u8; 32]);
        assert_eq!(pv.len(), 64);
        assert_eq!(&pv[..32], &[1u8; 32]);
        assert_eq!(&pv[32..], &[2u8; 32]);
    }

    #[test]
    fn sp1_adapter_blob_has_offset_64() {
        let pv = encode_sp1_public_values(&[0x11; 32], &[0x22; 32]);
        let proof = vec![0xABu8; 17];
        let blob = encode_sp1_adapter_proof_blob(&pv, &proof);
        assert_eq!(blob[31], 64);
        assert!(blob.len() > 64);
    }

    #[test]
    fn empty_proof_rejected() {
        let r = encode_sp1_stark_proof_arg(&[0u8; 32], &[0u8; 32], &[]);
        assert!(r.is_err());
    }

    #[test]
    fn selector_is_four_bytes() {
        assert_eq!(submit_proof_selector().len(), 4);
    }

    fn sample_submission(tag: u8) -> VerifiedProofSubmission {
        VerifiedProofSubmission {
            blueprint_hash: [tag; 32],
            public_inputs_hash: [tag; 32],
            stark_proof: vec![tag],
            net_profit_wei: 1,
            chain_id: 42_161,
            strategy_id: "SA".to_string(),
            generation_ms: 1,
        }
    }

    #[test]
    fn drain_respects_max_and_leaves_remainder() {
        let buf = PendingProofBuffer::new(16);
        for i in 0..5u8 {
            buf.push(sample_submission(i)).unwrap();
        }

        let first_batch = buf.drain(3);
        assert_eq!(first_batch.len(), 3);
        assert_eq!(first_batch[0].blueprint_hash, [0u8; 32]);
        assert_eq!(first_batch[2].blueprint_hash, [2u8; 32]);
        assert_eq!(buf.len(), 2, "remaining 2 entries must stay buffered");

        let second_batch = buf.drain(10);
        assert_eq!(second_batch.len(), 2, "drain(10) on 2 remaining must return just those 2");
        assert!(buf.is_empty());
    }

    #[test]
    fn drain_zero_removes_nothing() {
        let buf = PendingProofBuffer::new(16);
        buf.push(sample_submission(0)).unwrap();
        let batch = buf.drain(0);
        assert!(batch.is_empty());
        assert_eq!(buf.len(), 1);
    }
}