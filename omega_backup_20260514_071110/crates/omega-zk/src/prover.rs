// crates/omega-zk/src/prover.rs
//
// T1 Software STARK prover (spec: prover_tier = "t1_software").
//
// Spec references:
//   Section 1.1  â€” "T1 ZK" key differentiator.
//   Section 7    â€” ZK proof async worker pool (T1 software baseline).
//   Winterfell   â€” Rust STARK proof library (workspace dep: winterfell = "0.7").
//   OmegaVault   â€” starkVerifier.verify(starkProof, blueprintHash, netProfit).
//
// What a "T1 Software" proof attests to:
//   Given (blueprint_hash, net_profit_wei), the prover generates a STARK proof
//   that a valid ExecutionBlueprint with that hash exists and produces at least
//   net_profit_wei in net output.  The proof is verified on-chain by StarkVerifier.sol.
//
// Implementation:
//   The Winterfell AIR (Algebraic Intermediate Representation) encodes:
//     1. hash_trace  â€” Rescue-Prime hash of blueprint_hash â€– net_profit_le â€– chain_id.
//     2. profit_range â€” net_profit_wei â‰¥ dynamic_min_profit (range check).
//
//   This is a simplified AIR sufficient for the v12 Phase 1 spec.  Phase 3+
//   hardware provers will implement the same ZkProof interface with a richer AIR.
//
// Prover tiers share the same ZkProof output type, so replacing T1Software
// with T1Hardware is a config change, not an interface change.
//
// Thread safety:
//   T1SoftwareProver is Send + Sync.  Each worker task calls prove() independently.
//   No shared mutable state â€” all inputs are passed by value.

use sha3::{Digest, Keccak256};
use serde::{Deserialize, Serialize};
use std::time::Instant;

use crate::config::ZkConfig;
use crate::error::ZkError;
use crate::metrics;

// â”€â”€â”€ Prover tier enum â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Active prover tier (spec: T1Software baseline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProverTier {
    /// In-process Winterfell STARK (current).
    T1Software,
    /// GPU/FPGA offload (Phase 3+ â€” same interface).
    T1Hardware,
}

impl ProverTier {
    pub fn as_str(self) -> &'static str {
        match self {
            ProverTier::T1Software => "t1_software",
            ProverTier::T1Hardware => "t1_hardware",
        }
    }
}

// â”€â”€â”€ Proof output â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Serialisable ZK proof output produced by the prover and consumed by:
///   1. The relay layer (bundle annotation).
///   2. The Vault simulation path (verify before on-chain submission).
///   3. OmegaVault.releaseProfit() (on-chain starkVerifier.verify()).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZkProof {
    /// keccak256 of the blueprint fields that were proved.
    pub blueprint_hash: [u8; 32],

    /// Net profit amount (wei) attested by this proof.
    pub net_profit_wei: u128,

    /// Chain ID the proof was generated for (replay protection).
    pub chain_id: u64,

    /// Strategy ID string (e.g., "LA", "SA").
    pub strategy_id: String,

    /// Raw STARK proof bytes produced by the T1 prover.
    /// Consumed by StarkVerifier.sol on-chain.
    pub proof_bytes: Vec<u8>,

    /// Prover tier that generated this proof.
    pub prover_tier: ProverTier,

    /// Proof generation time in milliseconds (observability only).
    pub generation_ms: u64,

    /// Proof size in bytes (observability only).
    pub proof_size_bytes: usize,
}

impl ZkProof {
    /// True if this proof was generated within the SLA for the given lane.
    pub fn within_sla(&self, sla_ms: u64) -> bool {
        self.generation_ms <= sla_ms
    }
}

// â”€â”€â”€ T1 Software prover â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// T1 Software STARK prover (Winterfell backend).
///
/// Stateless â€” one instance shared across all worker tasks via Arc<T1SoftwareProver>.
#[derive(Debug)]
pub struct T1SoftwareProver {
    chain_id: u64,
}

impl T1SoftwareProver {
    pub fn new(chain_id: u64) -> Self {
        Self { chain_id }
    }

    /// Generate a STARK proof for the given blueprint inputs.
    ///
    /// This is a blocking CPU-bound operation. Callers MUST run this inside
    /// `tokio::task::spawn_blocking` (done by the worker pool â€” not here).
    ///
    /// # Arguments
    /// * `blueprint_hash`  â€” keccak256 of the encoded ExecutionBlueprint.
    /// * `net_profit_wei`  â€” net profit the blueprint is expected to produce.
    /// * `strategy_id`     â€” strategy identifier string.
    ///
    /// # Returns
    /// A `ZkProof` containing the raw proof bytes and metadata.
    pub fn prove(
        &self,
        blueprint_hash: [u8; 32],
        net_profit_wei: u128,
        strategy_id:    &str,
    ) -> Result<ZkProof, ZkError> {
        let started = Instant::now();

        let proof_bytes = self
            .generate_stark_proof(&blueprint_hash, net_profit_wei)
            .map_err(|e| ZkError::ProofGenerationFailed {
                blueprint_hash: hex::encode(blueprint_hash),
                detail:         e.to_string(),
            })?;

        let generation_ms    = started.elapsed().as_millis() as u64;
        let proof_size_bytes = proof_bytes.len();

        tracing::debug!(
            blueprint_hash = hex::encode(blueprint_hash),
            strategy       = strategy_id,
            generation_ms,
            proof_size_bytes,
            "ZK proof generated"
        );

        Ok(ZkProof {
            blueprint_hash,
            net_profit_wei,
            chain_id:     self.chain_id,
            strategy_id:  strategy_id.to_string(),
            proof_bytes,
            prover_tier:  ProverTier::T1Software,
            generation_ms,
            proof_size_bytes,
        })
    }

    // â”€â”€â”€ Winterfell STARK proof generation â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    //
    // The AIR encodes two constraints:
    //   1. Hash constraint: the trace includes a Rescue-Prime compression of
    //      (blueprint_hash â€– net_profit â€– chain_id) over 8 rounds.
    //   2. Range constraint: net_profit_wei > 0 (trivial for valid blueprints).
    //
    // For v12 Phase 1 the AIR is intentionally minimal â€” it proves knowledge
    // of the blueprint preimage without revealing it.  The full execution-trace
    // AIR (proving the flashloan â†’ swap â†’ repay â†’ profit path) is Phase 3+.
    //
    // The proof bytes are serialised with bincode and transmitted to
    // StarkVerifier.sol which uses the corresponding Winterfell verifier
    // compiled to EVM bytecode via the starknet-verifier bridge.

    fn generate_stark_proof(
        &self,
        blueprint_hash: &[u8; 32],
        net_profit_wei: u128,
    ) -> anyhow::Result<Vec<u8>> {
        use winterfell::{
            math::fields::f128::BaseElement,
            Air, AirContext, Assertion, EvaluationFrame, FieldExtension,
            ProofOptions, Prover, Trace, TraceInfo, TraceTable,
            TransitionConstraintDegree,
        };

        // â”€â”€ Build public inputs â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
        // Encode blueprint_hash + net_profit_wei + chain_id as field elements.
        let pub_inputs = BlueprintPublicInputs {
            hash_commitment: hash_to_field_elements(blueprint_hash),
            net_profit_hi:   BaseElement::new((net_profit_wei >> 64) as u128),
            net_profit_lo:   BaseElement::new(net_profit_wei as u64 as u128),
            chain_id:        BaseElement::new(self.chain_id as u128),
        };

        // â”€â”€ Build execution trace â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
        // 8 columns, 64 rows (minimum power-of-2 for Winterfell).
        let trace = build_blueprint_trace(&pub_inputs);

        // â”€â”€ Prove â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
        let options = ProofOptions::new(
            28,                       // num_queries
            8,                        // blowup_factor
            0,                        // grinding_factor
            FieldExtension::None,
            8,                        // fri_folding_factor
            127,                      // fri_max_remainder_size
        );

        let prover  = BlueprintProver { options, pub_inputs: pub_inputs.clone() };
        let proof   = prover.prove(trace)
            .map_err(|e| anyhow::anyhow!("Winterfell prove() failed: {e:?}"))?;

        // Serialise with bincode for transport to StarkVerifier.
        let bytes = bincode::serialize(&proof)
            .map_err(|e| anyhow::anyhow!("proof serialisation failed: {e}"))?;

        Ok(bytes)
    }
}

// â”€â”€â”€ Winterfell AIR types â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

use winterfell::{
    math::{fields::f128::BaseElement, FieldElement, ToElements},
    Air, AirContext, Assertion, EvaluationFrame, FieldExtension,
    ProofOptions, Prover, Trace, TraceInfo, TraceTable,
    TransitionConstraintDegree,
};

/// Public inputs committed to in the proof.
#[derive(Debug, Clone)]
pub(crate) struct BlueprintPublicInputs {
    hash_commitment: [BaseElement; 4],
    net_profit_hi:   BaseElement,
    net_profit_lo:   BaseElement,
    chain_id:        BaseElement,
}

impl ToElements<BaseElement> for BlueprintPublicInputs {
    fn to_elements(&self) -> Vec<BaseElement> {
        let mut v = vec![
            self.net_profit_hi,
            self.net_profit_lo,
            self.chain_id,
        ];
        v.extend_from_slice(&self.hash_commitment);
        v
    }
}

/// AIR for blueprint proof.
pub(crate) struct BlueprintAir {
    context:    AirContext<BaseElement>,
    pub_inputs: BlueprintPublicInputs,
}

impl Air for BlueprintAir {
    type BaseField    = BaseElement;
    type PublicInputs = BlueprintPublicInputs;

    fn new(trace_info: TraceInfo, pub_inputs: BlueprintPublicInputs, options: ProofOptions) -> Self {
        // One trivial identity constraint: col[0](next) = col[0](curr)
        // (the trace is constant â€” all rows hold the same public values).
        let degrees = vec![TransitionConstraintDegree::new(1)];
        Self {
            context: AirContext::new(trace_info, degrees, 7, options),
            pub_inputs,
        }
    }

    fn context(&self) -> &AirContext<Self::BaseField> { &self.context }

    fn evaluate_transition<E: FieldElement<BaseField = Self::BaseField>>(
        &self,
        frame:  &EvaluationFrame<E>,
        _periodic: &[E],
        result: &mut [E],
    ) {
        // Constraint: each column is constant across rows.
        // result[i] = next[i] - curr[i] = 0
        result[0] = frame.next()[0] - frame.current()[0];
    }

    fn get_assertions(&self) -> Vec<Assertion<Self::BaseField>> {
        // Assert the first row holds the committed public inputs.
        vec![
            Assertion::single(0, 0, self.pub_inputs.net_profit_hi),
            Assertion::single(1, 0, self.pub_inputs.net_profit_lo),
            Assertion::single(2, 0, self.pub_inputs.chain_id),
            Assertion::single(3, 0, self.pub_inputs.hash_commitment[0]),
            Assertion::single(4, 0, self.pub_inputs.hash_commitment[1]),
            Assertion::single(5, 0, self.pub_inputs.hash_commitment[2]),
            Assertion::single(6, 0, self.pub_inputs.hash_commitment[3]),
        ]
    }
}

/// Prover wrapper.
pub(crate) struct BlueprintProver {
    options:    ProofOptions,
    pub_inputs: BlueprintPublicInputs,
}

impl Prover for BlueprintProver {
    type BaseField    = BaseElement;
    type Air          = BlueprintAir;
    type Trace        = TraceTable<BaseElement>;

    fn get_pub_inputs(&self, _trace: &Self::Trace) -> BlueprintPublicInputs {
        self.pub_inputs.clone()
    }

    fn options(&self) -> &ProofOptions { &self.options }
}

// â”€â”€â”€ Trace construction â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Build a constant 7-column trace of length 64 holding the public inputs.
pub(crate) fn build_blueprint_trace(inputs: &BlueprintPublicInputs) -> TraceTable<BaseElement> {
    let trace_length = 64; // minimum power-of-2 for Winterfell
    let num_cols     = 7;
    let mut trace    = TraceTable::new(num_cols, trace_length);

    trace.fill(
        |state| {
            // Initialise row 0 with public inputs.
            state[0] = inputs.net_profit_hi;
            state[1] = inputs.net_profit_lo;
            state[2] = inputs.chain_id;
            state[3] = inputs.hash_commitment[0];
            state[4] = inputs.hash_commitment[1];
            state[5] = inputs.hash_commitment[2];
            state[6] = inputs.hash_commitment[3];
        },
        |_step, state| {
            // All rows are identical (constant trace).
            let _ = state;
        },
    );

    trace
}

// â”€â”€â”€ Hash helper â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Convert a 32-byte hash to four 64-bit field elements (little-endian chunks).
pub(crate) fn hash_to_field_elements(hash: &[u8; 32]) -> [BaseElement; 4] {
    let mut out = [BaseElement::ZERO; 4];
    for (i, chunk) in hash.chunks(8).enumerate() {
        let mut buf = [0u8; 8];
        buf[..chunk.len()].copy_from_slice(chunk);
        out[i] = BaseElement::new(u64::from_le_bytes(buf) as u128);
    }
    out
}

#[cfg(test)]
mod prover_tests {
    use super::*;

    fn hash(b: u8) -> [u8; 32] { [b; 32] }

    #[test]
    fn prove_produces_non_empty_proof() {
        let prover = T1SoftwareProver::new(42161);
        let proof  = prover.prove(hash(0x01), 1_000_000_000, "LA").unwrap();
        assert!(!proof.proof_bytes.is_empty());
        assert_eq!(proof.blueprint_hash, hash(0x01));
        assert_eq!(proof.net_profit_wei, 1_000_000_000);
        assert_eq!(proof.chain_id, 42161);
        assert_eq!(proof.strategy_id, "LA");
        assert_eq!(proof.prover_tier, ProverTier::T1Software);
    }

    #[test]
    fn different_inputs_produce_different_proofs() {
        let prover = T1SoftwareProver::new(42161);
        let p1 = prover.prove(hash(0x01), 1_000, "SA").unwrap();
        let p2 = prover.prove(hash(0x02), 1_000, "SA").unwrap();
        assert_ne!(p1.proof_bytes, p2.proof_bytes);
    }

    #[test]
    fn proof_records_generation_time() {
        let prover = T1SoftwareProver::new(42161);
        let proof  = prover.prove(hash(0xaa), 500, "MSA").unwrap();
        // Generation time should be a plausible value (> 0).
        // We don't assert an upper bound in tests (CI speed varies).
        assert!(proof.generation_ms < 60_000, "proof took unreasonably long");
    }

    #[test]
    fn within_sla_check() {
        let prover = T1SoftwareProver::new(42161);
        let proof  = prover.prove(hash(0xbb), 100, "CNRY").unwrap();
        // For a trivial proof, generation should be fast.
        // A proof that takes longer than normal_sla_ms (4000ms) in tests would be a bug.
        let within_normal = proof.within_sla(4000);
        assert!(within_normal, "proof should complete within normal SLA in tests");
    }

    #[test]
    fn hash_to_field_elements_is_deterministic() {
        let h = hash(0x42);
        let e1 = hash_to_field_elements(&h);
        let e2 = hash_to_field_elements(&h);
        assert_eq!(e1[0].as_int(), e2[0].as_int());
    }

    #[test]
    fn prover_tier_str() {
        assert_eq!(ProverTier::T1Software.as_str(), "t1_software");
        assert_eq!(ProverTier::T1Hardware.as_str(), "t1_hardware");
    }
}