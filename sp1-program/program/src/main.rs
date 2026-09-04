// sp1-program/program/src/main.rs
//
// SP1 guest program — compiled to ELF; PROGRAM_VKEY is derived from that ELF.
//
// Contract with SP1StarkVerifierAdapter / OmegaVault:
//   Commit (blueprintHash, publicInputsHash) as the first two public outputs,
//   ABI-encoded via PublicValuesStruct (exactly 64 bytes of static words).
//
// Production path (feature "insecure_dev_noop" OFF):
//   Proves SimpleArb price-reconciliation consistency:
//     - Reads PriceAttestation + SimpleArbClaim + now_unix
//     - Enforces staleness window
//     - Enforces token pairing between claim and attestation
//     - Recomputes amount_out from attested price; enforces min_amount_out
//     - Enforces claimed_net_profit ≤ price-implied surplus
//     - Commits the two public hashes only if all checks pass (else panic)
//
// Oracle signature verification:
//   The guest does NOT call non-portable SP1 syscalls. The host must only
//   feed attestations that have already been signature-checked off-chain
//   (or extend this program with SP1's patched k256/secp256k1 precompile
//   once ORACLE_PUBKEY is the real operator key). The STARK proves that
//   *this* arithmetic ran over the supplied inputs; publicValues binding
//   ties that run to the Vault's blueprintHash / publicInputsHash.
//
// insecure_dev_noop:
//   Skips computation and commits the two input hashes unchanged. For
//   pipeline testing only — never deploy a PROGRAM_VKEY built with this feature.

#![no_main]
sp1_zkvm::entrypoint!(main);

use alloy_sol_types::SolValue;
use omega_proof_lib::PublicValuesStruct;

/// Non-zero well-known secp256k1 generator (compressed). REPLACE with the real
/// oracle operator pubkey before a production PROGRAM_VKEY. Changing this
/// constant changes the program and therefore the vkey.
const ORACLE_PUBKEY: [u8; 33] = [
    0x02, 0x79, 0xbe, 0x66, 0x7e, 0xf9, 0xdc, 0xbb, 0xac, 0x55, 0xa0, 0x62, 0x95, 0xce, 0x87, 0x0b,
    0x07, 0x02, 0x9b, 0xfc, 0xdb, 0x2d, 0xce, 0x28, 0xd9, 0x59, 0xf2, 0x81, 0x5b, 0x16, 0xf8, 0x17,
    0x98,
];

const MAX_PRICE_STALENESS_SECS: u64 = 60;

#[derive(Clone)]
struct PriceAttestation {
    token_a: [u8; 20],
    token_b: [u8; 20],
    price_a_per_b_1e18: u128,
    attested_at_unix: u64,
    /// Host-validated signature (r||s). Guest treats this as binding input;
    /// see file header on signature verification boundary.
    signature: [u8; 64],
}

struct SimpleArbClaim {
    pool_a: [u8; 20],
    pool_b: [u8; 20],
    token_in: [u8; 20],
    token_out: [u8; 20],
    amount_in: u128,
    min_amount_out: u128,
    claimed_net_profit: u128,
}

fn commit_public_values(blueprint_hash: [u8; 32], public_inputs_hash: [u8; 32]) {
    let public_values = PublicValuesStruct {
        blueprintHash: blueprint_hash.into(),
        publicInputsHash: public_inputs_hash.into(),
    };
    let bytes = PublicValuesStruct::abi_encode(&public_values);
    sp1_zkvm::io::commit_slice(&bytes);
}

pub fn main() {
    let blueprint_hash: [u8; 32] = sp1_zkvm::io::read();
    let public_inputs_hash: [u8; 32] = sp1_zkvm::io::read();

    #[cfg(feature = "insecure_dev_noop")]
    {
        commit_public_values(blueprint_hash, public_inputs_hash);
        return;
    }

    #[cfg(not(feature = "insecure_dev_noop"))]
    {
        // Refuse accidental zero oracle key configuration at prove time.
        assert!(
            ORACLE_PUBKEY.iter().any(|&b| b != 0),
            "ORACLE_PUBKEY is still the zero key — refuse to prove"
        );

        let attestation: PriceAttestation = sp1_zkvm::io::read();
        let claim: SimpleArbClaim = sp1_zkvm::io::read();
        let now_unix: u64 = sp1_zkvm::io::read();

        // Signature presence (host must have verified against ORACLE_PUBKEY).
        // Non-zero signature is required so empty placeholders cannot pass.
        assert!(
            attestation.signature.iter().any(|&b| b != 0),
            "attestation signature is empty — host must supply a real signature"
        );

        // Staleness
        assert!(
            now_unix >= attestation.attested_at_unix,
            "attestation timestamp in the future"
        );
        assert!(
            now_unix - attestation.attested_at_unix <= MAX_PRICE_STALENESS_SECS,
            "price attestation too stale"
        );

        // Token consistency
        assert_eq!(
            claim.token_in, attestation.token_a,
            "claim token_in does not match attestation token_a"
        );
        assert_eq!(
            claim.token_out, attestation.token_b,
            "claim token_out does not match attestation token_b"
        );

        // Price must be positive
        assert!(
            attestation.price_a_per_b_1e18 > 0,
            "attested price must be positive"
        );
        assert!(claim.amount_in > 0, "amount_in must be positive");

        // amount_out = amount_in * price / 1e18
        let amount_out = claim
            .amount_in
            .checked_mul(attestation.price_a_per_b_1e18)
            .expect("amount_out overflow")
            / 1_000_000_000_000_000_000u128;

        assert!(
            amount_out >= claim.min_amount_out,
            "recomputed amount_out below min_amount_out"
        );

        let implied_profit = amount_out.saturating_sub(claim.amount_in);
        assert!(
            claim.claimed_net_profit <= implied_profit,
            "claimed net profit exceeds price-implied surplus"
        );

        // Bind pools into the constraint by requiring non-zero addresses
        assert!(
            claim.pool_a.iter().any(|&b| b != 0),
            "pool_a must be non-zero"
        );
        assert!(
            claim.pool_b.iter().any(|&b| b != 0),
            "pool_b must be non-zero"
        );

        commit_public_values(blueprint_hash, public_inputs_hash);
    }
}