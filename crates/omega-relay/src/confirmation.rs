// crates/omega-relay/src/confirmation.rs
//! Real bundle inclusion confirmation.
//!
//! The prior version of `client.rs` recorded `included: true` the moment a relay's HTTP
//! endpoint returned 200 with no JSON-RPC error — which only means the relay agreed to
//! *consider* the bundle, not that it landed on-chain. That fake signal fed directly into
//! `LaRelayMetrics`, which every ranking/cascade/reputation-carryover decision in this
//! crate depends on. This module is the real signal: it checks actual chain state.
//!
//! ## Design
//!
//! Ground truth about what's on-chain should come from the chain itself, not from a
//! relay's self-reported dashboard — different relays likely have different
//! inclusion-status APIs (Flashbots has one; bloXroute/Titan/Eden's exact equivalents
//! aren't something I have confident, verifiable knowledge of, so I'm not going to guess
//! at four different provider-specific API shapes). Instead: after a relay *accepts* a
//! submission, track it as pending against its target block. Once that block (plus a
//! small grace window, for RPC/propagation lag) has passed, check every transaction hash
//! in the bundle via a standard `eth_getTransactionReceipt` call against a regular chain
//! RPC endpoint. A bundle is confirmed included only if every one of its transactions has
//! a successful receipt.
//!
//! This is deliberately decoupled from the fast submission path: `track()` is
//! synchronous and cheap (just records intent), and `reconcile()` is the async step that
//! does network I/O, meant to be called once per new block alongside the existing
//! `on_new_block()` — not on the hot submission path, which shouldn't have to wait a
//! full block (or several) for a decision.
//!
//! A transaction's hash is `keccak256` of its raw signed bytes directly — true for both
//! legacy and EIP-2718 typed transactions — so no RLP field-level parsing is needed here.

use std::sync::Arc;

use dashmap::DashMap;
use serde::Deserialize;
use serde_json::json;
use tracing::debug;

use crate::client::BundlePayload;
use crate::config::RelayName;
use crate::error::{RelayError, RelayResult};

/// Grace period (in blocks) after a bundle's target block before giving up on
/// confirming it, to account for RPC/propagation lag. Matches `reorg::STABILITY_WINDOW_BLOCKS`
/// for consistency — both exist to absorb the same kind of short-term chain-state noise.
pub const CONFIRMATION_GRACE_BLOCKS: u64 = 5;

struct PendingBundle {
    relay: RelayName,
    tx_hashes: Vec<[u8; 32]>,
    target_block: u64,
}

/// Result of a resolved (confirmed-included or given-up-on) bundle.
#[derive(Debug, Clone)]
pub struct ConfirmationResult {
    /// Bundle hash this result is for.
    pub bundle_hash: String,
    /// Relay this bundle was tracked against.
    pub relay: RelayName,
    /// `true` only if every transaction in the bundle has a successful on-chain receipt.
    pub included: bool,
}

/// Tracks bundles pending on-chain inclusion confirmation.
pub struct InclusionTracker {
    rpc_url: String,
    http: reqwest::Client,
    pending: DashMap<String, PendingBundle>,
}

impl InclusionTracker {
    /// Construct a tracker against a standard chain JSON-RPC endpoint (NOT a relay
    /// endpoint — a regular node, since this checks canonical chain state).
    pub fn new(rpc_url: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            rpc_url: rpc_url.into(),
            http: reqwest::Client::new(),
            pending: DashMap::new(),
        })
    }

    /// Record a bundle as pending confirmation after a relay has accepted it.
    /// Cheap and synchronous — safe to call on the hot submission path.
    pub fn track(&self, relay: RelayName, bundle: &BundlePayload) -> RelayResult<()> {
        let target_block = parse_hex_block(&bundle.block_number)?;
        let tx_hashes: RelayResult<Vec<[u8; 32]>> =
            bundle.txs.iter().map(|t| tx_hash_of_raw(t)).collect();
        self.pending.insert(
            bundle.bundle_hash.clone(),
            PendingBundle { relay, tx_hashes: tx_hashes?, target_block },
        );
        Ok(())
    }

    /// Call once per new canonical block, alongside `on_new_block()`. Resolves every
    /// pending bundle whose target block has been reached (plus grace window), querying
    /// real chain state, and returns the results — feeding these into `LaRelayMetrics`
    /// is the caller's responsibility (see `MultiRelayClient::reconcile_inclusions`).
    pub async fn reconcile(&self, current_block: u64) -> Vec<ConfirmationResult> {
        let due: Vec<String> = self
            .pending
            .iter()
            .filter(|e| current_block >= e.value().target_block)
            .map(|e| e.key().clone())
            .collect();

        let mut resolved = Vec::with_capacity(due.len());

        for bundle_hash in due {
            let Some((_, pending)) = self.pending.remove(&bundle_hash) else { continue };

            let included = self.check_all_included(&pending.tx_hashes).await;

            if !included && current_block < pending.target_block + CONFIRMATION_GRACE_BLOCKS {
                // Not yet confirmed, but still within the grace window — re-queue rather
                // than prematurely recording a false negative.
                self.pending.insert(bundle_hash, pending);
                continue;
            }

            debug!(
                bundle_hash = %bundle_hash,
                relay       = %pending.relay,
                included,
                target_block = pending.target_block,
                current_block,
                "confirmation: bundle resolved"
            );

            resolved.push(ConfirmationResult { bundle_hash, relay: pending.relay, included });
        }

        resolved
    }

    /// Number of bundles currently awaiting confirmation (observability).
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    async fn check_all_included(&self, tx_hashes: &[[u8; 32]]) -> bool {
        if tx_hashes.is_empty() {
            return false;
        }
        for h in tx_hashes {
            match self.get_receipt_status(h).await {
                Ok(true) => continue,
                _ => return false,
            }
        }
        true
    }

    async fn get_receipt_status(&self, tx_hash: &[u8; 32]) -> RelayResult<bool> {
        let hash_hex = format!("0x{}", hex::encode(tx_hash));
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_getTransactionReceipt",
            "params": [hash_hex],
        });

        let resp = self
            .http
            .post(&self.rpc_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| RelayError::ConfirmationRpcFailed(e.to_string()))?;

        let parsed: ReceiptRpcResponse = resp
            .json()
            .await
            .map_err(|e| RelayError::ConfirmationRpcFailed(e.to_string()))?;

        match parsed.result {
            Some(receipt) => Ok(receipt.status.as_deref() == Some("0x1")),
            None => Ok(false), // not mined yet (or never will be)
        }
    }
}

#[derive(Deserialize)]
struct ReceiptRpcResponse {
    result: Option<TxReceipt>,
}

#[derive(Deserialize)]
struct TxReceipt {
    status: Option<String>,
}

/// Transaction hash of a raw signed transaction: `keccak256` of its exact bytes. True
/// for both legacy and EIP-2718 typed transactions (`keccak256(type || payload)`), so no
/// RLP field parsing is required here.
fn tx_hash_of_raw(raw_tx_hex: &str) -> RelayResult<[u8; 32]> {
    use sha3::{Digest, Keccak256};
    let trimmed = raw_tx_hex.trim_start_matches("0x");
    let bytes = hex::decode(trimmed)
        .map_err(|e| RelayError::ConfigInvalid(format!("invalid raw tx hex: {e}")))?;
    let mut hasher = Keccak256::new();
    hasher.update(&bytes);
    Ok(hasher.finalize().into())
}

fn parse_hex_block(block_hex: &str) -> RelayResult<u64> {
    let trimmed = block_hex.trim_start_matches("0x");
    u64::from_str_radix(trimmed, 16)
        .map_err(|e| RelayError::ConfigInvalid(format!("invalid block number '{block_hex}': {e}")))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_block_number() {
        assert_eq!(parse_hex_block("0x1a").unwrap(), 26);
        assert_eq!(parse_hex_block("0x0").unwrap(), 0);
    }

    #[test]
    fn rejects_malformed_block_number() {
        assert!(parse_hex_block("not-hex").is_err());
    }

    #[test]
    fn tx_hash_is_deterministic_and_correct_length() {
        let h1 = tx_hash_of_raw("0xdeadbeef").unwrap();
        let h2 = tx_hash_of_raw("0xdeadbeef").unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 32);
    }

    #[test]
    fn different_tx_bytes_give_different_hashes() {
        let h1 = tx_hash_of_raw("0xdeadbeef").unwrap();
        let h2 = tx_hash_of_raw("0xcafebabe").unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn track_and_pending_count() {
        let tracker = InclusionTracker::new("http://localhost:8545");
        let bundle = BundlePayload {
            bundle_hash: "0xabc".into(),
            txs: vec!["0xdeadbeef".into()],
            block_number: "0x64".into(),
            ..Default::default()
        };
        tracker.track(RelayName::Flashbots, &bundle).unwrap();
        assert_eq!(tracker.pending_count(), 1);
    }

    #[tokio::test]
    async fn reconcile_before_target_block_leaves_pending() {
        let tracker = InclusionTracker::new("http://localhost:8545");
        let bundle = BundlePayload {
            bundle_hash: "0xabc".into(),
            txs: vec!["0xdeadbeef".into()],
            block_number: "0x64".into(), // block 100
            ..Default::default()
        };
        tracker.track(RelayName::Flashbots, &bundle).unwrap();
        let results = tracker.reconcile(50).await; // current block 50, still before target
        assert!(results.is_empty());
        assert_eq!(tracker.pending_count(), 1);
    }

    #[tokio::test]
    async fn reconcile_past_grace_window_gives_up_and_resolves_false() {
        let tracker = InclusionTracker::new("http://127.0.0.1:1"); // deliberately unreachable
        let bundle = BundlePayload {
            bundle_hash: "0xabc".into(),
            txs: vec!["0xdeadbeef".into()],
            block_number: "0x64".into(), // block 100
            ..Default::default()
        };
        tracker.track(RelayName::Flashbots, &bundle).unwrap();
        // Past target block + grace window, RPC unreachable so never confirms —
        // must resolve as not-included rather than staying pending forever.
        let results = tracker.reconcile(100 + CONFIRMATION_GRACE_BLOCKS).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].included);
        assert_eq!(tracker.pending_count(), 0, "must not stay pending forever");
    }
}