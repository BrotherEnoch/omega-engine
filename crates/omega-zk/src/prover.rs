// crates/omega-zk/src/prover.rs
//
// T1 Software STARK prover (spec: prover_tier = "t1_software").
//
// FIX (this revision): public-inputs mismatch with OmegaVault.sol.
//
// Previously, BlueprintPublicInputs committed only (blueprint_hash, net_profit_wei,
// chain_id) -- confirmed against the real source this session. OmegaVault.sol's own
// computePublicInputsHash() requires binding
//   keccak256(abi.encode(PUBLIC_INPUTS_VERSION, address(this), blueprintHash, netProfit,
//   address(profit_token)))
// -- i.e. it also binds the Vault's own address and the profit token, neither of which
// appeared anywhere in this AIR. That's not cosmetic: address(this) exists specifically so a
// proof from one Vault deployment can't be replayed against another sharing the same
// verifier (staging vs. prod, explicitly called out in OmegaVault.sol's own comments) -- and
// as this file was written, that protection didn't actually exist at the proof layer.
//
// Rather than committing vault_address/profit_token as separate field elements (awkward
// address-chunking, and duplicates work the hash already does), this fix commits
// `public_inputs_hash` directly -- the SAME way this AIR already commits `blueprint_hash` --
// since that's the exact bytes32 value OmegaVault.computePublicInputsHash() produces and the
// exact value IStarkVerifier.verify() is handed. Smaller, more surgical change than
// restructuring the whole input set, and it mirrors the pattern already used for
// blueprint_hash rather than introducing a new one.
//
// net_profit_hi/net_profit_lo/chain_id are UNCHANGED and still committed directly, since
// other code (ZkProof consumers doing logging/simulation) may depend on reading
// proof.net_profit_wei/proof.chain_id directly rather than re-deriving them from a hash.
// This is additive, not a replacement of the existing fields.

use serde::{Deserialize, Serialize};
use std::time::Instant;

use crate::error::ZkError;

// ─── Prover tier enum ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProverTier {
    T1Software,
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

// ─── Proof output ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZkProof {
    pub blueprint_hash: [u8; 32],
    /// NEW (this fix): the exact publicInputsHash value OmegaVault.computePublicInputsHash()
    /// produced for this blueprint -- committed by the AIR alongside blueprint_hash. Callers
    /// (off-chain simulation, or whatever eventually implements IStarkVerifier.verify() for
    /// the chosen on-chain path) must pass this to ZkVerifier::verify() as the value to check
    /// the proof against, not derive it independently -- see verifier.rs.
    pub public_inputs_hash: [u8; 32],
    pub net_profit_wei: u128,
    pub chain_id: u64,
    pub strategy_id: String,
    pub proof_bytes: Vec<u8>,
    pub prover_tier: ProverTier,
    pub generation_ms: u64,
    pub proof_size_bytes: usize,
}

impl ZkProof {
    pub fn within_sla(&self, sla_ms: u64) -> bool {
        self.generation_ms <= sla_ms
    }
}

// ─── T1 Software prover ───────────────────────────────────────────────────────

#[derive(Debug)]
pub struct T1SoftwareProver {
    chain_id: u64,
}

impl T1SoftwareProver {
    pub fn new(chain_id: u64) -> Self {
        Self { chain_id }
    }

    /// NEW PARAM (this fix): `public_inputs_hash` -- the caller (whatever assembles the
    /// blueprint, presumably reading OmegaVault.computePublicInputsHash() or recomputing the
    /// identical formula off-chain) must supply the real value here. Passing a value that
    /// doesn't match what OmegaVault will actually compute produces a proof that verifies
    /// successfully but is bound to the wrong tuple -- this function has no way to check that
    /// correctness itself, since it doesn't have access to the Vault's on-chain state.
    pub fn prove(
        &self,
        blueprint_hash: [u8; 32],
        public_inputs_hash: [u8; 32],
        net_profit_wei: u128,
        strategy_id: &str,
    ) -> Result<ZkProof, ZkError> {
        let started = Instant::now();

        let proof_bytes = self
            .generate_stark_proof(&blueprint_hash, &public_inputs_hash, net_profit_wei)
            .map_err(|e| ZkError::ProofGenerationFailed {
                blueprint_hash: hex::encode(blueprint_hash),
                detail: e.to_string(),
            })?;

        let generation_ms = started.elapsed().as_millis() as u64;
        let proof_size_bytes = proof_bytes.len();

        tracing::debug!(
            blueprint_hash = hex::encode(blueprint_hash),
            public_inputs_hash = hex::encode(public_inputs_hash),
            strategy = strategy_id,
            generation_ms,
            proof_size_bytes,
            "ZK proof generated"
        );

        Ok(ZkProof {
            blueprint_hash,
            public_inputs_hash,
            net_profit_wei,
            chain_id: self.chain_id,
            strategy_id: strategy_id.to_string(),
            proof_bytes,
            prover_tier: ProverTier::T1Software,
            generation_ms,
            proof_size_bytes,
        })
    }

    fn generate_stark_proof(
        &self,
        blueprint_hash: &[u8; 32],
        public_inputs_hash: &[u8; 32],
        net_profit_wei: u128,
    ) -> anyhow::Result<Vec<u8>> {
        use winterfell::{math::fields::f128::BaseElement, FieldExtension, ProofOptions, Prover};

        let pub_inputs = BlueprintPublicInputs {
            hash_commitment: hash_to_field_elements(blueprint_hash),
            public_inputs_hash_commitment: hash_to_field_elements(public_inputs_hash),
            net_profit_hi: BaseElement::new(net_profit_wei >> 64),
            net_profit_lo: BaseElement::new(net_profit_wei as u64 as u128),
            chain_id: BaseElement::new(self.chain_id as u128),
        };

        let trace = build_blueprint_trace(&pub_inputs);

        let options = ProofOptions::new(28, 8, 0, FieldExtension::None, 8, 127);

        let prover = BlueprintProver {
            options,
            pub_inputs: pub_inputs.clone(),
        };
        let proof = prover
            .prove(trace)
            .map_err(|e| anyhow::anyhow!("Winterfell prove() failed: {e:?}"))?;

        Ok(proof.to_bytes())
    }
}

// ─── Winterfell AIR types ─────────────────────────────────────────────────────

use winterfell::{
    crypto::{hashers::Blake3_256, DefaultRandomCoin},
    math::{fields::f128::BaseElement, FieldElement, ToElements},
    Air, AirContext, Assertion, AuxTraceRandElements, ConstraintCompositionCoefficients,
    DefaultConstraintEvaluator, DefaultTraceLde, EvaluationFrame, ProofOptions, Prover, TraceInfo,
    TracePolyTable, TraceTable, TransitionConstraintDegree,
};

/// Number of trace columns / committed public-input field elements. Was 7 before this fix
/// (4 for blueprint_hash + net_profit_hi + net_profit_lo + chain_id); now 11
/// (+ 4 for public_inputs_hash_commitment). Defined once as a constant so
/// build_blueprint_trace, BlueprintAir::new, and get_assertions can't silently drift out of
/// sync with each other the way the original 7 was implicitly repeated in three places.
pub(crate) const NUM_TRACE_COLUMNS: usize = 11;

#[derive(Debug, Clone)]
pub(crate) struct BlueprintPublicInputs {
    hash_commitment: [BaseElement; 4],
    /// NEW (this fix): commits publicInputsHash the same way hash_commitment commits
    /// blueprint_hash. See file header for why this closes the OmegaVault mismatch.
    public_inputs_hash_commitment: [BaseElement; 4],
    net_profit_hi: BaseElement,
    net_profit_lo: BaseElement,
    chain_id: BaseElement,
}

impl BlueprintPublicInputs {
    pub(crate) fn new(
        blueprint_hash: &[u8; 32],
        public_inputs_hash: &[u8; 32],
        net_profit_wei: u128,
        chain_id: u64,
    ) -> Self {
        Self {
            hash_commitment: hash_to_field_elements(blueprint_hash),
            public_inputs_hash_commitment: hash_to_field_elements(public_inputs_hash),
            net_profit_hi: BaseElement::new(net_profit_wei >> 64),
            net_profit_lo: BaseElement::new(net_profit_wei as u64 as u128),
            chain_id: BaseElement::new(chain_id as u128),
        }
    }
}

impl ToElements<BaseElement> for BlueprintPublicInputs {
    fn to_elements(&self) -> Vec<BaseElement> {
        let mut v = vec![self.net_profit_hi, self.net_profit_lo, self.chain_id];
        v.extend_from_slice(&self.hash_commitment);
        v.extend_from_slice(&self.public_inputs_hash_commitment);
        v
    }
}

pub(crate) struct BlueprintAir {
    context: AirContext<BaseElement>,
    pub_inputs: BlueprintPublicInputs,
}

impl Air for BlueprintAir {
    type BaseField = BaseElement;
    type PublicInputs = BlueprintPublicInputs;

    fn new(
        trace_info: TraceInfo,
        pub_inputs: BlueprintPublicInputs,
        options: ProofOptions,
    ) -> Self {
        let degrees = vec![TransitionConstraintDegree::new(1)];
        Self {
            // Was hardcoded 7 -- now NUM_TRACE_COLUMNS (11), matching the real number of
            // assertions returned by get_assertions() below. This argument is the assertion
            // count Winterfell uses for composition-degree accounting; leaving it out of sync
            // with the actual assertion count is exactly the kind of silent-drift bug this
            // constant exists to prevent.
            context: AirContext::new(trace_info, degrees, NUM_TRACE_COLUMNS, options),
            pub_inputs,
        }
    }

    fn context(&self) -> &AirContext<Self::BaseField> {
        &self.context
    }

    fn evaluate_transition<E: FieldElement<BaseField = Self::BaseField>>(
        &self,
        frame: &EvaluationFrame<E>,
        _periodic: &[E],
        result: &mut [E],
    ) {
        // Unchanged: column 0 is still the only evolving column (counter), exactly as before
        // this fix. All newly-added columns (7-10, public_inputs_hash_commitment) are
        // constant-per-row, same treatment as the existing chain_id/hash_commitment columns.
        result[0] = frame.next()[0] - frame.current()[0] - E::ONE;
    }

    fn get_assertions(&self) -> Vec<Assertion<Self::BaseField>> {
        vec![
            Assertion::single(0, 0, self.pub_inputs.net_profit_hi),
            Assertion::single(1, 0, self.pub_inputs.net_profit_lo),
            Assertion::single(2, 0, self.pub_inputs.chain_id),
            Assertion::single(3, 0, self.pub_inputs.hash_commitment[0]),
            Assertion::single(4, 0, self.pub_inputs.hash_commitment[1]),
            Assertion::single(5, 0, self.pub_inputs.hash_commitment[2]),
            Assertion::single(6, 0, self.pub_inputs.hash_commitment[3]),
            // NEW (this fix): columns 7-10 bind public_inputs_hash_commitment, mirroring
            // exactly how columns 3-6 bind hash_commitment above.
            Assertion::single(7, 0, self.pub_inputs.public_inputs_hash_commitment[0]),
            Assertion::single(8, 0, self.pub_inputs.public_inputs_hash_commitment[1]),
            Assertion::single(9, 0, self.pub_inputs.public_inputs_hash_commitment[2]),
            Assertion::single(10, 0, self.pub_inputs.public_inputs_hash_commitment[3]),
        ]
    }
}

pub(crate) struct BlueprintProver {
    options: ProofOptions,
    pub_inputs: BlueprintPublicInputs,
}

impl Prover for BlueprintProver {
    type BaseField = BaseElement;
    type Air = BlueprintAir;
    type Trace = TraceTable<BaseElement>;
    type HashFn = Blake3_256<BaseElement>;
    type RandomCoin = DefaultRandomCoin<Self::HashFn>;
    type TraceLde<E>
        = DefaultTraceLde<E, Self::HashFn>
    where
        E: FieldElement<BaseField = Self::BaseField>;
    type ConstraintEvaluator<'a, E>
        = DefaultConstraintEvaluator<'a, Self::Air, E>
    where
        E: FieldElement<BaseField = Self::BaseField>;

    fn get_pub_inputs(&self, _trace: &Self::Trace) -> BlueprintPublicInputs {
        self.pub_inputs.clone()
    }

    fn options(&self) -> &ProofOptions {
        &self.options
    }

    fn new_trace_lde<E>(
        &self,
        trace_info: &TraceInfo,
        main_trace: &winterfell::matrix::ColMatrix<Self::BaseField>,
        domain: &winterfell::StarkDomain<Self::BaseField>,
    ) -> (Self::TraceLde<E>, TracePolyTable<E>)
    where
        E: FieldElement<BaseField = Self::BaseField>,
    {
        DefaultTraceLde::<E, Self::HashFn>::new(trace_info, main_trace, domain)
    }

    fn new_evaluator<'a, E>(
        &self,
        air: &'a Self::Air,
        aux_rand_elements: AuxTraceRandElements<E>,
        composition_coefficients: ConstraintCompositionCoefficients<E>,
    ) -> Self::ConstraintEvaluator<'a, E>
    where
        E: FieldElement<BaseField = Self::BaseField>,
    {
        DefaultConstraintEvaluator::new(air, aux_rand_elements, composition_coefficients)
    }
}

// ─── Trace construction ───────────────────────────────────────────────────────

pub(crate) fn build_blueprint_trace(inputs: &BlueprintPublicInputs) -> TraceTable<BaseElement> {
    let trace_length = 64;
    // Was hardcoded 7 -- now NUM_TRACE_COLUMNS, see that constant's own doc comment.
    let num_cols = NUM_TRACE_COLUMNS;
    let mut trace = TraceTable::new(num_cols, trace_length);

    trace.fill(
        |state| {
            state[0] = inputs.net_profit_hi;
            state[1] = inputs.net_profit_lo;
            state[2] = inputs.chain_id;
            state[3] = inputs.hash_commitment[0];
            state[4] = inputs.hash_commitment[1];
            state[5] = inputs.hash_commitment[2];
            state[6] = inputs.hash_commitment[3];
            // NEW (this fix): columns 7-10, mirroring columns 3-6.
            state[7] = inputs.public_inputs_hash_commitment[0];
            state[8] = inputs.public_inputs_hash_commitment[1];
            state[9] = inputs.public_inputs_hash_commitment[2];
            state[10] = inputs.public_inputs_hash_commitment[3];
        },
        |_step, state| {
            // Keep one column evolving so Winterfell builds a non-degenerate
            // trace composition polynomial; the remaining columns stay bound
            // to the public inputs through the first-step assertions.
            state[0] += BaseElement::ONE;
        },
    );

    trace
}

// ─── Hash helper ─────────────────────────────────────────────────────────────

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
    // StarkField must be in scope for .as_int() — fixes E0599
    use winterfell::math::StarkField;

    fn hash(b: u8) -> [u8; 32] {
        [b; 32]
    }

    // NEW (this fix): a second, distinct 32-byte value standing in for publicInputsHash in
    // tests -- distinct from blueprint_hash so tests can't accidentally pass if the two
    // commitments were swapped or aliased.
    fn pih(b: u8) -> [u8; 32] {
        [b; 32]
    }

    #[test]
    fn prove_produces_non_empty_proof() {
        let prover = T1SoftwareProver::new(42161);
        let proof = prover
            .prove(hash(0x01), pih(0x11), 1_000_000_000, "LA")
            .unwrap();
        assert!(!proof.proof_bytes.is_empty());
        assert_eq!(proof.blueprint_hash, hash(0x01));
        assert_eq!(proof.public_inputs_hash, pih(0x11));
        assert_eq!(proof.net_profit_wei, 1_000_000_000);
        assert_eq!(proof.chain_id, 42161);
        assert_eq!(proof.strategy_id, "LA");
        assert_eq!(proof.prover_tier, ProverTier::T1Software);
    }

    #[test]
    fn different_inputs_produce_different_proofs() {
        let prover = T1SoftwareProver::new(42161);
        let p1 = prover.prove(hash(0x01), pih(0x11), 1_000, "SA").unwrap();
        let p2 = prover.prove(hash(0x02), pih(0x11), 1_000, "SA").unwrap();
        assert_ne!(p1.proof_bytes, p2.proof_bytes);
    }

    // NEW (this fix): the specific regression this whole change is meant to catch -- same
    // blueprint_hash, different public_inputs_hash, must produce a different proof. Before
    // this fix, this test would have been impossible to write meaningfully because
    // public_inputs_hash didn't exist as an input at all; two proofs differing only in the
    // Vault address or profit token they were meant to be scoped to would have been
    // byte-for-byte identical.
    #[test]
    fn same_blueprint_hash_different_public_inputs_hash_produce_different_proofs() {
        let prover = T1SoftwareProver::new(42161);
        let p1 = prover.prove(hash(0x01), pih(0x11), 1_000, "SA").unwrap();
        let p2 = prover.prove(hash(0x01), pih(0x22), 1_000, "SA").unwrap();
        assert_ne!(p1.proof_bytes, p2.proof_bytes);
    }

    #[test]
    fn proof_records_generation_time() {
        let prover = T1SoftwareProver::new(42161);
        let proof = prover.prove(hash(0xaa), pih(0xbb), 500, "MSA").unwrap();
        assert!(proof.generation_ms < 60_000, "proof took unreasonably long");
    }

    #[test]
    fn within_sla_check() {
        let prover = T1SoftwareProver::new(42161);
        let proof = prover.prove(hash(0xbb), pih(0xcc), 100, "CNRY").unwrap();
        assert!(
            proof.within_sla(4000),
            "proof should complete within normal SLA in tests"
        );
    }

    #[test]
    fn hash_to_field_elements_is_deterministic() {
        let h = hash(0x42);
        let e1 = hash_to_field_elements(&h);
        let e2 = hash_to_field_elements(&h);
        // .as_int() requires StarkField in scope (imported above)
        assert_eq!(e1[0].as_int(), e2[0].as_int());
    }

    #[test]
    fn prover_tier_str() {
        assert_eq!(ProverTier::T1Software.as_str(), "t1_software");
        assert_eq!(ProverTier::T1Hardware.as_str(), "t1_hardware");
    }
}