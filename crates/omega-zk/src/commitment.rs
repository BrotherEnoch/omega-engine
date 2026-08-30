// crates/omega-zk/src/commitment.rs
//
// Proof commitment derivation (spec: ExecutionBlueprint.zk_proof_commitment: Option<B256>).
//
// A proof commitment is a 32-byte hash that binds together:
//   - The blueprint hash (what is being proved).
//   - The net profit amount (what the proof attests to).
//   - The chain ID (prevents cross-chain replay).
//   - The strategy ID (proof is strategy-specific).
//
// This commitment is embedded in the ExecutionBlueprint as an off-chain
// telemetry / blueprint-field binding. It is NOT the same value as
// `public_inputs_hash` (`binding.rs`) and is NOT what OmegaVault.submitProof
// or IStarkVerifier.verify check on-chain.
//
// On-chain binding is exclusively:
//   publicInputsHash = keccak256(abi.encode(VERSION, vault, blueprintHash, netProfit, token))
//
// Do not pass ProofCommitment into ZkVerifier or submitProof.
//
// Derivation:
//   commitment = keccak256(blueprint_hash ‖ net_profit_le ‖ chain_id_le ‖ strategy_id_bytes)

use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};

/// A 32-byte proof commitment embedded in ExecutionBlueprint.zk_proof_commitment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProofCommitment(pub [u8; 32]);

impl ProofCommitment {
    /// Hex-encode with 0x prefix.
    pub fn to_hex(&self) -> String {
        format!("0x{}", hex::encode(self.0))
    }

    /// Raw bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Display for ProofCommitment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

/// Compute the proof commitment for a blueprint.
///
/// # Arguments
/// * `blueprint_hash`   — keccak256 of the encoded ExecutionBlueprint (32 bytes).
/// * `net_profit_wei`   — expected net profit in wei (u128, little-endian encoded).
/// * `chain_id`         — chain ID the blueprint targets (u64, little-endian).
/// * `strategy_id`      — strategy identifier string bytes.
///
/// # Returns
/// A 32-byte `ProofCommitment` that binds all four fields together.
pub fn compute_proof_commitment(
    blueprint_hash: &[u8; 32],
    net_profit_wei: u128,
    chain_id: u64,
    strategy_id: &str,
) -> ProofCommitment {
    let mut h = Keccak256::new();
    h.update(blueprint_hash);
    h.update(net_profit_wei.to_le_bytes());
    h.update(chain_id.to_le_bytes());
    h.update(strategy_id.as_bytes());
    ProofCommitment(h.finalize().into())
}

/// Verify that `commitment` matches the supplied fields.
///
/// Used in the Vault simulation path before submitting the proof on-chain.
pub fn verify_commitment(
    commitment: &ProofCommitment,
    blueprint_hash: &[u8; 32],
    net_profit_wei: u128,
    chain_id: u64,
    strategy_id: &str,
) -> bool {
    let expected = compute_proof_commitment(blueprint_hash, net_profit_wei, chain_id, strategy_id);
    commitment == &expected
}

#[cfg(test)]
mod commitment_tests {
    use super::*;

    fn hash(b: u8) -> [u8; 32] {
        [b; 32]
    }

    #[test]
    fn commitment_is_deterministic() {
        let c1 = compute_proof_commitment(&hash(0xaa), 1_000_000, 42161, "LA");
        let c2 = compute_proof_commitment(&hash(0xaa), 1_000_000, 42161, "LA");
        assert_eq!(c1, c2);
    }

    #[test]
    fn different_hashes_produce_different_commitments() {
        let c1 = compute_proof_commitment(&hash(0xaa), 1_000_000, 42161, "LA");
        let c2 = compute_proof_commitment(&hash(0xbb), 1_000_000, 42161, "LA");
        assert_ne!(c1, c2);
    }

    #[test]
    fn different_profit_produces_different_commitment() {
        let c1 = compute_proof_commitment(&hash(0x01), 100, 42161, "SA");
        let c2 = compute_proof_commitment(&hash(0x01), 200, 42161, "SA");
        assert_ne!(c1, c2);
    }

    #[test]
    fn different_chain_produces_different_commitment() {
        let c1 = compute_proof_commitment(&hash(0x01), 100, 42161, "SA");
        let c2 = compute_proof_commitment(&hash(0x01), 100, 1, "SA");
        assert_ne!(c1, c2);
    }

    #[test]
    fn different_strategy_produces_different_commitment() {
        let c1 = compute_proof_commitment(&hash(0x01), 100, 42161, "SA");
        let c2 = compute_proof_commitment(&hash(0x01), 100, 42161, "LA");
        assert_ne!(c1, c2);
    }

    #[test]
    fn verify_commitment_passes_for_correct_inputs() {
        let c = compute_proof_commitment(&hash(0xcc), 500, 42161, "MSA");
        assert!(verify_commitment(&c, &hash(0xcc), 500, 42161, "MSA"));
    }

    #[test]
    fn verify_commitment_fails_for_wrong_profit() {
        let c = compute_proof_commitment(&hash(0xcc), 500, 42161, "MSA");
        assert!(!verify_commitment(&c, &hash(0xcc), 501, 42161, "MSA"));
    }

    #[test]
    fn commitment_display_is_0x_prefixed_hex() {
        let c = compute_proof_commitment(&hash(0x00), 0, 42161, "CNRY");
        let s = c.to_string();
        assert!(s.starts_with("0x"));
        assert_eq!(s.len(), 66); // "0x" + 64 hex chars
    }
}