// crates/aws-kms-signer/src/signer.rs
use crate::error::{Error, Result};
use crate::kms::{keccak256, KmsService};
use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey};
use std::sync::Arc;

/// 20-byte Ethereum address.
pub type Address = [u8; 20];

/// 65-byte Ethereum signature (r || s || v) where v is the recovery ID (0 or 1).
pub type Signature = [u8; 65];

/// AWS KMS-backed secp256k1 signer.
///
/// This type deliberately does **not** implement any local `TransactionSigner`
/// trait. It is a pure adapter that talks to AWS KMS and produces the
/// cryptographic primitives (address + signature) that an Omega-level
/// adapter can later wrap to satisfy Omega's real `TransactionSigner`
/// interface.
#[derive(Clone)]
pub struct AwsKmsSigner {
    service: Arc<KmsService>,
    address: Address,
    verifying_key: VerifyingKey,
}

impl AwsKmsSigner {
    /// Construct the signer.
    ///
    /// - `client` must already be configured with credentials / region.
    /// - `key_id` is the KMS key ID or ARN.
    /// - `expected_address` (optional) is the Ethereum address that must
    ///   match the address derived from the KMS public key. If supplied
    ///   and the addresses differ, construction fails closed.
    ///
    /// Performs:
    /// 1. DescribeKey validation (KeySpec, KeyUsage, KeyState)
    /// 2. GetPublicKey → VerifyingKey → Ethereum address
    /// 3. Optional address equality check
    pub async fn new(
        client: aws_sdk_kms::Client,
        key_id: impl Into<String>,
        expected_address: Option<Address>,
    ) -> Result<Self> {
        let service = Arc::new(KmsService::new(client, key_id));

        // 1. Explicit key validation
        service.validate_key().await?;

        // 2. Derive public key and address
        let verifying_key = service.get_verifying_key().await?;
        let address = public_key_to_address(&verifying_key);

        // 3. Fail-closed address check when configured
        if let Some(expected) = expected_address {
            if expected != address {
                return Err(Error::AddressMismatch {
                    expected: hex::encode(expected),
                    got: hex::encode(address),
                });
            }
        }

        Ok(Self {
            service,
            address,
            verifying_key,
        })
    }

    /// Ethereum address controlled by this signer.
    pub fn address(&self) -> Address {
        self.address
    }

    /// Sign a 32-byte digest (already keccak256-hashed).
    ///
    /// Returns the 65-byte Ethereum signature (r || s || v) with
    /// low-S normalized and a correct recovery ID.
    pub async fn sign_hash(&self, hash: &[u8; 32]) -> Result<Signature> {
        let der = self.service.sign_digest(hash).await?;

        // Parse ASN.1 DER → (r, s)
        let sig = K256Signature::from_der(&der)
            .map_err(|e| Error::SignatureConversion(e.to_string()))?;

        // Enforce low-S (EIP-2). normalize_s returns None when already low-S.
        let sig = sig.normalize_s().unwrap_or(sig);

        // Recover the correct recovery ID against the *normalized* signature.
        let recid = recover_id(&sig, hash, &self.verifying_key)?;

        // Assemble 65-byte form: r (32) || s (32) || v (1)
        let mut bytes = [0u8; 65];
        bytes[..32].copy_from_slice(&sig.r().to_bytes());
        bytes[32..64].copy_from_slice(&sig.s().to_bytes());
        bytes[64] = recid.to_byte();

        Ok(bytes)
    }

    /// Convenience: sign an arbitrary message under EIP-191 personal_sign.
    pub async fn sign_message(&self, message: &[u8]) -> Result<Signature> {
        let mut buf = Vec::with_capacity(message.len() + 64);
        buf.extend_from_slice(b"\x19Ethereum Signed Message:\n");
        buf.extend_from_slice(message.len().to_string().as_bytes());
        buf.extend_from_slice(message);
        let hash = keccak256(&buf);
        self.sign_hash(&hash).await
    }
}

// ---------------------------------------------------------------------------
// helpers (crate-visible for offline tests)
// ---------------------------------------------------------------------------

pub(crate) fn public_key_to_address(vk: &VerifyingKey) -> Address {
    let point = vk.to_encoded_point(false); // uncompressed
    let bytes = point.as_bytes();
    // drop the 0x04 prefix → 64-byte X||Y
    let hash = keccak256(&bytes[1..]);
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&hash[12..]);
    addr
}

pub(crate) fn recover_id(
    sig: &K256Signature,
    message: &[u8],
    expected: &VerifyingKey,
) -> Result<RecoveryId> {
    // Ethereum only uses the two even-y recovery IDs (0 and 1).
    for recid in [RecoveryId::new(false, false), RecoveryId::new(true, false)] {
        if let Ok(recovered) = VerifyingKey::recover_from_prehash(message, sig, recid) {
            if &recovered == expected {
                return Ok(recid);
            }
        }
    }
    Err(Error::AddressRecovery)
}