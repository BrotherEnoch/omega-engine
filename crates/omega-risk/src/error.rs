// omega-engine\crates\omega-risk\src\error.rs
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RiskError {
    #[error("circuit breaker is tripped: {0}")]
    Tripped(String),

    #[error("cannot reset: breaker is not currently tripped")]
    NotTripped,

    #[error("invalid breaker configuration: {0}")]
    InvalidConfig(String),
}

pub type Result<T> = std::result::Result<T, RiskError>;