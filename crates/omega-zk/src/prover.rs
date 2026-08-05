// crates/omega-zk/src/prover.rs
//
// T1 Software STARK prover (spec: prover_tier = "t1_software").

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

    pub fn prove(
        &self,
        blueprint_hash: [u8; 32],
        net_profit_wei: u128,
        strategy_id: &str,
    ) -> Result<ZkProof, ZkError> {
        let started = Instant::now();

        let proof_bytes = self
            .generate_stark_proof(&blueprint_hash, net_profit_wei)
            .map_err(|e| ZkError::ProofGenerationFailed {
                blueprint_hash: hex::encode(blueprint_hash),
                detail: e.to_string(),
            })?;

        let generation_ms = started.elapsed().as_millis() as u64;
        let proof_size_bytes = proof_bytes.len();

        tracing::debug!(
            blueprint_hash = hex::encode(blueprint_hash),
            strategy = strategy_id,
            generation_ms,
            proof_size_bytes,
            "ZK proof generated"
        );

        Ok(ZkProof {
            blueprint_hash,
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
        net_profit_wei: u128,
    ) -> anyhow::Result<Vec<u8>> {
        use winterfell::{math::fields::f128::BaseElement, FieldExtension, ProofOptions, Prover};

        let pub_inputs = BlueprintPublicInputs {
            hash_commitment: hash_to_field_elements(blueprint_hash),
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

#[derive(Debug, Clone)]
pub(crate) struct BlueprintPublicInputs {
    hash_commitment: [BaseElement; 4],
    net_profit_hi: BaseElement,
    net_profit_lo: BaseElement,
    chain_id: BaseElement,
}

impl BlueprintPublicInputs {
    pub(crate) fn new(blueprint_hash: &[u8; 32], net_profit_wei: u128, chain_id: u64) -> Self {
        Self {
            hash_commitment: hash_to_field_elements(blueprint_hash),
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
            context: AirContext::new(trace_info, degrees, 7, options),
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
    let num_cols = 7;
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

    #[test]
    fn prove_produces_non_empty_proof() {
        let prover = T1SoftwareProver::new(42161);
        let proof = prover.prove(hash(0x01), 1_000_000_000, "LA").unwrap();
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
        let proof = prover.prove(hash(0xaa), 500, "MSA").unwrap();
        assert!(proof.generation_ms < 60_000, "proof took unreasonably long");
    }

    #[test]
    fn within_sla_check() {
        let prover = T1SoftwareProver::new(42161);
        let proof = prover.prove(hash(0xbb), 100, "CNRY").unwrap();
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
