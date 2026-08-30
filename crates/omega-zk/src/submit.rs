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
//   Nothing previously turned a verified ZkProof into the calldata OmegaVault
//   expects, or held it for a sender. This module:
//
//     * ABI-encodes `submitProof(bytes32,bytes32,bytes)` against the real
//       OmegaVault.sol signature (no alloy dependency — pure bytes).
//     * Carries a `VerifiedProofSubmission` ready for any tx-signing path
//       (KeyManagerTransactionSigner / ExecutionPipeline) without omega-zk
//       depending on omega-execution or omega-rpc.
//     * Provides `PendingProofBuffer` — a small, bounded, lock-free-ish queue
//       of verified proofs waiting for on-chain submission.
//
// ## Fail closed
//
//   * Empty starkProof bytes → reject encoding (Vault would revert InvalidProof).
//   * Buffer full → drop newest with a warn metric path (caller decides retry).
//   * This module does NOT broadcast transactions; a missing signer must not
//     silently invent a submission.
//
// ## Selector
//
//   submitProof(bytes32,bytes32,bytes)
//   keccak256 → first 4 bytes. Computed offline and pinned as a constant so
//   we do not need a keccak dep at encode time beyond what sha3 already
//   provides in this crate for binding/commitment.

use std::collections::VecDeque;
use std::sync::Mutex;

use sha3::{Digest, Keccak256};

use crate::error::ZkError;
use crate::prover::ZkProof;

// ─────────────────────────────────────────────────────────────────────────────
// Selector (pinned)
// ─────────────────────────────────────────────────────────────────────────────

/// First 4 bytes of keccak256("submitProof(bytes32,bytes32,bytes)").
///
/// Computed at runtime via the same `sha3::Keccak256` this crate already uses
/// for `binding` / `commitment`. Not hard-pinned to a magic constant so a
/// typo cannot silently encode the wrong selector.
pub fn submit_proof_selector() -> [u8; 4] {
    let mut h = Keccak256::new();
    h.update(b"submitProof(bytes32,bytes32,bytes)");
    let full = h.finalize();
    let mut out = [0u8; 4];
    out.copy_from_slice(&full[..4]);
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// VerifiedProofSubmission
// ─────────────────────────────────────────────────────────────────────────────

/// A proof that has already passed `ZkVerifier::verify` and is ready to be
/// sent as `OmegaVault.submitProof(blueprintHash, publicInputsHash, starkProof)`.
#[derive(Debug, Clone)]
pub struct VerifiedProofSubmission {
    pub blueprint_hash: [u8; 32],
    pub public_inputs_hash: [u8; 32],
    pub stark_proof: Vec<u8>,
    pub net_profit_wei: u128,
    pub chain_id: u64,
    pub strategy_id: String,
    pub generation_ms: u64,
}

impl VerifiedProofSubmission {
    /// Build from a verified `ZkProof`. Caller must have already run
    /// `ZkVerifier::verify(&proof, expected_public_inputs_hash)`.
    ///
    /// Fail closed: empty proof bytes are rejected.
    pub fn from_verified_proof(proof: &ZkProof) -> Result<Self, ZkError> {
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

    /// ABI-encode `submitProof(bytes32,bytes32,bytes)` calldata (selector + args).
    pub fn encode_calldata(&self) -> Result<Vec<u8>, ZkError> {
        encode_submit_proof_calldata(
            &self.blueprint_hash,
            &self.public_inputs_hash,
            &self.stark_proof,
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ABI encode
// ─────────────────────────────────────────────────────────────────────────────

/// Encode `submitProof(bytes32 blueprintHash, bytes32 publicInputsHash, bytes starkProof)`.
///
/// Layout (standard Solidity ABI for two static words + one dynamic `bytes`):
///   selector (4)
///   word0: blueprintHash
///   word1: publicInputsHash
///   word2: offset to bytes (= 96 = 0x60)
///   at offset: length (32) ‖ data ‖ right-pad to 32-byte boundary
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

    let mut out = Vec::with_capacity(4 + 96 + 32 + stark_proof.len() + 32);
    out.extend_from_slice(&selector);
    out.extend_from_slice(blueprint_hash);
    out.extend_from_slice(public_inputs_hash);

    // Offset to dynamic bytes: 3 × 32 = 96.
    let mut offset_word = [0u8; 32];
    offset_word[31] = 96;
    out.extend_from_slice(&offset_word);

    // length
    let mut len_word = [0u8; 32];
    let len = stark_proof.len() as u64;
    len_word[24..32].copy_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&len_word);

    // data + right padding
    out.extend_from_slice(stark_proof);
    let pad = (32 - (stark_proof.len() % 32)) % 32;
    out.extend(std::iter::repeat_n(0u8, pad));

    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// PendingProofBuffer
// ─────────────────────────────────────────────────────────────────────────────

/// Bounded buffer of verified proofs awaiting on-chain submission.
///
/// Designed so `main.rs` (or a future keeper task) can:
///   1. `push` after off-chain verify succeeds
///   2. `pop` / `drain` from a dedicated submit loop that owns the signer
///
/// Capacity default: 256 — enough to absorb a short RPC outage without
/// unbounded memory growth. Full buffer refuses the push (fail closed on
/// "silent loss of a proof pathway" vs unbounded growth).
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

    /// Push a verified submission. Returns `Err` if the buffer is full.
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

    /// Drain up to `max` items (FIFO).
    pub fn drain(&self, max: usize) -> Vec<VerifiedProofSubmission> {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let n = max.min(g.len());
        g.drain(..n).collect()
    }
}

impl Default for PendingProofBuffer {
    fn default() -> Self {
        Self::new(256)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prover::{ProverTier, ZkProof};

    fn sample_proof(bytes: Vec<u8>) -> ZkProof {
        ZkProof {
            blueprint_hash: [0x11; 32],
            public_inputs_hash: [0x22; 32],
            net_profit_wei: 1_000,
            chain_id: 42161,
            strategy_id: "LA".into(),
            proof_bytes: bytes,
            prover_tier: ProverTier::T1Software,
            generation_ms: 10,
            proof_size_bytes: 64,
        }
    }

    #[test]
    fn submit_proof_selector_is_stable() {
        assert_eq!(submit_proof_selector(), submit_proof_selector());
        assert_eq!(submit_proof_selector().len(), 4);
    }

    #[test]
    fn encode_rejects_empty_proof() {
        let err = encode_submit_proof_calldata(&[0; 32], &[0; 32], &[]).unwrap_err();
        assert!(matches!(err, ZkError::InvalidProofLength { .. }));
    }

    #[test]
    fn encode_has_selector_and_static_words() {
        let proof = vec![0xab; 65];
        let data = encode_submit_proof_calldata(&[0x11; 32], &[0x22; 32], &proof).unwrap();
        assert_eq!(&data[..4], &submit_proof_selector());
        assert_eq!(&data[4..36], &[0x11; 32]);
        assert_eq!(&data[36..68], &[0x22; 32]);
        // offset == 96
        assert_eq!(data[68 + 31], 96);
        // length == 65
        assert_eq!(data[100 + 31], 65);
        assert_eq!(&data[132..132 + 65], &proof[..]);
    }

    #[test]
    fn from_verified_rejects_empty() {
        let p = sample_proof(vec![]);
        assert!(VerifiedProofSubmission::from_verified_proof(&p).is_err());
    }

    #[test]
    fn buffer_push_pop_fifo() {
        let buf = PendingProofBuffer::new(2);
        let a = VerifiedProofSubmission::from_verified_proof(&sample_proof(vec![1; 64])).unwrap();
        let b = VerifiedProofSubmission::from_verified_proof(&sample_proof(vec![2; 64])).unwrap();
        buf.push(a).unwrap();
        buf.push(b).unwrap();
        assert!(buf
            .push(VerifiedProofSubmission::from_verified_proof(&sample_proof(vec![3; 64])).unwrap())
            .is_err());
        let first = buf.pop().unwrap();
        assert_eq!(first.stark_proof[0], 1);
    }

    #[test]
    fn encode_round_trip_length_consistent() {
        let proof = vec![0xff; 100];
        let data = encode_submit_proof_calldata(&[1; 32], &[2; 32], &proof).unwrap();
        // 4 + 96 + 32 + 100 + 28 pad (100 % 32 = 4 → pad 28)
        assert_eq!(data.len(), 4 + 96 + 32 + 100 + 28);
    }
}