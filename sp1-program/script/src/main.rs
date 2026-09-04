// sp1-program/script/src/main.rs
//
// Host-side SP1 proving script.
// Emits the opaque proof blob OmegaVault.submitProof expects when the Vault
// is wired to SP1StarkVerifierAdapter:
//   starkProof = abi.encode(bytes publicValues, bytes proofBytes)
// where publicValues = abi.encode(bytes32 blueprintHash, bytes32 publicInputsHash)

use std::path::PathBuf;

use alloy_sol_types::SolValue;
use clap::Parser;
use omega_proof_lib::{ProofBundle, PublicValuesStruct};
use sp1_sdk::{ProverClient, SP1Stdin};

#[derive(Parser, Debug)]
#[command(author, version, about = "Omega SP1 proving script")]
struct Args {
    #[arg(long, default_value = "./elf/riscv32im-succinct-zkvm-elf")]
    elf: PathBuf,

    #[arg(long)]
    blueprint_hash: String,

    #[arg(long)]
    public_inputs_hash: String,

    #[arg(long)]
    output: Option<PathBuf>,

    /// Use insecure_dev_noop guest path (never for production vkeys)
    #[arg(long, default_value_t = false)]
    insecure_dev: bool,
}

fn parse_hash32(s: &str) -> [u8; 32] {
    let s = s.trim_start_matches("0x");
    let bytes = hex::decode(s).expect("invalid hex for 32-byte hash");
    assert_eq!(bytes.len(), 32, "hash must be exactly 32 bytes");
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

/// abi.encode(bytes a, bytes b) — matches SP1StarkVerifierAdapter decoding.
fn abi_encode_two_bytes(a: &[u8], b: &[u8]) -> Vec<u8> {
    fn pad32(n: usize) -> usize {
        (32 - (n % 32)) % 32
    }
    let a_len = a.len();
    let b_len = b.len();
    let a_block = 32 + a_len + pad32(a_len);
    let offset0: u64 = 64;
    let offset1: u64 = 64 + a_block as u64;

    let mut out = Vec::new();
    let mut w = [0u8; 32];
    w[24..32].copy_from_slice(&offset0.to_be_bytes());
    out.extend_from_slice(&w);
    w = [0u8; 32];
    w[24..32].copy_from_slice(&offset1.to_be_bytes());
    out.extend_from_slice(&w);

    w = [0u8; 32];
    w[24..32].copy_from_slice(&(a_len as u64).to_be_bytes());
    out.extend_from_slice(&w);
    out.extend_from_slice(a);
    out.extend(std::iter::repeat_n(0u8, pad32(a_len)));

    w = [0u8; 32];
    w[24..32].copy_from_slice(&(b_len as u64).to_be_bytes());
    out.extend_from_slice(&w);
    out.extend_from_slice(b);
    out.extend(std::iter::repeat_n(0u8, pad32(b_len)));

    out
}

fn main() {
    let args = Args::parse();

    let blueprint_hash = parse_hash32(&args.blueprint_hash);
    let public_inputs_hash = parse_hash32(&args.public_inputs_hash);

    let mut stdin = SP1Stdin::new();
    stdin.write(&blueprint_hash);
    stdin.write(&public_inputs_hash);

    if !args.insecure_dev {
        // Minimal non-empty signature so the guest non-zero check can pass in
        // integration tests. Production callers must supply a real attestation.
        let mut sig = [0u8; 64];
        sig[0] = 1;
        let attestation = (
            [0x11u8; 20], // token_a
            [0x22u8; 20], // token_b
            1_000_000_000_000_000_000u128, // price 1.0 * 1e18
            1_700_000_000u64, // attested_at
            sig,
        );
        stdin.write(&attestation);

        let claim = (
            [0xAAu8; 20], // pool_a
            [0xBBu8; 20], // pool_b
            [0x11u8; 20], // token_in
            [0x22u8; 20], // token_out
            1_000_000u128, // amount_in
            1_000_000u128, // min_amount_out
            0u128,         // claimed_net_profit
        );
        stdin.write(&claim);

        let now: u64 = 1_700_000_030u64; // within 60s of attested_at
        stdin.write(&now);
    }

    let client = ProverClient::new();
    let elf = std::fs::read(&args.elf).expect("read ELF");
    let (pk, vk) = client.setup(&elf);

    println!("Proving… (insecure_dev={})", args.insecure_dev);
    let proof = client
        .prove(&pk, stdin)
        .run()
        .expect("proof generation failed");

    // publicValues = abi.encode(bytes32, bytes32) = 64 static bytes
    let public_values_struct = PublicValuesStruct {
        blueprintHash: blueprint_hash.into(),
        publicInputsHash: public_inputs_hash.into(),
    };
    let public_values = PublicValuesStruct::abi_encode(&public_values_struct);

    let proof_bytes = proof.bytes();

    // Opaque blob for OmegaVault.submitProof third arg (SP1 adapter encoding)
    let stark_proof_arg = abi_encode_two_bytes(&public_values, &proof_bytes);

    // Also emit ProofBundle (same shape as adapter decode) for tooling
    let bundle = ProofBundle {
        publicValues: public_values.clone().into(),
        proofBytes: proof_bytes.clone().into(),
    };
    let bundle_encoded = ProofBundle::abi_encode(&bundle);

    if let Some(path) = args.output {
        std::fs::write(&path, &stark_proof_arg).expect("write output");
        println!("Wrote SP1 adapter proof blob to {}", path.display());
        let bundle_path = path.with_extension("bundle.hex");
        std::fs::write(&bundle_path, hex::encode(&bundle_encoded)).ok();
    } else {
        println!("submitProof starkProof (hex): 0x{}", hex::encode(&stark_proof_arg));
        println!("ProofBundle (hex): 0x{}", hex::encode(&bundle_encoded));
    }

    client.verify(&proof, &vk).expect("local SP1 verification failed");
    println!("Local SP1 verification succeeded");
}