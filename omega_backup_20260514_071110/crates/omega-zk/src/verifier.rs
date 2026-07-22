// crates/omega-zk/src/verifier.rs
//
// STARK proof verifier (spec: OmegaVault.starkVerifier.verify(starkProof, blueprintHash, netProfit)).
//
// Used in two contexts:
//   1. Off-chain simulation before relay submission â€” verifies the proof
//      is valid before attaching it to the bundle.
//   2. On-chain gate in OmegaVault.releaseProfit() â€” the Solidity StarkVerifier
//      calls the EVM-compiled Winterfell verifier with the same proof bytes.
//
// The verifier is stateless and Send + Sync â€” it can be called concurrently
// from multiple relay-submission tasks.

use crate::error::ZkError;
use crate::metrics;
use crate::prover::{ZkProof, ProverTier};
use bincode;

/// Stateless STARK proof verifier.
#[derive(Debug, Default)]
pub struct ZkVerifier {
    expected_chain_id: u64,
}

impl ZkVerifier {
    pub fn new(chain_id: u64) -> Self {
        Self { expected_chain_id: chain_id }
    }

    /// Verify a ZkProof produced by T1SoftwareProver.
    ///
    /// Checks:
    ///   1. Chain ID matches the verifier's expected chain.
    ///   2. Proof bytes are non-empty and meet minimum length.
    ///   3. Winterfell proof deserialises correctly.
    ///   4. Winterfell verify() passes.
    ///
    /// Returns `Ok(())` on success; `Err(ZkError::VerificationFailed)` otherwise.
    pub fn verify(&self, proof: &ZkProof) -> Result<(), ZkError> {
        // 1. Chain ID guard.
        if proof.chain_id != self.expected_chain_id {
            metrics::VERIFICATION_FAILURES.inc();
            return Err(ZkError::VerificationFailed {
                blueprint_hash: hex::encode(proof.blueprint_hash),
            });
        }

        // 2. Minimum proof length (a valid Winterfell proof is at least 256 bytes).
        const MIN_PROOF_BYTES: usize = 64;
        if proof.proof_bytes.len() < MIN_PROOF_BYTES {
            metrics::VERIFICATION_FAILURES.inc();
            return Err(ZkError::InvalidProofLength {
                min_bytes: MIN_PROOF_BYTES,
                got_bytes: proof.proof_bytes.len(),
            });
        }

        // 3 + 4. Deserialise and verify via Winterfell.
        match proof.prover_tier {
            ProverTier::T1Software => self.verify_winterfell(proof),
            ProverTier::T1Hardware => self.verify_winterfell(proof), // same format
        }
    }

    fn verify_winterfell(&self, proof: &ZkProof) -> Result<(), ZkError> {
        use winterfell::{
            math::{fields::f128::BaseElement, FieldElement, ToElements},
            verify, AcceptableOptions,
        };
        use crate::prover::{BlueprintPublicInputs, hash_to_field_elements, BlueprintAir};

        // Deserialise the proof bytes.
        let stark_proof: winterfell::StarkProof =
            bincode::deserialize(&proof.proof_bytes).map_err(|e| {
                metrics::VERIFICATION_FAILURES.inc();
                ZkError::VerificationFailed {
                    blueprint_hash: hex::encode(proof.blueprint_hash),
                }
            })?;

        // Reconstruct public inputs (must match what was proved).
        let pub_inputs = BlueprintPublicInputs {
            hash_commitment: hash_to_field_elements(&proof.blueprint_hash),
            net_profit_hi:   BaseElement::new((proof.net_profit_wei >> 64) as u128),
            net_profit_lo:   BaseElement::new(proof.net_profit_wei as u64 as u128),
            chain_id:        BaseElement::new(proof.chain_id as u128),
        };

        let acceptable = AcceptableOptions::OptionSet(vec![
            winterfell::ProofOptions::new(28, 8, 0,
                winterfell::FieldExtension::None, 8, 127),
        ]);

        verify::<BlueprintAir>(stark_proof, pub_inputs, &acceptable).map_err(|_| {
            metrics::VERIFICATION_FAILURES.inc();
            ZkError::VerificationFailed {
                blueprint_hash: hex::encode(proof.blueprint_hash),
            }
        })?;

        metrics::PROOFS_VERIFIED.inc();
        tracing::debug!(
            blueprint_hash = hex::encode(proof.blueprint_hash),
            "ZK proof verified successfully"
        );
        Ok(())
    }
}