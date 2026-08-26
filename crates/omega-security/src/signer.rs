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
//   The signer itself is stateless — all key state lives in KeyManager.
//
// ── CORRECTION (this revision) ──────────────────────────────────────────────
//
// This module's doc comment previously stated:
//   "Signing uses the Ethereum personalSign prefix (EIP-191
//    \x19Ethereum Signed Message:\n32) because that is what ecrecover in the
//    Orchestrator expects."
//
// That is FALSE against the deployed OmegaOrchestrator.sol read while fixing
// this. The contract's `execute()` does:
//
//     bytes32 bpHash = keccak256(abi.encode(address(this), EXPECTED_CHAIN_ID, blueprintCalldata));
//     address signer = bpHash.recover(sig);
//
// `bpHash.recover(sig)` is OpenZeppelin's `ECDSA.recover(bytes32, bytes)`,
// which calls `ecrecover` directly on the hash it is given — it applies NO
// prefix. The EIP-191 personal-sign prefix is a *different* OZ function
// (`toEthSignedMessageHash().recover(sig)`), which `execute()` never calls.
//
// Consequence: `sign()` below, which DOES apply the EIP-191 prefix, produces
// a signature that recovers to a different address than the one the contract
// checks against `execution_key`/`pending_key`. A signature from `sign()`
// alone will NOT pass `_acceptsKey()` and every `execute()` call built from
// it will revert with `InvalidSignature()`.
//
// `sign()` is left as-is below and remains correct for its OTHER stated use
// — the Flashbots `X-Flashbots-Signature` reputation header, which is an
// off-chain relay convention with its own (EIP-191-based) expectation and no
// on-chain `recover()` to match. What's added is `sign_raw_hash()`, a
// sibling method that signs a hash with no prefix at all, for exactly the
// on-chain-authorization case. Callers building a blueprint authorization
// signature for `OmegaOrchestrator.execute()` MUST use `sign_raw_hash()`,
// not `sign()`.
//
// Spec notes:
//   - OmegaOrchestrator._accepts_key() supports the dual-key window during rotation:
//     both execution_key and pending_key are valid within rotation_window_end_block.
//   - On Arbitrum: Arbitrum sequencer receives bundles directly, no MEV-Boost signing
//     required.  Flashbots-style header is only needed for L1 relay submissions.

use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
use sha3::{Digest, Keccak256};
use std::sync::Arc;

use crate::error::SecurityError;
use crate::key_manager::KeyManager;
use crate::metrics;

/// EIP-191 personal sign prefix (Ethereum standard). Used only by `sign()`
/// (the Flashbots reputation-header path) — NOT by `sign_raw_hash()`. See
/// this module's "CORRECTION" doc comment above for why the two must not be
/// conflated.
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
    /// The hash that was actually signed. For `sign()` this is the raw
    /// blueprint hash passed in (the EIP-191 prefixing happens internally,
    /// on top of this value). For `sign_raw_hash()` this is exactly the
    /// hash that was signed with no transformation.
    pub blueprint_hash: [u8; 32],
    /// Compact secp256k1 signature.
    pub signature: Signature,
    /// Ethereum address (20 bytes) of the signing key used.
    pub signer_address: [u8; 20],
    /// Pre-formatted Flashbots header value: "<addr>:<sig_hex>". Populated
    /// by both `sign()` and `sign_raw_hash()` for structural uniformity,
    /// but only `sign()`'s output is actually intended for use as a real
    /// Flashbots header — see each method's doc comment.
    pub flashbots_header: String,
}

impl SignedBundle {
    /// True if `signer_address` matches `expected`.
    pub fn signed_by(&self, expected: &[u8; 20]) -> bool {
        &self.signer_address == expected
    }
}

/// Stateless blueprint signer — shares key material via Arc<KeyManager>.
#[derive(Clone)]
pub struct BlueprintSigner {
    key_manager: Arc<KeyManager>,
    secp: Arc<Secp256k1<secp256k1::All>>,
}

impl BlueprintSigner {
    pub fn new(key_manager: Arc<KeyManager>) -> Self {
        Self {
            key_manager,
            secp: Arc::new(Secp256k1::new()),
        }
    }

    /// Ethereum address of the KeyManager's current active key.
    /// Used by `KeyManagerTransactionSigner` to fail closed if a produced
    /// authorization signature does not recover to this address.
    pub fn active_address(&self) -> [u8; 20] {
        self.key_manager.active_address()
    }

    /// Sign `blueprint_hash` with the current active execution key, using
    /// the EIP-191 personal-sign prefix.
    ///
    /// Steps:
    ///   1. Apply EIP-191 personal sign prefix.
    ///   2. keccak256(prefix || blueprint_hash) → prefixed_hash.
    ///   3. secp256k1.sign_ecdsa_recoverable(prefixed_hash, secret_key).
    ///   4. Serialize to compact 65-byte form [r||s||v].
    ///   5. Build Flashbots header string.
    ///
    /// USE FOR: the Flashbots `X-Flashbots-Signature` reputation header
    /// only. Do NOT use this for `OmegaOrchestrator.execute()`'s `sig`
    /// argument — that contract recovers against the raw, unprefixed hash.
    /// See `sign_raw_hash()` and this module's "CORRECTION" doc comment.
    pub fn sign(&self, blueprint_hash: &[u8; 32]) -> Result<SignedBundle, SecurityError> {
        let secret_key = self
            .key_manager
            .active_secret_key()
            .ok_or(SecurityError::NoActiveKey)?;

        let prefixed_hash = eip191_hash(blueprint_hash);
        let msg = Message::from_digest_slice(&prefixed_hash).map_err(|e| {
            SecurityError::SigningFailed {
                detail: e.to_string(),
            }
        })?;

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
        let sig_hex = format!("0x{}", hex::encode(sig_bytes));
        let flashbots_header = format!("0x{}:{}", addr_hex, sig_hex);

        metrics::BLUEPRINTS_SIGNED
            .with_label_values(&[&hex::encode(&signer_address[..4])])
            .inc();

        tracing::debug!(
            blueprint_hash = hex::encode(blueprint_hash),
            signer = %addr_hex,
            "blueprint signed (EIP-191 prefixed — Flashbots header use only)"
        );

        Ok(SignedBundle {
            blueprint_hash: *blueprint_hash,
            signature: Signature { bytes: sig_bytes },
            signer_address,
            flashbots_header,
        })
    }

    /// Sign `hash` directly, with NO prefix of any kind.
    ///
    /// Matches `OmegaOrchestrator.execute()`'s `bpHash.recover(sig)` exactly
    /// — OpenZeppelin's `ECDSA.recover(bytes32, bytes)` calls `ecrecover` on
    /// the hash it is given, unmodified. This is the method to use for
    /// producing the `sig` argument to `execute(blueprintCalldata, sig)`.
    ///
    /// `hash` here must already be the domain-separated on-chain hash —
    /// `keccak256(abi.encode(orchestratorAddress, chainId, blueprintCalldata))`
    /// — not `ExecutionBlueprint::blueprint_hash` (a distinct, unrelated
    /// off-chain content hash over a different field layout; see
    /// `ExecutionBlueprint::compute_hash()`'s own doc comment). Computing
    /// that on-chain hash from an `ExecutionBlueprint` is not this method's
    /// job — it's the caller's, once a `blueprintCalldata` ABI encoder
    /// exists (not yet implemented anywhere in this workspace as of this
    /// revision).
    ///
    /// Steps:
    ///   1. NO prefix applied — `hash` is signed as-is.
    ///   2. secp256k1.sign_ecdsa_recoverable(hash, secret_key).
    ///   3. Serialize to compact 65-byte form [r||s||v], v = recovery_id + 27
    ///      (standard Ethereum convention; `ECDSA.recover` accepts both the
    ///      27/28 and 0/1 conventions for the trailing byte).
    pub fn sign_raw_hash(&self, hash: &[u8; 32]) -> Result<SignedBundle, SecurityError> {
        let secret_key = self
            .key_manager
            .active_secret_key()
            .ok_or(SecurityError::NoActiveKey)?;

        let msg = Message::from_digest_slice(hash).map_err(|e| SecurityError::SigningFailed {
            detail: e.to_string(),
        })?;

        let (recovery_id, compact_sig) = self
            .secp
            .sign_ecdsa_recoverable(&msg, &secret_key)
            .serialize_compact();

        let v = recovery_id.to_i32() as u8 + 27;
        let mut sig_bytes = [0u8; 65];
        sig_bytes[..64].copy_from_slice(&compact_sig);
        sig_bytes[64] = v;

        let signer_address = secret_key_to_address(&self.secp, &secret_key);
        let addr_hex = hex::encode(signer_address);
        let sig_hex = format!("0x{}", hex::encode(sig_bytes));
        let flashbots_header = format!("0x{}:{}", addr_hex, sig_hex);

        metrics::BLUEPRINTS_SIGNED
            .with_label_values(&[&hex::encode(&signer_address[..4])])
            .inc();

        tracing::debug!(
            hash = hex::encode(hash),
            signer = %addr_hex,
            "raw hash signed (no prefix — on-chain execute() authorization path)"
        );

        Ok(SignedBundle {
            blueprint_hash: *hash,
            signature: Signature { bytes: sig_bytes },
            signer_address,
            flashbots_header,
        })
    }

    /// Verify that `bundle` (produced by `sign()`, i.e. EIP-191 prefixed)
    /// was signed by `expected_signer`.
    ///
    /// Recovers the public key from the signature and checks the derived
    /// address. Does NOT apply to bundles produced by `sign_raw_hash()` —
    /// use `verify_raw_hash()` for those.
    pub fn verify(
        &self,
        bundle: &SignedBundle,
        expected_signer: &[u8; 20],
    ) -> Result<(), SecurityError> {
        let prefixed_hash = eip191_hash(&bundle.blueprint_hash);
        self.verify_against_digest(&prefixed_hash, bundle, expected_signer)
    }

    /// Verify that `bundle` (produced by `sign_raw_hash()`, i.e. no prefix)
    /// was signed by `expected_signer`. Mirrors `verify()` but skips the
    /// EIP-191 prefixing step, matching `sign_raw_hash()`'s signing path.
    pub fn verify_raw_hash(
        &self,
        bundle: &SignedBundle,
        expected_signer: &[u8; 20],
    ) -> Result<(), SecurityError> {
        self.verify_against_digest(&bundle.blueprint_hash, bundle, expected_signer)
    }

    fn verify_against_digest(
        &self,
        digest: &[u8; 32],
        bundle: &SignedBundle,
        expected_signer: &[u8; 20],
    ) -> Result<(), SecurityError> {
        let msg = Message::from_digest_slice(digest).map_err(|e| SecurityError::SigningFailed {
            detail: e.to_string(),
        })?;

        let v_adjusted = bundle.signature.bytes[64].saturating_sub(27);
        let rec_id = secp256k1::ecdsa::RecoveryId::from_i32(v_adjusted as i32).map_err(|_e| {
            SecurityError::SignatureInvalid {
                blueprint_hash: hex::encode(bundle.blueprint_hash),
            }
        })?;

        let rec_sig = secp256k1::ecdsa::RecoverableSignature::from_compact(
            &bundle.signature.bytes[..64],
            rec_id,
        )
        .map_err(|_| SecurityError::SignatureInvalid {
            blueprint_hash: hex::encode(bundle.blueprint_hash),
        })?;

        let recovered_pubkey = self.secp.recover_ecdsa(&msg, &rec_sig).map_err(|_| {
            SecurityError::SignatureInvalid {
                blueprint_hash: hex::encode(bundle.blueprint_hash),
            }
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

// ─── Hash helpers ─────────────────────────────────────────────────────────────

/// Compute keccak256(EIP-191 prefix || 32-byte hash). Used only by `sign()`
/// / `verify()` — the Flashbots-header path. NOT used by `sign_raw_hash()`.
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

/// Compute an arbitrary hash as keccak256 of the given bytes.
///
/// NOTE: despite the name, this is a generic keccak256 helper, NOT
/// specifically the on-chain `bpHash`. Computing the actual on-chain
/// `bpHash = keccak256(abi.encode(address(this), EXPECTED_CHAIN_ID,
/// blueprintCalldata))` requires ABI-encoding `blueprintCalldata` first —
/// that encoder does not exist in this workspace as of this revision. Do
/// not call this function on raw, non-ABI-encoded blueprint bytes and
/// assume the result matches what the contract will recover against.
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
        let sk = SecretKey::from_slice(&[0x42u8; 32]).unwrap();
        let addr = secret_key_to_address(&secp, &sk);
        let km = KeyManager::from_secret_key(sk);
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

    // ── sign_raw_hash / verify_raw_hash — the on-chain execute() path ─────

    #[test]
    fn sign_raw_hash_produces_65_byte_signature() {
        let (signer, _) = make_signer();
        let hash: [u8; 32] = [0xab; 32];
        let bundle = signer.sign_raw_hash(&hash).unwrap();
        assert_eq!(bundle.signature.bytes.len(), 65);
    }

    #[test]
    fn sign_raw_hash_and_verify_raw_hash_roundtrip() {
        let (signer, addr) = make_signer();
        let hash: [u8; 32] = [0x77; 32];
        let bundle = signer.sign_raw_hash(&hash).unwrap();
        assert!(signer.verify_raw_hash(&bundle, &addr).is_ok());
    }

    #[test]
    fn sign_raw_hash_differs_from_sign_for_the_same_hash() {
        // Direct regression guard for the bug this revision fixes: the two
        // methods must NOT produce recoverable-equivalent signatures for
        // the same input hash, because they sign different digests
        // (prefixed vs. raw). If this ever started passing, it would mean
        // the raw-hash path had been accidentally reprefixed.
        let (signer, addr) = make_signer();
        let hash: [u8; 32] = [0x33; 32];

        let prefixed_bundle = signer.sign(&hash).unwrap();
        let raw_bundle = signer.sign_raw_hash(&hash).unwrap();

        // A signature produced by sign() must NOT verify under
        // verify_raw_hash() against the same signer address, and vice
        // versa — proving the two digests are genuinely different.
        assert!(signer.verify_raw_hash(&prefixed_bundle, &addr).is_err());
        assert!(signer.verify(&raw_bundle, &addr).is_err());

        // But each verifies correctly under its OWN matching method.
        assert!(signer.verify(&prefixed_bundle, &addr).is_ok());
        assert!(signer.verify_raw_hash(&raw_bundle, &addr).is_ok());
    }

    #[test]
    fn verify_raw_hash_wrong_signer_fails() {
        let (signer, _) = make_signer();
        let hash: [u8; 32] = [0x44; 32];
        let bundle = signer.sign_raw_hash(&hash).unwrap();
        let wrong_addr = [0xee; 20];
        assert!(signer.verify_raw_hash(&bundle, &wrong_addr).is_err());
    }

    #[test]
    fn sign_raw_hash_same_hash_same_key_deterministic() {
        let (signer, _) = make_signer();
        let hash = [0x66u8; 32];
        let b1 = signer.sign_raw_hash(&hash).unwrap();
        let b2 = signer.sign_raw_hash(&hash).unwrap();
        assert_eq!(b1.signature.bytes, b2.signature.bytes);
    }
}
