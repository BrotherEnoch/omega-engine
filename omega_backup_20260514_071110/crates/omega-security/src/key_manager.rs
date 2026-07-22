// crates/omega-security/src/key_manager.rs
//
// Execution-key manager with dual-key rotation window (spec S3 / S5).
//
// Spec references:
//   OmegaOrchestrator.sol:
//     address public execution_key;
//     address public pending_key;           // dual-key rotation window
//     uint64  public rotation_window_end_block;
//
//     function _accepts_key(address k):
//       if k == execution_key â†’ true
//       if pending_key != 0 && k == pending_key && block <= window_end â†’ true
//
//   S5 governance: Key rotation requires L2 fast-approve (2-of-5 multisig).
//   Section 3 (Health FSM): key rotation logged to health persistence layer.
//   Appendix C: Ledger/CloudHSM/Fireblocks; 5-minute L2 escalation; rotation ceremony.
//
// State machine:
//   Active     â€” one live execution key, no rotation in progress.
//   Rotating   â€” pending_key set; both keys accepted for rotation_window_blocks.
//   Completing â€” rotation window closing; next block_tick() completes it.
//
// Thread-safety:
//   KeyRotationState is behind a parking_lot::RwLock.
//   active_secret_key() is a fast read-lock path (hot path for signing).
//
// Security:
//   SecretKey is zeroized on drop via the secp256k1 crate's ZeroizeOnDrop feature.
//   In production the secret key bytes are loaded from HSM / env var; this
//   module stores them in memory only for the signing operation lifetime.

use std::sync::Arc;
use parking_lot::RwLock;
use chrono::{DateTime, Utc};
use secp256k1::SecretKey;
use serde::{Deserialize, Serialize};

use crate::error::SecurityError;
use crate::metrics;
use crate::signer::secret_key_to_address;

/// Rotation window in blocks (spec: ~15 seconds on Arbitrum at 250ms/block = 60 blocks).
/// Matches `SequencerRestartHandler.restart_window_blocks` = 60 for consistency.
pub const ROTATION_WINDOW_BLOCKS: u64 = 60;

/// Observable rotation state (no key bytes â€” safe to serialize/log).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyRotationState {
    /// Single active key; no rotation pending.
    Active {
        /// Ethereum address (hex) of the active key â€” safe to log.
        address: String,
    },
    /// Rotation in progress: both keys accepted until `window_end_block`.
    Rotating {
        active_address:  String,
        pending_address: String,
        window_end_block: u64,
        initiated_at:    DateTime<Utc>,
    },
}

impl KeyRotationState {
    pub fn is_rotating(&self) -> bool {
        matches!(self, KeyRotationState::Rotating { .. })
    }
}

/// Internal key state (holds actual secret key bytes).
struct KeyState {
    active_key:   SecretKey,
    active_addr:  [u8; 20],
    pending_key:  Option<SecretKey>,
    pending_addr: Option<[u8; 20]>,
    window_end:   Option<u64>,
    initiated_at: Option<DateTime<Utc>>,
}

/// Execution-key manager.
///
/// `Arc<KeyManager>` is shared between `BlueprintSigner` and the governance
/// control-plane handler.
pub struct KeyManager {
    state:       RwLock<KeyState>,
    secp:        secp256k1::Secp256k1<secp256k1::All>,
    expected_chain_id: u64,
}

impl KeyManager {
    /// Create from a raw `SecretKey` (used in tests and local dev).
    pub fn from_secret_key(sk: SecretKey) -> Self {
        let secp = secp256k1::Secp256k1::new();
        let addr = secret_key_to_address(&secp, &sk);
        let state = KeyState {
            active_key:   sk,
            active_addr:  addr,
            pending_key:  None,
            pending_addr: None,
            window_end:   None,
            initiated_at: None,
        };
        Self {
            state: RwLock::new(state),
            secp,
            expected_chain_id: 42161,
        }
    }

    /// Create from hex-encoded secret key bytes (32 bytes, 64 hex chars, no 0x prefix).
    pub fn from_hex(hex_key: &str, chain_id: u64) -> Result<Self, SecurityError> {
        let bytes = hex::decode(hex_key.trim_start_matches("0x"))
            .map_err(|e| SecurityError::SigningFailed { detail: e.to_string() })?;
        if bytes.len() != 32 {
            return Err(SecurityError::SigningFailed {
                detail: format!("expected 32 key bytes, got {}", bytes.len()),
            });
        }
        let sk = SecretKey::from_slice(&bytes)
            .map_err(|e| SecurityError::SigningFailed { detail: e.to_string() })?;
        let secp = secp256k1::Secp256k1::new();
        let addr = secret_key_to_address(&secp, &sk);

        tracing::info!(
            chain_id,
            address = hex::encode(addr),
            "KeyManager initialised"
        );

        let state = KeyState {
            active_key:   sk,
            active_addr:  addr,
            pending_key:  None,
            pending_addr: None,
            window_end:   None,
            initiated_at: None,
        };
        Ok(Self {
            state: RwLock::new(state),
            secp,
            expected_chain_id: chain_id,
        })
    }

    // â”€â”€ Hot-path read â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Return a clone of the active signing key (called on every blueprint sign).
    /// Lock-contention: RwLock read â€” multiple signers can read concurrently.
    #[inline]
    pub fn active_secret_key(&self) -> Option<SecretKey> {
        let s = self.state.read();
        Some(s.active_key)
    }

    /// Return the active Ethereum address (for Flashbots header and replay guard).
    pub fn active_address(&self) -> [u8; 20] {
        self.state.read().active_addr
    }

    /// True if `addr` is accepted under the current key state.
    ///
    /// Implements `OmegaOrchestrator._accepts_key()` logic in Rust so the
    /// simulation path can validate signatures before on-chain submission.
    pub fn accepts_address(&self, addr: &[u8; 20], current_block: u64) -> bool {
        let s = self.state.read();
        if &s.active_addr == addr {
            return true;
        }
        if let (Some(pa), Some(we)) = (&s.pending_addr, s.window_end) {
            if pa == addr && current_block <= we {
                return true;
            }
        }
        false
    }

    // â”€â”€ Rotation â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Initiate a dual-key rotation.
    ///
    /// Called by the governance handler after L2 fast-approve (2-of-5 multisig).
    /// The new key is accepted as `pending_key` for `ROTATION_WINDOW_BLOCKS`.
    ///
    /// Returns `Err(RotationAlreadyPending)` if a rotation is already in progress.
    pub fn initiate_rotation(
        &self,
        new_key:       SecretKey,
        current_block: u64,
    ) -> Result<KeyRotationState, SecurityError> {
        let mut s = self.state.write();
        if s.pending_key.is_some() {
            return Err(SecurityError::RotationAlreadyPending);
        }

        let new_addr      = secret_key_to_address(&self.secp, &new_key);
        let window_end    = current_block + ROTATION_WINDOW_BLOCKS;
        let initiated_at  = Utc::now();

        s.pending_key  = Some(new_key);
        s.pending_addr = Some(new_addr);
        s.window_end   = Some(window_end);
        s.initiated_at = Some(initiated_at);

        tracing::warn!(
            active  = hex::encode(s.active_addr),
            pending = hex::encode(new_addr),
            window_end,
            "key rotation initiated â€” dual-key window open"
        );

        metrics::KEY_ROTATIONS.inc();

        Ok(KeyRotationState::Rotating {
            active_address:   hex::encode(s.active_addr),
            pending_address:  hex::encode(new_addr),
            window_end_block: window_end,
            initiated_at,
        })
    }

    /// Called on every new block to advance the rotation state machine.
    ///
    /// If a rotation is in progress and `current_block > window_end`, the pending
    /// key is promoted to active and the old key is dropped (zeroed).
    pub fn on_new_block(&self, current_block: u64) {
        let needs_complete = {
            let s = self.state.read();
            matches!(s.window_end, Some(we) if current_block > we)
        };

        if needs_complete {
            let mut s = self.state.write();
            if let (Some(pk), Some(pa), Some(we)) =
                (s.pending_key.take(), s.pending_addr.take(), s.window_end.take())
            {
                let old_addr = s.active_addr;
                s.active_key  = pk;
                s.active_addr = pa;
                s.initiated_at = None;

                tracing::warn!(
                    old_address = hex::encode(old_addr),
                    new_address = hex::encode(pa),
                    completed_at_block = current_block,
                    "key rotation completed â€” old key decommissioned"
                );
            }
        }
    }

    /// Return the current observable rotation state (no secret bytes exposed).
    pub fn rotation_state(&self) -> KeyRotationState {
        let s = self.state.read();
        match (s.pending_addr, s.window_end, s.initiated_at) {
            (Some(pa), Some(we), Some(ia)) => KeyRotationState::Rotating {
                active_address:   hex::encode(s.active_addr),
                pending_address:  hex::encode(pa),
                window_end_block: we,
                initiated_at:     ia,
            },
            _ => KeyRotationState::Active {
                address: hex::encode(s.active_addr),
            },
        }
    }

    /// Cancel a pending rotation (governance veto path).
    pub fn cancel_rotation(&self) {
        let mut s = self.state.write();
        if let Some(pa) = s.pending_addr.take() {
            s.pending_key  = None;
            s.window_end   = None;
            s.initiated_at = None;
            tracing::warn!(
                pending = hex::encode(pa),
                "key rotation CANCELLED by governance"
            );
        }
    }
}

#[cfg(test)]
mod key_manager_tests {
    use super::*;
    use secp256k1::SecretKey;

    fn sk(byte: u8) -> SecretKey { SecretKey::from_slice(&[byte; 32]).unwrap() }
    fn km(byte: u8) -> KeyManager { KeyManager::from_secret_key(sk(byte)) }

    #[test]
    fn initial_state_is_active() {
        let m = km(1);
        assert!(!m.rotation_state().is_rotating());
    }

    #[test]
    fn active_key_is_returned() {
        let m = km(0x42);
        assert!(m.active_secret_key().is_some());
    }

    #[test]
    fn rotation_opens_dual_key_window() {
        let m = km(0x01);
        m.initiate_rotation(sk(0x02), 100).unwrap();
        assert!(m.rotation_state().is_rotating());
    }

    #[test]
    fn double_rotation_returns_error() {
        let m = km(0x01);
        m.initiate_rotation(sk(0x02), 100).unwrap();
        assert!(matches!(
            m.initiate_rotation(sk(0x03), 100),
            Err(SecurityError::RotationAlreadyPending)
        ));
    }

    #[test]
    fn both_keys_accepted_during_window() {
        let secp = secp256k1::Secp256k1::new();
        let m    = km(0x01);
        let addr_active = m.active_address();
        let new_sk = sk(0x02);
        let addr_pending = secret_key_to_address(&secp, &new_sk);
        m.initiate_rotation(new_sk, 1000).unwrap();

        assert!(m.accepts_address(&addr_active,  1010)); // old key still valid
        assert!(m.accepts_address(&addr_pending, 1010)); // new key valid in window
        assert!(!m.accepts_address(&[0xff; 20],  1010)); // random key rejected
    }

    #[test]
    fn pending_key_rejected_after_window_expires() {
        let secp = secp256k1::Secp256k1::new();
        let m = km(0x01);
        let new_sk = sk(0x02);
        let addr_pending = secret_key_to_address(&secp, &new_sk);
        m.initiate_rotation(new_sk, 1000).unwrap();

        // Block 1061 > window_end (1060)
        assert!(!m.accepts_address(&addr_pending, 1061));
    }

    #[test]
    fn on_new_block_promotes_pending_key() {
        let secp = secp256k1::Secp256k1::new();
        let m = km(0x01);
        let new_sk = sk(0x02);
        let new_addr = secret_key_to_address(&secp, &new_sk);
        m.initiate_rotation(new_sk, 1000).unwrap();

        // Complete: block > window_end = 1060
        m.on_new_block(1061);

        assert!(!m.rotation_state().is_rotating());
        assert_eq!(m.active_address(), new_addr);
    }

    #[test]
    fn cancel_rotation_clears_pending() {
        let m = km(0x01);
        m.initiate_rotation(sk(0x02), 100).unwrap();
        assert!(m.rotation_state().is_rotating());
        m.cancel_rotation();
        assert!(!m.rotation_state().is_rotating());
    }

    #[test]
    fn from_hex_valid_key() {
        let key_hex = hex::encode([0x42u8; 32]);
        let km = KeyManager::from_hex(&key_hex, 42161).unwrap();
        assert!(km.active_secret_key().is_some());
    }

    #[test]
    fn from_hex_invalid_length_errors() {
        assert!(KeyManager::from_hex("0xdead", 42161).is_err());
    }
}