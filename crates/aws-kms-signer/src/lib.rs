// crates/aws-kms-signer/src/lib.rs
//! Minimal AWS KMS-backed secp256k1 signer for Omega.
//!
//! # Design constraints (enforced)
//!
//! - No private `TransactionSigner` trait. This crate only produces
//!   the cryptographic primitives (`Address` + `Signature`). An
//!   Omega-level adapter will implement Omega's real interface on top.
//! - No `aws-config`. The caller supplies a fully-configured
//!   `aws_sdk_kms::Client`.
//! - `aws-sdk-kms` is pinned to the experimentally proven version.
//! - Explicit `DescribeKey` validation before any signing.
//! - Optional fail-closed expected-address check.
//! - Private key never leaves AWS KMS.

mod error;
mod kms;
mod signer;

#[cfg(test)]
mod crypto_tests;

pub use error::{Error, Result};
pub use signer::{Address, AwsKmsSigner, Signature};