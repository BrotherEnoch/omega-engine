// sp1-program/program/src/candidates/price_reconciliation_simplearb.rs
//
// ============================================================================================
// STATUS: PROPOSAL. NOT WIRED INTO main.rs. NOT CONFIRMED AS THE ANSWER.
// ============================================================================================
// This is one concrete, fully-worked candidate for "what does this program prove" -- the
// "independent re-simulation against canonical price data" option from the list several turns
// back. It exists so you have something specific to react to instead of an abstract menu.
// If this is the direction: say so, and I'll wire it into main.rs for real (replacing the
// todo!() there), delete this file, and extend the same pattern to LiquidationArb/
// MultiStepArb/MevOfa, which this does NOT cover. If it's wrong: tell me what's wrong with it
// specifically -- that's more useful to me than "no."
//
// THE CLAIM THIS PROGRAM WOULD PROVE, STATED PLAINLY:
//   "A SimpleArb execution with these parameters (pool_a, pool_b, token_in, token_out,
//   amount_in) and this claimed net_profit is consistent with an independently-recomputed
//   swap output, using a price attested by a specific trusted oracle key, within a bounded
//   staleness window -- i.e. the claimed profit is not larger than canonical off-chain pricing
//   would justify, even if the on-chain pool price was manipulated at execution time."
//
// WHAT THIS DOES NOT COVER, EVEN IF ADOPTED AS-IS:
//   - LiquidationArb, MultiStepArb, MevOfa need their own analogous logic -- not written here.
//   - Real AMM curves vary (constant-product vs. concentrated liquidity vs. StableSwap); the
//     recompute_expected_output() below assumes plain constant-product (x*y=k), which is
//     probably wrong for pool_a/pool_b in general -- SimpleArb.sol's own docstring says
//     pool_a/pool_b are YOUR OWN adapter contracts with a custom swap() signature, not raw
//     Uniswap V2 pairs, so their actual pricing curve is whatever those adapters implement,
//     which I have no source for either.
//   - WHO the trusted oracle signer actually is, and how its private key is operated/secured,
//     is a real key-management decision with its own security surface -- not addressed here.
//   - I have NOT independently verified the exact SP1 precompile-acceleration setup for
//     secp256k1 (the k256 signature verification below needs SP1's patched crate wired in via
//     `[patch.crates-io]` for it to run at reasonable proving cost) -- flagged inline below.
//
// WHY constant-product AND a signed-price-oracle model specifically, rather than something
// else: this was the most concrete of the candidates raised earlier and the most tractable to
// draft correctly without inventing your actual trust model from nothing -- a MPT/state-proof
// based approach (proving directly against a block's real state root) would be more powerful
// and require no trusted signer at all, but is a substantially heavier build (effectively a
// mini stateless-client verifier inside the guest) and not something to sketch casually. If
// you'd rather go that direction, say so explicitly -- it's a different and larger project.
// ============================================================================================

use alloy_sol_types::SolValue;
use omega_proof_lib::PublicValuesStruct;

/// Trusted oracle signer's public key, baked in at build time. CHANGING THIS VALUE CHANGES
/// THE PROGRAM AND THEREFORE PROGRAM_VKEY -- this is a real deployment parameter, not a
/// placeholder to leave as zero. I have no basis to pick a real value; this is intentionally
/// left as an unfilled constant.
const ORACLE_PUBKEY: [u8; 33] = [0u8; 33]; // TODO: real compressed secp256k1 pubkey, 33 bytes

/// Maximum age (in seconds) a price attestation may be before it's rejected as stale.
/// Chosen value is illustrative, not a recommendation -- how stale is "too stale" is a real
/// risk parameter that trades off oracle availability against manipulation window, and depends
/// on your actual block times and MEV latency assumptions.
const MAX_PRICE_STALENESS_SECS: u64 = 60; // TODO: confirm or change

/// A signed price attestation for a token pair, structurally analogous to what a Chainlink-
/// style push oracle or your own internal price-signing service would produce. Exact wire
/// format assumed here, not confirmed against any real oracle you're using.
pub struct PriceAttestation {
    pub token_a: [u8; 20],
    pub token_b: [u8; 20],
    /// Price of token_a in terms of token_b, scaled by 1e18 -- an assumed convention, not
    /// confirmed against your actual price-feed format.
    pub price_a_per_b_1e18: u128,
    pub attested_at_unix: u64,
    /// 64-byte compact secp256k1 signature (r || s) over
    /// keccak256(abi.encode(token_a, token_b, price_a_per_b_1e18, attested_at_unix)) --
    /// assumed message format, not confirmed.
    pub signature: [u8; 64],
}

pub struct SimpleArbClaim {
    pub pool_a: [u8; 20],
    pub pool_b: [u8; 20],
    pub token_in: [u8; 20],
    pub token_out: [u8; 20],
    pub amount_in: u128,
    pub claimed_net_profit: u128,
    pub current_block_timestamp: u64,
}

pub fn run(
    blueprint_hash: [u8; 32],
    public_inputs_hash: [u8; 32],
    claim: SimpleArbClaim,
    price: PriceAttestation,
) {
    // -- 1. Verify the price attestation is signed by the trusted oracle key ------------------
    // FLAGGED, NOT INDEPENDENTLY CONFIRMED: for this to run at reasonable proving cost rather
    // than an extremely slow generic-EC-arithmetic path, SP1's patched `k256` crate needs to
    // be wired in via a `[patch.crates-io]` entry pointing at Succinct's fork (their docs
    // describe this "patched crates for precompile acceleration" pattern; I have not
    // independently verified the exact patch coordinates/version to pin here -- check
    // https://docs.succinct.xyz for the current patch table before relying on this compiling
    // efficiently, or at all).
    let message = build_attestation_message(&price);
    let sig_valid = verify_oracle_signature(&ORACLE_PUBKEY, &message, &price.signature);
    assert!(sig_valid, "price attestation signature invalid");

    // -- 2. Freshness check ---------------------------------------------------------------------
    let age = claim
        .current_block_timestamp
        .saturating_sub(price.attested_at_unix);
    assert!(age <= MAX_PRICE_STALENESS_SECS, "price attestation stale");

    // -- 3. Confirm the attestation is actually for the token pair being traded ----------------
    assert!(
        (price.token_a == claim.token_in && price.token_b == claim.token_out)
            || (price.token_a == claim.token_out && price.token_b == claim.token_in),
        "price attestation token pair mismatch"
    );

    // -- 4. Recompute expected output using canonical price, independent of pool_a/pool_b's
    //    actual on-chain state at execution time -------------------------------------------
    // NOTE: assumes constant-product-equivalent pricing via the attested price directly
    // (amount_out = amount_in * price), i.e. treats the attested price as the execution price
    // with no slippage modeled -- see file header on why this is almost certainly wrong for
    // your actual pool_a/pool_b adapters and needs real curve logic, not this simplification.
    let expected_amount_out = recompute_expected_output(&claim, &price);

    // -- 5. The actual profit check: claimed profit must not exceed what canonical pricing
    //    would justify. This is the actual security property -- everything above exists to
    //    make this comparison trustworthy. --------------------------------------------------
    assert!(
        u128::from(claim.claimed_net_profit) <= expected_amount_out,
        "claimed profit exceeds canonically-priced expected output"
    );

    // -- 6. Only commit if every check above passed. A failed assert! aborts the guest program
    //    entirely -- no proof can be generated for a run that panics, which is the correct
    //    behavior here: an unprovable claim should not become a "valid" proof of anything. --
    let public_values = PublicValuesStruct {
        blueprintHash: blueprint_hash.into(),
        publicInputsHash: public_inputs_hash.into(),
    };
    let bytes = PublicValuesStruct::abi_encode(&public_values);
    sp1_zkvm::io::commit_slice(&bytes);
}

fn build_attestation_message(price: &PriceAttestation) -> [u8; 32] {
    // TODO: real message format must match whatever your actual oracle signs. This is a
    // placeholder keccak-style construction, not confirmed against any real signer.
    use tiny_keccak::{Hasher, Keccak};
    let mut hasher = Keccak::v256();
    hasher.update(&price.token_a);
    hasher.update(&price.token_b);
    hasher.update(&price.price_a_per_b_1e18.to_be_bytes());
    hasher.update(&price.attested_at_unix.to_be_bytes());
    let mut out = [0u8; 32];
    hasher.finalize(&mut out);
    out
}

fn verify_oracle_signature(pubkey: &[u8; 33], message: &[u8; 32], sig: &[u8; 64]) -> bool {
    // TODO: real secp256k1 verification via SP1's patched k256 crate -- see the flagged note
    // above. Left as a stub returning false (fails closed) rather than a fabricated "always
    // true" implementation, which would be strictly worse than the current todo!() in main.rs:
    // it would compile, look complete, and silently prove nothing while appearing to check
    // something.
    let _ = (pubkey, message, sig);
    false
}

fn recompute_expected_output(claim: &SimpleArbClaim, price: &PriceAttestation) -> u128 {
    // TODO: real pricing/curve logic -- see file header. Placeholder linear conversion only,
    // fails closed (returns 0, which will always fail the profit check above) rather than
    // fabricating a curve that looks plausible but isn't validated against pool_a/pool_b's
    // real behavior.
    let _ = (claim, price);
    0
}