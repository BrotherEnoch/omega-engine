// sp1-program/program/src/candidates/price_reconciliation_simplearb.rs
//
// UNSAFE REFERENCE ONLY — not part of the guest binary and not a production
// verifier. This arithmetic helper does not authenticate oracle signatures,
// bind the claim to OmegaOrchestrator calldata, or account for execution costs.
// The production guest is fail-closed until those properties are implemented.

use alloy_sol_types::SolValue;
use omega_proof_lib::PublicValuesStruct;

/// Maximum age (seconds) a price attestation may have before rejection.
pub const MAX_PRICE_STALENESS_SECS: u64 = 60;

#[derive(Clone, Debug)]
pub struct PriceAttestation {
    pub token_a: [u8; 20],
    pub token_b: [u8; 20],
    /// Price of token_a in terms of token_b, scaled by 1e18.
    pub price_a_per_b_1e18: u128,
    pub attested_at_unix: u64,
    /// 64-byte compact secp256k1 signature (r || s).
    pub signature: [u8; 64],
}

#[derive(Clone, Debug)]
pub struct SimpleArbClaim {
    pub pool_a: [u8; 20],
    pub pool_b: [u8; 20],
    pub token_in: [u8; 20],
    pub token_out: [u8; 20],
    pub amount_in: u128,
    pub min_amount_out: u128,
    pub claimed_net_profit: u128,
}

/// Host-side helper that performs the same checks the guest will run.
pub fn validate_claim(
    attestation: &PriceAttestation,
    claim: &SimpleArbClaim,
    now_unix: u64,
) -> Result<(), &'static str> {
    if now_unix < attestation.attested_at_unix {
        return Err("attestation timestamp in the future");
    }
    if now_unix - attestation.attested_at_unix > MAX_PRICE_STALENESS_SECS {
        return Err("price attestation too stale");
    }
    if claim.token_in != attestation.token_a {
        return Err("token_in mismatch");
    }
    if claim.token_out != attestation.token_b {
        return Err("token_out mismatch");
    }

    let amount_out = claim
        .amount_in
        .checked_mul(attestation.price_a_per_b_1e18)
        .ok_or("amount_out overflow")?
        / 1_000_000_000_000_000_000u128;

    if amount_out < claim.min_amount_out {
        return Err("recomputed amount_out below min");
    }
    let implied = amount_out.saturating_sub(claim.amount_in);
    if claim.claimed_net_profit > implied {
        return Err("claimed profit exceeds implied surplus");
    }
    Ok(())
}

/// Encode the two public values the on-chain adapter expects.
pub fn encode_public_values(
    blueprint_hash: [u8; 32],
    public_inputs_hash: [u8; 32],
) -> Vec<u8> {
    let pv = PublicValuesStruct {
        blueprintHash: blueprint_hash.into(),
        publicInputsHash: public_inputs_hash.into(),
    };
    PublicValuesStruct::abi_encode(&pv)
}

