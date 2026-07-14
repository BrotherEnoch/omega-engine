omega-engine\crates\omega-testnet\src\error.rs
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TestnetError {
    #[error("invalid testnet configuration: {0}")]
    InvalidConfig(String),

    #[error(
        "refused: chain_id {0} looks like a mainnet chain and \
         `allow_mainnet_chain_id` was not explicitly set. This crate is for \
         testnet dry-runs; if you intend to target this chain deliberately, \
         set allow_mainnet_chain_id = true in TestnetConfig."
    )]
    MainnetChainIdRejected(u64),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, TestnetError>;