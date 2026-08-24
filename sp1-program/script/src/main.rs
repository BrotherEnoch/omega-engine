// sp1-program/script/src/main.rs
//
// Host-side proving script: loads the compiled guest ELF, feeds it inputs, generates an
// on-chain-verifiable proof, and assembles the exact calldata blob SP1StarkVerifierAdapter.sol
// expects as OmegaVault.submitProof()'s `starkProof` argument.
//
// ============================================================================================
// SAME BLOCKING GAP AS program/src/main.rs: what does this program actually prove?
// ============================================================================================
// This script can only write the two inputs that are FIXED by the on-chain contract
// (blueprintHash, publicInputsHash) -- everything else (what additional private/public inputs
// the real computation needs, where this script sources them from -- on-chain state? an
// indexer? your own execution logs?) depends entirely on the still-open question raised
// several turns ago and not yet answered.
//
// By default this refuses to run past that point (see the OMEGA_INSECURE_DEV_NOOP gate below).
// Setting OMEGA_INSECURE_DEV_NOOP=1 unblocks it for PIPELINE TESTING ONLY, against a guest ELF
// also built with the matching insecure_dev_noop feature -- it does not answer the open
// question, it only lets the proving mechanics themselves be exercised while that question
// remains open. Do not use a proof or PROGRAM_VKEY produced this way against real funds.
// ============================================================================================
//
// TWO REAL DECISIONS MADE HERE, FLAGGED PER THIS CONVERSATION'S OWN PATTERN:
//
//   1. SYNC ProverClient::new() API, not the async ProverClient::builder()...build().await
//      pattern. Search evidence surfaced BOTH patterns across different SP1 docs/versions --
//      the sync `ProverClient::new()` / `client.setup(ELF)` / `client.prove(&pk,
//      stdin).groth16().run()` shape appeared consistently across many independent sources
//      including Succinct's own "Basics" docs page, while a newer async builder pattern
//      appeared in Succinct's "Prover Network Quickstart" page specifically (which may be
//      async because it's Prover-Network-specific, not necessarily because the whole API
//      moved). I went with the sync pattern as the safer default for local/CPU proving, which
//      is what you'd want for initial testing before deciding whether to route through the
//      Prover Network. If you're targeting the Prover Network specifically, re-verify against
//      current docs -- the async builder pattern may be required there.
//
//   2. GROTH16, not the default/compressed proof type. OmegaVault's C6 gate is only
//      satisfiable by a proof `SP1StarkVerifierAdapter` can verify via `ISP1Verifier`, and
//      Succinct's own docs are explicit that plain/compressed SP1 proofs are NOT verifiable
//      on-chain -- only Groth16- or PLONK-wrapped ones are. Chose Groth16 over PLONK as the
//      more commonly-referenced on-chain option across the sources I found; either would work
//      with the adapter as written, since both go through the same ISP1Verifier interface.
//      REQUIRES DOCKER RUNNING LOCALLY, plus >=16GB RAM, per Succinct's own docs -- this is a
//      real environment prerequisite, not a code detail; expect this to fail without it.
//
// WHAT THIS SCRIPT DOES NOT DO: submit the resulting proof on-chain. It generates the proof
// and prints/returns the exact calldata bytes `submitProof(blueprintHash, publicInputsHash,
// <printed bytes>)` needs -- sending that transaction (choosing an RPC endpoint, a signer, gas
// handling, retry logic) is a separate relayer concern I haven't been asked to build and
// won't assume the shape of.

use alloy_sol_types::SolValue;
use anyhow::Result;
use omega_proof_lib::ProofBundle;
use sp1_sdk::{include_elf, ProverClient, SP1Stdin};

/// The compiled guest ELF. `include_elf!` resolves this via SP1's build-time tooling (backed
/// by the `sp1-build`/`sp1-helper` crates) once `program/` has been built with `cargo prove
/// build` -- it will fail to compile until that ELF actually exists, which it doesn't yet
/// given program/src/main.rs is still a stub.
const ELF: &[u8] = include_elf!("omega-proof-program");

fn main() -> Result<()> {
    sp1_sdk::utils::setup_logger();

    // -- Fixed inputs -------------------------------------------------------------------------
    // These two are the only inputs this script can correctly construct right now -- they're
    // the values OmegaVault itself computes and passes to receivePendingProfit/submitProof, so
    // whatever real pipeline eventually calls this script must supply the REAL values for a
    // specific blueprint here, not the zeroed placeholders below.
    let blueprint_hash: [u8; 32] = [0u8; 32]; // TODO: real blueprintHash for the execution being proven
    let public_inputs_hash: [u8; 32] = [0u8; 32]; // TODO: real publicInputsHash, e.g. from
                                                   // OmegaVault.computePublicInputsHash(...)

    let mut stdin = SP1Stdin::new();
    stdin.write(&blueprint_hash);
    stdin.write(&public_inputs_hash);

    // TODO: write whatever additional private/public inputs the real computation needs, e.g.:
    //   stdin.write(&strategy_execution_trace);
    //   stdin.write(&canonical_price_data);
    // Shape depends entirely on the still-unresolved question in program/src/main.rs's header.

    // ============================================================================================
    // insecure_dev_noop gate -- mirrors program/'s own Cargo feature of the same name. This is a
    // separate crate, so it can't share a Cargo feature flag directly; gated on an explicit env
    // var instead, checked at runtime rather than compile time, but with the same intent: refuse
    // to proceed by default, only run the (meaningless) proving pipeline if explicitly told to.
    //
    // Setting this env var does NOT answer what this program should prove -- it only lets you
    // exercise the proving pipeline against a guest ELF that was ALSO built with
    // `cargo prove build --features insecure_dev_noop` (flagging: I have not independently
    // confirmed `cargo prove build` passes through --features identically to plain `cargo
    // build` -- verify this against your installed toolchain version before relying on it).
    // If the ELF wasn't built with that feature, it still contains the real todo!() and will
    // panic during proving regardless of this env var.
    // ============================================================================================
    let insecure_dev_noop = std::env::var("OMEGA_INSECURE_DEV_NOOP").as_deref() == Ok("1");
    if !insecure_dev_noop {
        todo!(
            "This script cannot correctly run until (a) program/src/main.rs's open question is \
             resolved and its real logic implemented, and (b) this script is updated to source \
             real blueprint_hash/public_inputs_hash and any other required inputs from an \
             actual data source, rather than the zeroed placeholders above. Set \
             OMEGA_INSECURE_DEV_NOOP=1 only to exercise the pipeline mechanics in development \
             against a matching insecure_dev_noop-built ELF -- never for a real deployment."
        );
    }
    eprintln!(
        "\u{26A0}\u{FE0F}  OMEGA_INSECURE_DEV_NOOP=1 -- generating a proof of NOTHING. \
         This proof and any PROGRAM_VKEY derived from it must never be used against real funds."
    );

    // -- Everything below is the CONFIRMED-CORRECT shape, runnable now only in dev-noop mode --
    let client = ProverClient::new();
    let (pk, vk) = client.setup(ELF);

    // Groth16, not the default proof type -- see file header, decision 2, for why this is
    // required rather than optional.
    let proof = client
        .prove(&pk, stdin)
        .groth16()
        .run()
        .expect("proof generation failed");

    // Always verify locally before trusting/shipping a proof -- standard practice across
    // every SP1 example found, not specific to this project.
    client.verify(&proof, &vk).expect("proof verification failed");

    // This is PROGRAM_VKEY -- the value DeployCore.s.sol / SP1StarkVerifierAdapter's
    // constructor needs. Print it so it can be captured once, not regenerated by guesswork
    // -- it's derived from the compiled ELF and is stable as long as the program doesn't
    // change. IMPORTANT: a vkey printed from an insecure_dev_noop build is only valid for
    // testing that same dev-noop ELF -- see the warnings above and in program/src/main.rs.
    println!("PROGRAM_VKEY = {}", vk.bytes32());

    // Assemble the exact calldata blob SP1StarkVerifierAdapter.verify() expects as its
    // `proof` parameter: abi.encode(bytes publicValues, bytes proofBytes). See
    // lib/src/lib.rs's ProofBundle doc comment for why a named struct's abi_encode() here
    // is ABI-identical to encoding a raw (bytes, bytes) tuple directly.
    let bundle = ProofBundle {
        publicValues: proof.public_values.to_vec().into(),
        proofBytes: proof.bytes().into(),
    };
    let encoded_proof_arg = ProofBundle::abi_encode(&bundle);

    println!("submitProof calldata argument (starkProof):");
    println!("0x{}", hex::encode(&encoded_proof_arg));

    Ok(())
}