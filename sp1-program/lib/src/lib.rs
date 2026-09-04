// sp1-program/lib/src/lib.rs
//
// Shared between the guest program (program/) and the host-side proving script.
// Defines the ONLY part of this whole proof that is fixed by
// SP1StarkVerifierAdapter.sol: the public-values ABI shape.
//
// SP1StarkVerifierAdapter.verify() does:
//   (bytes32 committedBlueprintHash, bytes32 committedPublicInputsHash) =
//       abi.decode(publicValues, (bytes32, bytes32));
//
// DO NOT change the field order or add fields before these two without also
// updating SP1StarkVerifierAdapter.sol's decode call.

use alloy_sol_types::sol;

sol! {
    struct PublicValuesStruct {
        bytes32 blueprintHash;
        bytes32 publicInputsHash;
        // Additional public outputs may be appended AFTER these two fields.
        // The on-chain adapter only reads the first 64 bytes.
    }
}

// ProofBundle — used by script/ (host side). Matches the adapter's
// abi.decode(proof, (bytes, bytes)).
sol! {
    struct ProofBundle {
        bytes publicValues;
        bytes proofBytes;
    }
}