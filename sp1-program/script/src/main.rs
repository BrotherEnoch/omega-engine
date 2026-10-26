//! Derive the SP1 verification-key hash for the built guest.
//!
//! This utility intentionally does not create proofs. The current guest is
//! fail-closed until oracle signature verification and trade-to-proof binding
//! are implemented and reviewed.

use std::path::PathBuf;

use clap::Parser;
use sp1_sdk::{HashableKey, ProverClient};

#[derive(Parser, Debug)]
#[command(author, version, about = "Derive the Omega SP1 verification-key hash")]
struct Args {
    #[arg(long, default_value = "../target/elf-compilation/riscv64im-succinct-zkvm-elf/release/omega-proof-program")]
    elf: PathBuf,

    /// Optional deployment value to compare with the derived hash.
    #[arg(long)]
    expected_vkey: Option<String>,
}

fn main() {
    let args = Args::parse();
    let elf = std::fs::read(&args.elf).expect("read SP1 ELF");
    let client = ProverClient::new();
    let (_, verifying_key) = client.setup(&elf);

    let derived = hex::encode(HashableKey::bytes32(&verifying_key));
    println!("PROGRAM_VKEY=0x{derived}");

    if let Some(expected) = args.expected_vkey.as_deref() {
        let expected = expected.trim().strip_prefix("0x").unwrap_or(expected.trim());
        if !expected.eq_ignore_ascii_case(&derived) {
            eprintln!("PROGRAM_VKEY does not match the supplied deployment value");
            std::process::exit(1);
        }
        println!("PROGRAM_VKEY matches the supplied value");
    }
}

