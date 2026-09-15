// crates/aws-kms-signer/src/kms.rs
use crate::error::{Error, Result};
use aws_sdk_kms::{
    primitives::Blob,
    types::{KeySpec, KeyState, KeyUsageType, MessageType, SigningAlgorithmSpec},
    Client,
};
use k256::ecdsa::VerifyingKey;
use k256::pkcs8::DecodePublicKey;
use sha3::{Digest, Keccak256};
use std::sync::Arc;

/// Thin wrapper around an already-constructed AWS KMS client.
/// Credential resolution and client construction are the caller's responsibility.
/// This keeps aws-config out of the dependency tree.
#[derive(Clone)]
pub struct KmsService {
    client: Arc<Client>,
    key_id: String,
}

impl KmsService {
    pub fn new(client: Client, key_id: impl Into<String>) -> Self {
        Self {
            client: Arc::new(client),
            key_id: key_id.into(),
        }
    }

    /// Validate that the configured key is suitable for Ethereum secp256k1 signing.
    ///
    /// Checks:
    /// - KeySpec == ECC_SECG_P256K1
    /// - KeyUsage == SIGN_VERIFY
    /// - KeyState == Enabled
    pub async fn validate_key(&self) -> Result<()> {
        let resp = self
            .client
            .describe_key()
            .key_id(&self.key_id)
            .send()
            .await?;

        let meta = resp
            .key_metadata()
            .ok_or_else(|| Error::KeyValidation("missing key metadata".into()))?;

        let spec = meta.key_spec();
        if spec != Some(&KeySpec::EccSecgP256K1) {
            return Err(Error::KeyValidation(format!(
                "expected KeySpec ECC_SECG_P256K1, got {:?}",
                spec
            )));
        }

        let usage = meta.key_usage();
        if usage != Some(&KeyUsageType::SignVerify) {
            return Err(Error::KeyValidation(format!(
                "expected KeyUsage SIGN_VERIFY, got {:?}",
                usage
            )));
        }

        let state = meta.key_state();
        if state != Some(&KeyState::Enabled) {
            return Err(Error::KeyValidation(format!(
                "expected KeyState Enabled, got {:?}",
                state
            )));
        }

        Ok(())
    }

    pub async fn get_verifying_key(&self) -> Result<VerifyingKey> {
        let resp = self
            .client
            .get_public_key()
            .key_id(&self.key_id)
            .send()
            .await?;

        let der = resp
            .public_key()
            .ok_or(Error::InvalidPublicKey)?
            .as_ref();

        // KMS returns SubjectPublicKeyInfo (SPKI) DER
        VerifyingKey::from_public_key_der(der).map_err(|_| Error::InvalidPublicKey)
    }

    /// Sign a 32-byte digest. Returns the raw ASN.1 DER signature from KMS.
    pub async fn sign_digest(&self, digest: &[u8; 32]) -> Result<Vec<u8>> {
        let resp = self
            .client
            .sign()
            .key_id(&self.key_id)
            .message(Blob::new(digest.as_slice()))
            .message_type(MessageType::Digest)
            .signing_algorithm(SigningAlgorithmSpec::EcdsaSha256)
            .send()
            .await?;

        let sig = resp
            .signature()
            .ok_or_else(|| Error::Internal("KMS returned empty signature".into()))?
            .as_ref()
            .to_vec();

        Ok(sig)
    }
}

/// Compute keccak256.
pub fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    hasher.finalize().into()
}