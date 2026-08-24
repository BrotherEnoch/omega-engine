// sp1-program/lib/src/lib.rs
//
// Shared between the guest program (program/) and whatever host-side proving script you
// build later. Defines the ONLY part of this whole proof that's actually fixed by
// SP1StarkVerifierAdapter.sol: the public-values ABI shape.
//
// SP1StarkVerifierAdapter.verify() does:
//   (bytes32 committedBlueprintHash, bytes32 committedPublicInputsHash) =
//       abi.decode(publicValues, (bytes32, bytes32));
//
// abi.decode with a plain (bytes32, bytes32) tuple type expects two consecutive 32-byte
// words with no offset/length header (both are static types) -- exactly what
// PublicValuesStruct::abi_encode() below produces, confirmed against alloy_sol_types'
// actual behavior (SolValue::abi_encode on a struct of static fields = standard Solidity
// struct/tuple encoding, no dynamic-type offset machinery involved).
//
// DO NOT change the field order or add fields before these two without also updating
// SP1StarkVerifierAdapter.sol's decode call to match -- they're two halves of one contract.
// Fields AFTER these two are fine to add; the adapter only reads the first 64 bytes.

use alloy_sol_types::sol;

sol! {
    struct PublicValuesStruct {
        bytes32 blueprintHash;
        bytes32 publicInputsHash;
        // TODO: any additional public outputs your actual proof needs to expose go here,
        // AFTER these two fields. Left empty because what this program computes hasn't been
        // decided yet -- see the open question in program/src/main.rs.
    }
}

// -------------------------------------------------------------------------------------------
// ProofBundle -- used by script/ (host side), NOT the guest program. This is the OTHER half
// of the ABI contract: SP1StarkVerifierAdapter.verify()'s `proof` parameter is
// abi.decode(proof, (bytes, bytes)) -- a plain two-element tuple of dynamic bytes, which is
// ABI-identical to a struct of two `bytes` fields (Solidity structs encode as tuples; there is
// no separate "struct" wire format). Defining it here as a named struct rather than encoding a
// raw (Vec<u8>, Vec<u8>) tuple directly in script/ exists purely for readability/type-safety on
// the Rust side -- the on-chain bytes it produces are identical either way.
//
// Field order matters and must stay (publicValues, proofBytes) to match the adapter's decode.
sol! {
    struct ProofBundle {
        bytes publicValues;
        bytes proofBytes;
    }
}