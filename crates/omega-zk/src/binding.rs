// crates/omega-zk/src/binding.rs
//
// Computes the OFF-CHAIN mirror of OmegaVault.computePublicInputsHash() — MUST match that
// Solidity function's exact encoding, byte for byte, or every proof this produces will bind
// to a hash OmegaVault never actually computes for the same blueprint, and
// OmegaVault.submitProof() will always revert with Gate_ProofInputsMismatch on-chain.
//
// OmegaVault.sol's real formula (confirmed against that file's own source, provided earlier
// in this investigation):
//
//   keccak256(abi.encode(PUBLIC_INPUTS_VERSION, address(this), blueprintHash, netProfit,
//   address(profit_token)))
//
// Encoding rule for abi.encode() over an ALL-STATIC-TYPE tuple
// (uint256, address, bytes32, uint256, address): no dynamic types are present, so there is
// no offset/length header machinery to replicate — it is exactly the concatenation of each
// argument's canonical 32-byte word, in argument order. This is standard, version-
// independent Solidity ABI behavior (not a library-specific detail), reasoned from the ABI
// spec itself. FLAGGED, NOT INDEPENDENTLY VERIFIED BY EXECUTION: no Rust toolchain was
// available in the environment this was written in, so this has not been run and
// cross-checked against real `cast abi-encode` / Solidity output. Verify with something like:
//
//   cast abi-encode "f(uint256,address,bytes32,uint256,address)" 1 <vault> <bp_hash> \
//     <net_profit> <token>
//   cast keccak <the output above>
//
// and compare against compute_public_inputs_hash's output for the same inputs, before
// trusting this in production.
//
// Word layout:
//   uint256 PUBLIC_INPUTS_VERSION -> 32-byte big-endian
//   address vault_address          -> 12 zero bytes + 20 address bytes (left-padded)
//   bytes32 blueprint_hash          -> 32 raw bytes (already exactly 32 bytes, no padding)
//   uint256 net_profit_wei          -> 32-byte big-endian. Stored as u128 here, matching
//                                      ZkProof's existing field type (prover.rs) — a
//                                      pre-existing narrower-than-uint256 constraint already
//                                      present in this codebase's Rust side, not introduced
//                                      by this file. A u128 value converts losslessly into a
//                                      uint256 word (zero-extended into the high 16 bytes).
//   address profit_token            -> 12 zero bytes + 20 address bytes (left-padded)
//
// PUBLIC_INPUTS_VERSION is hardcoded to 1 here, matching OmegaVault.sol's own
// `uint256 public constant PUBLIC_INPUTS_VERSION = 1;`. If that constant is ever bumped on
// the Solidity side, THIS constant must be bumped in lockstep, or every proof produced here
// will silently bind to the wrong (stale) version's hash and every submitProof() call will
// revert with Gate_ProofInputsMismatch against a live Vault running the new version.

use sha3::{Digest, Keccak256};

/// Must match `OmegaVault.PUBLIC_INPUTS_VERSION` exactly — see file header.
pub const PUBLIC_INPUTS_VERSION: u64 = 1;

/// Computes the exact `publicInputsHash` value `OmegaVault.computePublicInputsHash()`
/// produces on-chain, for the same four real inputs.
///
/// # Arguments
/// * `vault_address`   - the deployed OmegaVault's own address (`address(this)` on-chain).
/// * `blueprint_hash`  - this blueprint's `blueprint_hash` (already 32 bytes).
/// * `net_profit_wei`  - the net profit this blueprint claims, in wei.
/// * `profit_token`    - the ERC20 address OmegaVault's immutable `profit_token` is set to.
pub fn compute_public_inputs_hash(
    vault_address: [u8; 20],
    blueprint_hash: [u8; 32],
    net_profit_wei: u128,
    profit_token: [u8; 20],
) -> [u8; 32] {
    let mut buf = Vec::with_capacity(160);

    // PUBLIC_INPUTS_VERSION as uint256
    let mut version_word = [0u8; 32];
    version_word[24..32].copy_from_slice(&PUBLIC_INPUTS_VERSION.to_be_bytes());
    buf.extend_from_slice(&version_word);

    // vault_address as address (left-padded to 32 bytes)
    let mut vault_word = [0u8; 32];
    vault_word[12..32].copy_from_slice(&vault_address);
    buf.extend_from_slice(&vault_word);

    // blueprint_hash as bytes32 (already exactly 32 bytes — no padding needed)
    buf.extend_from_slice(&blueprint_hash);

    // net_profit_wei as uint256 (u128 zero-extends into the low 16 bytes of the word)
    let mut profit_word = [0u8; 32];
    profit_word[16..32].copy_from_slice(&net_profit_wei.to_be_bytes());
    buf.extend_from_slice(&profit_word);

    // profit_token as address (left-padded to 32 bytes)
    let mut token_word = [0u8; 32];
    token_word[12..32].copy_from_slice(&profit_token);
    buf.extend_from_slice(&token_word);

    debug_assert_eq!(buf.len(), 160, "five 32-byte words must total exactly 160 bytes");

    let mut hasher = Keccak256::new();
    hasher.update(&buf);
    let result = hasher.finalize();

    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

#[cfg(test)]
mod binding_tests {
    use super::*;

    #[test]
    fn deterministic() {
        let h1 = compute_public_inputs_hash([0x11; 20], [0x22; 32], 1_000, [0x33; 20]);
        let h2 = compute_public_inputs_hash([0x11; 20], [0x22; 32], 1_000, [0x33; 20]);
        assert_eq!(h1, h2);
    }

    /// The exact cross-deployment binding property C4 (OmegaVault.sol) exists for — a
    /// different vault_address must produce a different hash, or a proof from one Vault
    /// deployment could be replayed against another sharing the same verifier.
    #[test]
    fn different_vault_address_produces_different_hash() {
        let h1 = compute_public_inputs_hash([0x11; 20], [0x22; 32], 1_000, [0x33; 20]);
        let h2 = compute_public_inputs_hash([0x99; 20], [0x22; 32], 1_000, [0x33; 20]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn different_profit_token_produces_different_hash() {
        let h1 = compute_public_inputs_hash([0x11; 20], [0x22; 32], 1_000, [0x33; 20]);
        let h2 = compute_public_inputs_hash([0x11; 20], [0x22; 32], 1_000, [0x99; 20]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn different_blueprint_hash_produces_different_hash() {
        let h1 = compute_public_inputs_hash([0x11; 20], [0x22; 32], 1_000, [0x33; 20]);
        let h2 = compute_public_inputs_hash([0x11; 20], [0x44; 32], 1_000, [0x33; 20]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn different_net_profit_produces_different_hash() {
        let h1 = compute_public_inputs_hash([0x11; 20], [0x22; 32], 1_000, [0x33; 20]);
        let h2 = compute_public_inputs_hash([0x11; 20], [0x22; 32], 2_000, [0x33; 20]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn zero_net_profit_does_not_panic_or_collide_with_nonzero() {
        let h1 = compute_public_inputs_hash([0x11; 20], [0x22; 32], 0, [0x33; 20]);
        let h2 = compute_public_inputs_hash([0x11; 20], [0x22; 32], 1, [0x33; 20]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn max_u128_net_profit_does_not_panic() {
        let _ = compute_public_inputs_hash([0x11; 20], [0x22; 32], u128::MAX, [0x33; 20]);
    }

    /// Independent keccak oracle (Python Crypto.Hash.keccak over the same 160-byte
    /// abi.encode layout). Pins the binding against a second implementation.
    #[test]
    fn matches_independent_keccak_oracle() {
        let got = compute_public_inputs_hash([0x11; 20], [0x22; 32], 1_000, [0x33; 20]);
        let expected = hex::decode(
            "431f54a4255bab0ac74cc0b392917f879f655d25422afd0b9ca28dba931182a5",
        )
        .unwrap();
        assert_eq!(got.as_slice(), expected.as_slice());
    }
}