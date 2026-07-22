// crates/omega-security/src/signer.rs
//
// Blueprint signing layer (spec S8 / S12 / OmegaOrchestrator.sol AUTH check).
//
// Responsibilities:
//   1. Sign the keccak256 of every ExecutionBlueprint before relay submission.
//      The signature proves authenticity to the on-chain Orchestrator (ecrecover).
//
//   2. Generate the Flashbots `X-Flashbots-Signature` header value:
//      "<signer_address>:<secp256k1_signature_hex>"
//      This is required by Flashbots and compatible relays to associate bundles
//      with a reputation address.
//
//   3. Verify incoming signatures (used by the orchestrator simulation path and
//      in the replay guard to ensure we only add our own bundles to the
//      executed set).
//
// Key material:
//   `BlueprintSigner` holds an Arc<KeyManager> and delegates key selection to it.
//   The signer itself is stateless â€” all key state lives in KeyManager.
//
// Spec notes:
//   - OmegaOrchestrator._accepts_key() supports the dual-key window during rotation:
//     both execution_key and pending_key are valid within rotation_window_end_block.
//   - Signing uses the Ethereum personalSign prefix (EIP-191 \x19Ethereum Signed Message:\n32)
//     because that is what ecrecover in the Orchestrator expects.
//   - On Arbitrum: Arbitrum sequencer receives bundles directly, no MEV-Boost signing
//     required.  Flashbots-style header is only needed for L1 relay submissions.

use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
use sha3::{Digest, Keccak256};
use std::sync::Arc;

use crate::error::SecurityError;
use crate::key_manager::KeyManager;
use crate::metrics;

/// EIP-191 personal sign prefix (Ethereum standard).
const ETH_SIGN_PREFIX: &[u8] = b"\x19Ethereum Signed Message:\n32";

/// A secp256k1 signature over a 32-byte message hash.
#[derive(Debug, Clone)]
pub struct Signature {
    /// 65-byte compact signature: [r(32) || s(32) || v(1)].
    pub bytes: [u8; 65],
}

impl Signature {
    /// Hex-encode with 0x prefix.
    pub fn to_hex(&self) -> String {
        format!("0x{}", hex::encode(self.bytes))
    }
}

/// A signed blueprint bundle ready for relay submission.
#[derive(Debug, Clone)]
pub struct SignedBundle {
    /// keccak256 hash of the encoded blueprint (the message that was signed).
    pub blueprint_hash: [u8; 32],
    /// Compact secp256k1 signature.
    pub signature: Signature,
    /// Ethereum address (20 bytes) of the signing key used.
    pub signer_address: [u8; 20],
    /// Pre-formatted Flashbots header value: "<addr>:<sig_hex>".
    pub flashbots_header: String,
}

impl SignedBundle {
    /// True if `signer_address` matches `expected`.
    pub fn signed_by(&self, expected: &[u8; 20]) -> bool {
        &self.signer_address == expected
    }
}

/// Stateless blueprint signer â€” shares key material via Arc<KeyManager>.
#[derive(Clone)]
pub struct BlueprintSigner {
    key_manager: Arc<KeyManager>,
    secp:        Arc<Secp256k1<secp256k1::All>>,
}

impl BlueprintSigner {
    pub fn new(key_manager: Arc<KeyManager>) -> Self {
        Self {
            key_manager,
            secp: Arc::new(Secp256k1::new()),
        }
    }

    /// Sign `blueprint_hash` with the current active execution key.
    ///
    /// Steps:
    ///   1. Apply EIP-191 personal sign prefix.
    ///   2. keccak256(prefix || blueprint_hash) â†’ prefixed_hash.
    ///   3. secp256k1.sign_ecdsa_recoverable(prefixed_hash, secret_key).
    ///   4. Serialize to compact 65-byte form [r||s||v].
    ///   5. Build Flashbots header string.
    pub fn sign(&self, blueprint_hash: &[u8; 32]) -> Result<SignedBundle, SecurityError> {
        let secret_key = self
            .key_manager
            .active_secret_key()
            .ok_or(SecurityError::NoActiveKey)?;

        let prefixed_hash = eip191_hash(blueprint_hash);
        let msg = Message::from_slice(&prefixed_hash)
            .map_err(|e| SecurityError::SigningFailed { detail: e.to_string() })?;

        let (recovery_id, compact_sig) = self
            .secp
            .sign_ecdsa_recoverable(&msg, &secret_key)
            .serialize_compact();

        let v = recovery_id.to_i32() as u8 + 27; // Ethereum v adjustment
        let mut sig_bytes = [0u8; 65];
        sig_bytes[..64].copy_from_slice(&compact_sig);
        sig_bytes[64] = v;

        let signer_address = secret_key_to_address(&self.secp, &secret_key);
        let addr_hex = hex::encode(signer_address);
        let sig_hex  = format!("0x{}", hex::encode(sig_bytes));
        let flashbots_header = format!("0x{}:{}", addr_hex, sig_hex);

        metrics::BLUEPRINTS_SIGNED
            .with_label_values(&[&hex::encode(&signer_address[..4])])
            .inc();

        tracing::debug!(
            blueprint_hash = hex::encode(blueprint_hash),
            signer = %addr_hex,
            "blueprint signed"
        );

        Ok(SignedBundle {
            blueprint_hash: *blueprint_hash,
            signature: Signature { bytes: sig_bytes },
            signer_address,
            flashbots_header,
        })
    }

    /// Verify that `bundle` was signed by `expected_signer`.
    ///
    /// Recovers the public key from the signature and checks the derived address.
    pub fn verify(
        &self,
        bundle:           &SignedBundle,
        expected_signer:  &[u8; 20],
    ) -> Result<(), SecurityError> {
        let prefixed_hash = eip191_hash(&bundle.blueprint_hash);
        let msg = Message::from_slice(&prefixed_hash)
            .map_err(|e| SecurityError::SigningFailed { detail: e.to_string() })?;

        let v_adjusted = bundle.signature.bytes[64].saturating_sub(27);
        let rec_id = secp256k1::ecdsa::RecoveryId::from_i32(v_adjusted as i32)
            .map_err(|e| SecurityError::SignatureInvalid {
                blueprint_hash: hex::encode(bundle.blueprint_hash),
            })?;

        let rec_sig = secp256k1::ecdsa::RecoverableSignature::from_compact(
            &bundle.signature.bytes[..64],
            rec_id,
        )
        .map_err(|_| SecurityError::SignatureInvalid {
            blueprint_hash: hex::encode(bundle.blueprint_hash),
        })?;

        let recovered_pubkey = self
            .secp
            .recover_ecdsa(&msg, &rec_sig)
            .map_err(|_| SecurityError::SignatureInvalid {
                blueprint_hash: hex::encode(bundle.blueprint_hash),
            })?;

        let recovered_address = pubkey_to_address(&recovered_pubkey);

        if &recovered_address != expected_signer {
            metrics::SIGNATURE_FAILURES.inc();
            return Err(SecurityError::SignatureInvalid {
                blueprint_hash: hex::encode(bundle.blueprint_hash),
            });
        }
        Ok(())
    }
}

// â”€â”€â”€ Hash helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Compute keccak256(EIP-191 prefix || 32-byte hash).
pub fn eip191_hash(hash: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(ETH_SIGN_PREFIX);
    hasher.update(hash);
    hasher.finalize().into()
}

/// Compute keccak256 of arbitrary bytes.
pub fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Derive an Ethereum address (last 20 bytes of keccak256(uncompressed_pubkey[1..])).
pub fn pubkey_to_address(pubkey: &PublicKey) -> [u8; 20] {
    let serialized = pubkey.serialize_uncompressed();
    let hash = keccak256(&serialized[1..]); // skip 0x04 prefix
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&hash[12..]);
    addr
}

/// Derive an Ethereum address from a secret key.
pub fn secret_key_to_address(secp: &Secp256k1<secp256k1::All>, sk: &SecretKey) -> [u8; 20] {
    let pk = PublicKey::from_secret_key(secp, sk);
    pubkey_to_address(&pk)
}

/// Compute blueprint hash as keccak256 of the ABI-encoded blueprint fields.
/// Mirrors the on-chain hash in OmegaOrchestrator.execute().
pub fn blueprint_hash(encoded_blueprint: &[u8]) -> [u8; 32] {
    keccak256(encoded_blueprint)
}

#[cfg(test)]
mod signer_tests {
    use super::*;
    use crate::key_manager::KeyManager;
    use secp256k1::SecretKey;

    fn make_signer() -> (BlueprintSigner, [u8; 20]) {
        let secp = Secp256k1::new();
        let sk   = SecretKey::from_slice(&[0x42u8; 32]).unwrap();
        let addr = secret_key_to_address(&secp, &sk);
        let km   = KeyManager::from_secret_key(sk);
        let signer = BlueprintSigner::new(Arc::new(km));
        (signer, addr)
    }

    #[test]
    fn sign_produces_65_byte_signature() {
        let (signer, _) = make_signer();
        let hash: [u8; 32] = [0xab; 32];
        let bundle = signer.sign(&hash).unwrap();
        assert_eq!(bundle.signature.bytes.len(), 65);
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let (signer, addr) = make_signer();
        let hash: [u8; 32] = [0x77; 32];
        let bundle = signer.sign(&hash).unwrap();
        assert!(signer.verify(&bundle, &addr).is_ok());
    }

    #[test]
    fn verify_wrong_signer_fails() {
        let (signer, _) = make_signer();
        let hash: [u8; 32] = [0x55; 32];
        let bundle = signer.sign(&hash).unwrap();
        let wrong_addr = [0xff; 20];
        assert!(signer.verify(&bundle, &wrong_addr).is_err());
    }

    #[test]
    fn flashbots_header_format() {
        let (signer, _) = make_signer();
        let bundle = signer.sign(&[0x01; 32]).unwrap();
        // Format: "0x<40 hex chars>:0x<130 hex chars>"
        let parts: Vec<&str> = bundle.flashbots_header.splitn(2, ':').collect();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].starts_with("0x") && parts[0].len() == 42); // "0x" + 40 hex
        assert!(parts[1].starts_with("0x") && parts[1].len() == 132); // "0x" + 130 hex
    }

    #[test]
    fn same_hash_same_key_produces_same_signature() {
        let (signer, _) = make_signer();
        let hash = [0x99u8; 32];
        let b1 = signer.sign(&hash).unwrap();
        let b2 = signer.sign(&hash).unwrap();
        assert_eq!(b1.signature.bytes, b2.signature.bytes);
    }

    #[test]
    fn different_hashes_produce_different_signatures() {
        let (signer, _) = make_signer();
        let b1 = signer.sign(&[0x01; 32]).unwrap();
        let b2 = signer.sign(&[0x02; 32]).unwrap();
        assert_ne!(b1.signature.bytes, b2.signature.bytes);
    }

    #[test]
    fn eip191_hash_is_deterministic() {
        let h1 = eip191_hash(&[0xde; 32]);
        let h2 = eip191_hash(&[0xde; 32]);
        assert_eq!(h1, h2);
    }

    #[test]
    fn keccak256_known_vector() {
        // keccak256("") = c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470
        let result = keccak256(b"");
        assert_eq!(
            hex::encode(result),
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
    }
}