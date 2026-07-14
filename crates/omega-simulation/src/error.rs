// omega-engine\crates\omega-simulation\src\error.rs
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SimError {
    #[error("failed to spawn local fork node: {0}")]
    ForkSpawnFailed(String),

    #[error("fork RPC endpoint not reachable at {0}")]
    ForkUnreachable(String),

    #[error("contract deployment failed for {contract}: {reason}")]
    DeploymentFailed { contract: String, reason: String },

    #[error("simulated transaction reverted: {0}")]
    Reverted(String),

    #[error("provider error: {0}")]
    Provider(#[from] ethers::providers::ProviderError),

    #[error("contract call error: {0}")]
    Contract(String),

    #[error(
        "refused: this crate only submits to a local fork handle. \
         Attempted destination `{0}` looks like a live relay/signing target. \
         Live execution belongs in omega-execution, not here."
    )]
    LiveTransportForbidden(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("other: {0}")]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, SimError>;