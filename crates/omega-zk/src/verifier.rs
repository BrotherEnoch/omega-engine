// crates/omega-zk/src/verifier.rs
//
// STARK proof verifier (spec: OmegaVault.starkVerifier.verify(starkProof, blueprintHash, publicInputsHash)).
//
// Used in two contexts:
//   1. Off-chain simulation before relay submission — verifies the proof
//      is valid before attaching it to the bundle.
//   2. On-chain gate in OmegaVault.releaseProfit() — the Solidity StarkVerifier
//      calls the EVM-compiled Winterfell verifier with the same proof bytes.
//
// The verifier is stateless and Send + Sync — it can be called concurrently
// from multiple relay-submission tasks.
//
// FIX (this revision), matching prover.rs's own fix: verify() now takes an explicit
// `expected_public_inputs_hash` parameter and checks the proof's committed
// public_inputs_hash against it, mirroring exactly how the real on-chain call works --
// OmegaVault.submitProof() passes blueprintHash AND publicInputsHash to
// IStarkVerifier.verify() as explicit arguments to check against, not values the verifier
// independently knows or stores as fixed state. This is deliberately NOT stored as
// constructor-time state on ZkVerifier the way expected_chain_id is: chain_id is genuinely
// fixed per-deployment of this whole system, but publicInputsHash varies per-blueprint by
// design, so it has to be a per-call argument, not per-instance state.

use crate::error::ZkError;
use crate::metrics;
use crate::prover::{ProverTier, ZkProof};
/// Stateless STARK proof verifier.
#[derive(Debug, Default)]
pub struct ZkVerifier {
    expected_chain_id: u64,
}

impl ZkVerifier {
    pub fn new(chain_id: u64) -> Self {
        Self {
            expected_chain_id: chain_id,
        }
    }

    /// Verify a ZkProof produced by T1SoftwareProver.
    ///
    /// Checks:
    ///   1. Chain ID matches the verifier's expected chain.
    ///   2. `expected_public_inputs_hash` matches the value the proof actually commits to --
    ///      NEW (this fix). Without this check, a proof correctly bound to Vault A's
    ///      publicInputsHash would still pass verification when checked against Vault B's
    ///      expected value, as long as blueprint_hash/net_profit/chain_id happened to match
    ///      -- exactly the cross-deployment replay OmegaVault.sol's address(this) binding is
    ///      meant to prevent. This is the check that actually enforces that prevention on the
    ///      Rust side; committing the hash in prover.rs alone doesn't enforce anything unless
    ///      something also checks it here.
    ///   3. Proof bytes meet a minimum length sanity floor (see MIN_PROOF_BYTES doc comment
    ///      below — fixed this revision, was previously documented incorrectly).
    ///   4. Winterfell proof deserialises correctly.
    ///   5. Winterfell verify() passes.
    ///
    /// Returns `Ok(())` on success; `Err(ZkError::VerificationFailed)` otherwise.
    pub fn verify(
        &self,
        proof: &ZkProof,
        expected_public_inputs_hash: [u8; 32],
    ) -> Result<(), ZkError> {
        // 1. Chain ID guard.
        if proof.chain_id != self.expected_chain_id {
            metrics::VERIFICATION_FAILURES.inc();
            return Err(ZkError::VerificationFailed {
                blueprint_hash: hex::encode(proof.blueprint_hash),
            });
        }

        // 2. NEW (this fix): publicInputsHash guard -- see doc comment above for why this is
        // the check that actually closes the cross-deployment replay gap, not merely
        // committing the hash in the proof.
        if proof.public_inputs_hash != expected_public_inputs_hash {
            metrics::VERIFICATION_FAILURES.inc();
            return Err(ZkError::VerificationFailed {
                blueprint_hash: hex::encode(proof.blueprint_hash),
            });
        }

        // 3. Minimum proof length -- FIXED (this revision): the prior comment here claimed
        // "a valid Winterfell proof is at least 256 bytes" while the constant was set to 64,
        // a direct contradiction between the two. Neither number was ever verified against a
        // real proof produced by THIS specific AIR/ProofOptions configuration -- real
        // Winterfell STARK proofs are typically tens to hundreds of KB, so both 64 and 256
        // are almost certainly far below the true size regardless. This check is a cheap
        // sanity floor to reject an obviously-truncated or empty byte array before spending
        // time on real deserialization/verification below -- it is NOT a validated precise
        // minimum for this AIR, and shouldn't be read as one. Determining the real minimum
        // requires actually running T1SoftwareProver::prove() once against this exact
        // ProofOptions (28, 8, 0, FieldExtension::None, 8, 127) and measuring proof_bytes.len()
        // -- flagged rather than guessed at, since no Rust toolchain was available to run that
        // measurement directly.
        const MIN_PROOF_BYTES: usize = 64;
        if proof.proof_bytes.len() < MIN_PROOF_BYTES {
            metrics::VERIFICATION_FAILURES.inc();
            return Err(ZkError::InvalidProofLength {
                min_bytes: MIN_PROOF_BYTES,
                got_bytes: proof.proof_bytes.len(),
            });
        }

        // 4 + 5. Deserialise and verify via Winterfell.
        match proof.prover_tier {
            ProverTier::T1Software => self.verify_winterfell(proof),
            ProverTier::T1Hardware => self.verify_winterfell(proof), // same format
        }
    }

    fn verify_winterfell(&self, proof: &ZkProof) -> Result<(), ZkError> {
        use crate::prover::{BlueprintAir, BlueprintPublicInputs};
        use winterfell::{
            crypto::{hashers::Blake3_256, DefaultRandomCoin},
            math::fields::f128::BaseElement,
            verify, AcceptableOptions,
        };

        // Deserialise the proof bytes.
        let stark_proof: winterfell::StarkProof =
            winterfell::StarkProof::from_bytes(&proof.proof_bytes).map_err(|_e| {
                metrics::VERIFICATION_FAILURES.inc();
                ZkError::VerificationFailed {
                    blueprint_hash: hex::encode(proof.blueprint_hash),
                }
            })?;

        // Reconstruct public inputs (must match what was proved). Updated (this fix) to pass
        // proof.public_inputs_hash through to the 4-argument constructor -- see prover.rs.
        let pub_inputs = BlueprintPublicInputs::new(
            &proof.blueprint_hash,
            &proof.public_inputs_hash,
            proof.net_profit_wei,
            proof.chain_id,
        );

        let acceptable = AcceptableOptions::OptionSet(vec![winterfell::ProofOptions::new(
            28,
            8,
            0,
            winterfell::FieldExtension::None,
            8,
            127,
        )]);

        type HashFn = Blake3_256<BaseElement>;
        type RandCoin = DefaultRandomCoin<HashFn>;

        verify::<BlueprintAir, HashFn, RandCoin>(stark_proof, pub_inputs, &acceptable).map_err(
            |_| {
                metrics::VERIFICATION_FAILURES.inc();
                ZkError::VerificationFailed {
                    blueprint_hash: hex::encode(proof.blueprint_hash),
                }
            },
        )?;

        metrics::PROOFS_VERIFIED.inc();
        tracing::debug!(
            blueprint_hash = hex::encode(proof.blueprint_hash),
            public_inputs_hash = hex::encode(proof.public_inputs_hash),
            "ZK proof verified successfully"
        );
        Ok(())
    }
}

#[cfg(test)]
mod verifier_tests {
    use super::*;
    use crate::prover::T1SoftwareProver;

    fn hash(b: u8) -> [u8; 32] {
        [b; 32]
    }

    // NEW (this fix): regression test for the exact gap this change closes -- a proof
    // correctly generated and internally valid, but checked against the WRONG expected
    // public_inputs_hash (standing in for "checked against a different Vault deployment's
    // expected value"), must fail verification rather than silently pass.
    #[test]
    fn verify_rejects_wrong_expected_public_inputs_hash() {
        let prover = T1SoftwareProver::new(42161);
        let verifier = ZkVerifier::new(42161);

        let proof = prover
            .prove(hash(0x01), hash(0x11), 1_000_000_000, "SA")
            .unwrap();

        // Correct expected value -- should pass.
        assert!(verifier.verify(&proof, hash(0x11)).is_ok());

        // Wrong expected value (e.g. another Vault deployment's publicInputsHash) -- must fail.
        assert!(verifier.verify(&proof, hash(0x99)).is_err());
    }

    #[test]
    fn verify_rejects_wrong_chain_id() {
        let prover = T1SoftwareProver::new(42161);
        let verifier = ZkVerifier::new(1); // different chain

        let proof = prover
            .prove(hash(0x01), hash(0x11), 1_000_000_000, "SA")
            .unwrap();

        assert!(verifier.verify(&proof, hash(0x11)).is_err());
    }
}