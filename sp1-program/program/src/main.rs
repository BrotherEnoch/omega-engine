// sp1-program/program/src/main.rs
//
// Fail-closed SP1 guest. It deliberately panics until proof semantics can
// authenticate oracle data and bind the verified trade to the on-chain inputs.

#![no_main]
sp1_zkvm::entrypoint!(main);

// Intentionally disabled: the existing circuit does not authenticate oracle
// signatures in-circuit or bind its arithmetic claim to the actual orchestrator
// calldata. Do not enable proof production until both properties are implemented
// and reviewed. Keeping this guard in the guest makes the current verification key
// incapable of producing an accepted proof.
fn production_proof_path_is_reviewed() -> bool {
    false
}

pub fn main() {
    assert!(
        production_proof_path_is_reviewed(),
        "SP1 proof production is disabled: oracle authentication and trade-to-proof binding are not implemented"
    );
}

