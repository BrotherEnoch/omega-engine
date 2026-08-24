// sp1-program/program/src/main.rs
//
// SP1 guest program -- runs inside the zkVM, gets compiled to an ELF, and is what
// PROGRAM_VKEY (in DeployCore.s.sol / SP1StarkVerifierAdapter.sol) is derived from.
//
// ============================================================================================
// OPEN QUESTION THIS FILE CANNOT ANSWER FOR YOU: what does this program actually compute?
// ============================================================================================
// SP1StarkVerifierAdapter.sol and OmegaVault.sol together fix ONE thing: whatever this
// program computes, it must end by committing (blueprintHash, publicInputsHash) as its first
// two public outputs, in that order, ABI-encoded (see lib/src/lib.rs). That's the entire
// contract between this program and the Solidity side -- it says nothing about what claim
// the proof is actually making, because nothing in OmegaVault.sol, the Gate library, or any
// other file you've shared specifies that. Candidates discussed but not decided:
//   - Independent re-simulation of the strategy execution, proving netProfit matches a
//     recomputation against canonical price data (a check the on-chain path can't itself do).
//   - Some form of the "L4 Security layer" / OFA-compliance attestation MevOfa.sol's
//     docstring mentions but never specifies the mechanism of.
//   - Something else -- batch reconciliation, solvency, fee-split correctness.
// Until this is answered, everything between the `sp1_zkvm::io::read` calls and the
// `commit_slice` call below is a placeholder, not a real proof of anything. Treat the
// `todo!()` as load-bearing, not decorative -- this program will panic (fail to prove) if run
// as-is, deliberately, rather than silently produce a valid-looking proof of nothing.
// ============================================================================================

#![no_main]
sp1_zkvm::entrypoint!(main);

use alloy_sol_types::SolValue;
use omega_proof_lib::PublicValuesStruct;

pub fn main() {
    // -- Inputs -----------------------------------------------------------------------------
    // blueprintHash and publicInputsHash are read as public-facing commitments regardless of
    // what else this program ends up computing -- they're the two values
    // SP1StarkVerifierAdapter checks against what OmegaVault passed in. Read here as plain
    // inputs (their correctness as commitments doesn't depend on being secret).
    let blueprint_hash: [u8; 32] = sp1_zkvm::io::read();
    let public_inputs_hash: [u8; 32] = sp1_zkvm::io::read();

    // TODO: whatever private/public inputs the REAL computation needs go here, e.g.:
    //   let strategy_execution_trace: SomeType = sp1_zkvm::io::read();
    //   let canonical_price_data: SomeType = sp1_zkvm::io::read();
    // Shape entirely depends on the unresolved question above.

    // ========================================================================================
    // insecure_dev_noop: NOT a real proof of anything. Only compiled in when the
    // `insecure_dev_noop` Cargo feature is explicitly enabled (see this crate's Cargo.toml).
    // Skips the real computation entirely and commits the two input hashes unchanged. This
    // exists ONLY so script/ (and downstream: the adapter, OmegaVault wiring) can be exercised
    // end-to-end while the real computation is still undecided. A PROGRAM_VKEY built this way
    // authenticates "this program ran," nothing about "netProfit / whatever claim was
    // correct" -- because it never checked anything. Do not point a production
    // STARK_VERIFIER/SP1StarkVerifierAdapter deployment at a vkey built this way.
    // ========================================================================================
    #[cfg(feature = "insecure_dev_noop")]
    {
        let public_values = PublicValuesStruct {
            blueprintHash: blueprint_hash.into(),
            publicInputsHash: public_inputs_hash.into(),
        };
        let bytes = PublicValuesStruct::abi_encode(&public_values);
        sp1_zkvm::io::commit_slice(&bytes);
        return;
    }

    // -- Real path: still unresolved -----------------------------------------------------------
    // See the file-level doc comment (top of this file, unchanged from before) for the open
    // question this todo!() stands in for. This is the path any production build must take.
    #[cfg(not(feature = "insecure_dev_noop"))]
    todo!(
        "Define what this program actually proves before deploying against real funds. \
         See the file header for the open question and candidate answers. (Set the \
         insecure_dev_noop feature only for exercising the surrounding pipeline in \
         development -- never for a real deployment.)"
    );
}