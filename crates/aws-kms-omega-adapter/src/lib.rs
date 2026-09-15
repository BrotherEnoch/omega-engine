// crates/aws-kms-omega-adapter/src/lib.rs
//! Non-invasive adapter: Omega `TransactionSigner` + AWS KMS outer envelope.
//!
//! # Gate purpose
//!
//! Prove, by compilation only, that:
//! 1. A type can implement `omega_execution::TransactionSigner`.
//! 2. The outer EIP-1559 signing digest can be produced by
//!    `aws_kms_signer::AwsKmsSigner::sign_hash` (no in-memory `SecretKey`).
//! 3. No Omega production crate is modified.
//!
//! # What this is not
//!
//! - Not wired into `main.rs` / production pipeline.
//! - Not a live KMS integration test.
//! - Not a full re-implementation of `KeyManagerTransactionSigner`'s
//!   blueprint calldata / domain-hash path (those helpers are not public
//!   on the Omega side). Full end-to-end `sign_transaction` remains
//!   fail-closed until that surface is shared or duplicated in a later gate.
//!
//! # Outer-envelope path (implemented)
//!
//! ```text
//! unsigned EIP-1559 RLP
//!     → keccak256
//!     → AwsKmsSigner::sign_hash   ← replaces KeyManager::active_secret_key + local secp
//!     → r || s || y_parity
//!     → signed EIP-1559 RLP
//! ```

mod envelope;
mod error;
mod signer;

pub use envelope::sign_eip1559_envelope;
pub use error::AdapterError;
pub use signer::AwsKmsTransactionSigner;

/// Compile-time assertion: `AwsKmsTransactionSigner` implements Omega's trait.
#[cfg(test)]
mod trait_impl_compile_gate {
    use super::AwsKmsTransactionSigner;
    use omega_execution::TransactionSigner;

    fn assert_transaction_signer<T: TransactionSigner>() {}

    #[test]
    fn aws_kms_transaction_signer_implements_omega_trait() {
        assert_transaction_signer::<AwsKmsTransactionSigner>();
    }
}