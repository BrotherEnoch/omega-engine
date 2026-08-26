// omega-engine\crates\omega-testnet\src\error.rs
//! Error type for omega-testnet.
//!
//! FIX (this revision): this file previously contained a copy-pasted
//! `SimError` (omega-simulation's error type, not this crate's own) — it
//! referenced `ethers::providers::ProviderError` and `anyhow::Error`,
//! neither of which is a dependency of `omega-testnet` (E0433), and
//! defined no `TestnetError` at all, so every other file in this crate
//! that imports `TestnetError` (config.rs, lib.rs's own re-export)
//! failed with E0432. Replaced with this crate's actual error type,
//! built only from variants `config.rs`/`gate.rs` are confirmed to use:
//! `InvalidConfig` (every `validate()` failure in config.rs) and
//! `MainnetChainIdRejected` (gate.rs's own test asserts
//! `matches!(err, TestnetError::MainnetChainIdRejected(42161))`, so the
//! variant must be a single-field tuple variant carrying the rejected
//! `u64` chain ID, not a struct variant or a String). `Io`/`Serde` are
//! included since a testnet dry-run report (report.rs) is the kind of
//! thing that gets written to disk — not confirmed against report.rs's
//! real source (not shown this session), so if `cargo build` reports
//! these as unused, that's fine to leave (an unused enum variant is not
//! a compile error) or delete once report.rs's real needs are visible.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TestnetError {
    #[error("invalid testnet config: {0}")]
    InvalidConfig(String),

    #[error(
        "chain_id {0} looks like a known mainnet chain — refusing to run against it. \
         If this is an intentional mainnet canary run (not a testnet dry-run), set \
         allow_mainnet_chain_id = true explicitly."
    )]
    MainnetChainIdRejected(u64),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, TestnetError>;