// crates/aws-kms-signer/src/crypto_tests.rs
//! Offline cryptographic correctness tests.
//!
//! These tests never contact AWS KMS. They prove the pure Rust
//! conversion / recovery logic that sits between a KMS response and
//! an Ethereum signature.

#[cfg(test)]
mod tests {
    use crate::error::Error;
    use crate::kms::keccak256;
    use crate::signer::{public_key_to_address, recover_id, Address};
    use k256::ecdsa::{Signature as K256Signature, SigningKey, VerifyingKey};
    use k256::SecretKey;

    // ------------------------------------------------------------------
    // Helpers – generate deterministic test material
    // ------------------------------------------------------------------

    /// Fixed test private key (do not use in production).
    /// This is the well-known Hardhat/Anvil account #0 key.
    fn test_signing_key() -> SigningKey {
        let bytes =
            hex::decode("ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80")
                .unwrap();
        let secret = SecretKey::from_slice(&bytes).unwrap();
        SigningKey::from(secret)
    }

    fn test_verifying_key() -> VerifyingKey {
        *test_signing_key().verifying_key()
    }

    // ------------------------------------------------------------------
    // 1. SEC1 public-key round-trip
    // ------------------------------------------------------------------

    #[test]
    fn sec1_roundtrip_to_verifying_key() {
        let vk = test_verifying_key();
        let point = vk.to_encoded_point(false); // uncompressed SEC1
        let parsed = VerifyingKey::from_sec1_bytes(point.as_bytes()).unwrap();
        assert_eq!(vk, parsed);
    }

    // ------------------------------------------------------------------
    // 2. Public key → Ethereum address
    // ------------------------------------------------------------------

    #[test]
    fn public_key_to_ethereum_address() {
        let vk = test_verifying_key();
        let addr = public_key_to_address(&vk);

        // Known address for the test private key above
        // (Hardhat/Anvil account #0)
        let expected: Address = hex::decode("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266")
            .unwrap()
            .try_into()
            .unwrap();

        assert_eq!(addr, expected);
    }

    // ------------------------------------------------------------------
    // 3 + 4. low-S normalization (already-normalized path)
    // ------------------------------------------------------------------

    #[test]
    fn der_to_low_s() {
        let sk = test_signing_key();
        let msg = keccak256(b"test message for low-S");
        let (sig, _recid) = sk.sign_prehash_recoverable(&msg).unwrap();

        let normalized = sig.normalize_s().unwrap_or(sig);

        // After normalization the high bit of S must be clear
        let s_bytes = normalized.s().to_bytes();
        assert!(
            s_bytes[0] <= 0x7f,
            "S is still high after normalization: {:02x?}",
            s_bytes
        );
    }

    // ------------------------------------------------------------------
    // 5 + 6. Recovery ID + recovered pubkey == original
    // ------------------------------------------------------------------

    #[test]
    fn recovery_id_and_pubkey_match() {
        let sk = test_signing_key();
        let vk = test_verifying_key();
        let msg = keccak256(b"recovery test");

        let (sig, expected_recid) = sk.sign_prehash_recoverable(&msg).unwrap();
        let sig = sig.normalize_s().unwrap_or(sig);

        let recovered_recid = recover_id(&sig, &msg, &vk).unwrap();
        assert_eq!(recovered_recid.to_byte(), expected_recid.to_byte());

        let recovered_vk =
            VerifyingKey::recover_from_prehash(&msg, &sig, recovered_recid).unwrap();
        assert_eq!(recovered_vk, vk);
    }

    // ------------------------------------------------------------------
    // 7. Final r || s || v signature correctness
    // ------------------------------------------------------------------

    #[test]
    fn final_rsv_signature_verifies() {
        let sk = test_signing_key();
        let vk = test_verifying_key();
        let msg = keccak256(b"final rsv test");

        let (sig, _) = sk.sign_prehash_recoverable(&msg).unwrap();
        let sig = sig.normalize_s().unwrap_or(sig);
        let recid = recover_id(&sig, &msg, &vk).unwrap();

        let mut bytes = [0u8; 65];
        bytes[..32].copy_from_slice(&sig.r().to_bytes());
        bytes[32..64].copy_from_slice(&sig.s().to_bytes());
        bytes[64] = recid.to_byte();

        let reconstructed = K256Signature::from_slice(&bytes[..64]).unwrap();
        let recovered =
            VerifyingKey::recover_from_prehash(&msg, &reconstructed, recid).unwrap();
        assert_eq!(recovered, vk);
    }

    // ------------------------------------------------------------------
    // 8. EIP-191 message hashing
    // ------------------------------------------------------------------

    #[test]
    fn eip191_hash() {
        let message = b"Hello, Ethereum!";
        let mut buf = Vec::new();
        buf.extend_from_slice(b"\x19Ethereum Signed Message:\n");
        buf.extend_from_slice(message.len().to_string().as_bytes());
        buf.extend_from_slice(message);
        let hash = keccak256(&buf);

        assert_eq!(hash.len(), 32);
        assert!(hash.iter().any(|&b| b != 0));
    }

    // ------------------------------------------------------------------
    // 9. Address mismatch fails closed
    // ------------------------------------------------------------------

    #[test]
    fn address_mismatch_is_detectable() {
        let vk = test_verifying_key();
        let real_addr = public_key_to_address(&vk);

        let wrong: Address = [0u8; 20];
        assert_ne!(real_addr, wrong);

        let err = Error::AddressMismatch {
            expected: hex::encode(wrong),
            got: hex::encode(real_addr),
        };
        let msg = err.to_string();
        assert!(msg.contains("address mismatch"));
        assert!(msg.contains(&hex::encode(real_addr)));
    }

    // ------------------------------------------------------------------
    // 10. Exact production DER boundary
    //
    // This is the missing proof:
    //   local signature
    //        ↓
    //   encode to DER          (same wire format KMS returns)
    //        ↓
    //   K256Signature::from_der()   ← production path
    //        ↓
    //   normalize_s()
    //        ↓
    //   recover_id()
    //        ↓
    //   r || s || v
    //        ↓
    //   recovered pubkey == original
    // ------------------------------------------------------------------

    #[test]
    fn der_boundary_matches_production_path() {
        let sk = test_signing_key();
        let vk = test_verifying_key();
        let msg = keccak256(b"der boundary production path");

        // 1. Generate a deterministic local signature
        let (original_sig, original_recid) = sk.sign_prehash_recoverable(&msg).unwrap();

        // 2. Encode it to DER (the exact format AWS KMS returns)
        let der = original_sig.to_der();
        let der_bytes = der.as_bytes();

        // 3. Parse that DER with the same call used in production
        let parsed = K256Signature::from_der(der_bytes)
            .expect("from_der must accept a DER signature produced by k256 itself");

        // 4. Normalize S (production path)
        let normalized = parsed.normalize_s().unwrap_or(parsed);

        // 5. Recover the public key using the same helper production uses
        let recid = recover_id(&normalized, &msg, &vk)
            .expect("recovery ID must be found for a valid signature");

        // 6. Confirm recovered public key equals the original
        let recovered =
            VerifyingKey::recover_from_prehash(&msg, &normalized, recid).unwrap();
        assert_eq!(recovered, vk);

        // 7. Confirm the final r || s || v form
        let mut rsv = [0u8; 65];
        rsv[..32].copy_from_slice(&normalized.r().to_bytes());
        rsv[32..64].copy_from_slice(&normalized.s().to_bytes());
        rsv[64] = recid.to_byte();

        // The recovery ID we computed must be consistent with the original
        // (after low-S normalization the parity can flip, but the recovered
        // key must still match).
        assert_eq!(
            VerifyingKey::recover_from_prehash(
                &msg,
                &K256Signature::from_slice(&rsv[..64]).unwrap(),
                recid
            )
            .unwrap(),
            vk
        );

        // Sanity: original recovery ID is in {0,1}
        assert!(original_recid.to_byte() <= 1);
    }
}