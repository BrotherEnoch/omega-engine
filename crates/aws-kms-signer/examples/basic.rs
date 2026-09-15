// crates/aws-kms-signer/examples/basic.rs
//! WARNING: This example is intentionally a no-op.
//!
//! The real construction path requires an externally supplied
//! `aws_sdk_kms::Client` (so that this crate never depends on aws-config).
//!
//! Performing a live `DescribeKey` / `GetPublicKey` / `Sign` call would
//! create real AWS traffic and is therefore disabled until explicitly
//! enabled by the caller.
//!
//! To exercise the signer against a real key you must:
//! 1. Build an `aws_sdk_kms::Client` yourself (using whatever credential
//!    mechanism Omega already uses).
//! 2. Call `AwsKmsSigner::new(client, key_id, expected_address)`.
//! 3. Call `sign_hash` or `sign_message`.
//!
//! Do not run live signing from this example until the crate has been
//! reviewed and integrated.

use aws_kms_signer::AwsKmsSigner;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("aws-kms-signer example is disabled by design.");
    eprintln!("Construct an aws_sdk_kms::Client externally and call AwsKmsSigner::new.");
    eprintln!("See the crate-level documentation in src/lib.rs.");

    // Keep the type in the binary so the example still links against the crate.
    let _ = std::any::type_name::<AwsKmsSigner>();

    Ok(())
}