// crates/aws-kms-signer/src/error.rs — OmegaEngine v12.0 Error Types
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("AWS KMS error: {0}")]
    Kms(String),

    #[error("invalid public key material from KMS")]
    InvalidPublicKey,

    #[error("KMS key validation failed: {0}")]
    KeyValidation(String),

    #[error("signature conversion failed: {0}")]
    SignatureConversion(String),

    #[error("address recovery failed")]
    AddressRecovery,

    #[error("configured address mismatch: expected {expected}, derived {got}")]
    AddressMismatch {
        expected: String,
        got: String,
    },

    #[error("internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<aws_sdk_kms::Error> for Error {
    fn from(e: aws_sdk_kms::Error) -> Self {
        Error::Kms(e.to_string())
    }
}

impl From<aws_sdk_kms::error::SdkError<aws_sdk_kms::operation::get_public_key::GetPublicKeyError>>
    for Error
{
    fn from(
        e: aws_sdk_kms::error::SdkError<
            aws_sdk_kms::operation::get_public_key::GetPublicKeyError,
        >,
    ) -> Self {
        Error::Kms(e.to_string())
    }
}

impl From<aws_sdk_kms::error::SdkError<aws_sdk_kms::operation::sign::SignError>> for Error {
    fn from(e: aws_sdk_kms::error::SdkError<aws_sdk_kms::operation::sign::SignError>) -> Self {
        Error::Kms(e.to_string())
    }
}

impl From<aws_sdk_kms::error::SdkError<aws_sdk_kms::operation::describe_key::DescribeKeyError>>
    for Error
{
    fn from(
        e: aws_sdk_kms::error::SdkError<
            aws_sdk_kms::operation::describe_key::DescribeKeyError,
        >,
    ) -> Self {
        Error::Kms(e.to_string())
    }
}