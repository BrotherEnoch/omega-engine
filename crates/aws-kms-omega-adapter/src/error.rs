// crates/aws-kms-omega-adapter/src/error.rs
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("AWS KMS signer error: {0}")]
    Kms(String),

    #[error("RLP / envelope encoding error: {0}")]
    Envelope(String),

    #[error(
        "execute-calldata path not public on omega-execution; \
         AwsKmsTransactionSigner::sign_transaction is intentionally fail-closed \
         until KeyManagerTransactionSigner calldata helpers are shared or \
         re-implemented in a follow-up gate"
    )]
    CalldataPathNotPublic,

    #[error("invalid chain_id: must be non-zero")]
    ZeroChainId,

    #[error("gas_limit must be non-zero")]
    ZeroGasLimit,

    #[error("empty calldata refused")]
    EmptyCalldata,
}

impl From<aws_kms_signer::Error> for AdapterError {
    fn from(e: aws_kms_signer::Error) -> Self {
        AdapterError::Kms(e.to_string())
    }
}