// src/tx_signer_factory.rs
//! Production transaction-signer factory for omega-engine.
//!
//! Selects the gas-paying outer-envelope backend from env:
//!
//! ```text
//! OMEGA_TX_SIGNER=local   (default) — OMEGA_TX_SIGNING_KEY hex SecretKey
//! OMEGA_TX_SIGNER=kms               — AWS KMS (no in-memory gas key)
//! ```
//!
//! KMS mode requires `AWS_KMS_KEY_ID`, AWS region/credentials, and optionally
//! `OMEGA_TX_EXPECTED_ADDRESS=0x...` as a fail-closed address check.
//!
//! Blueprint authorization is always the local `BlueprintSigner` built from
//! `OMEGA_BLUEPRINT_SIGNING_KEY`, in both modes.
//!
//! ## Changelog
//!
//! - C10p: `next_tx_nonce` restored. The submitProof keeper in `main.rs` takes nonces from
//!   the signer's shared envelope counter (the same one `ExecutionPipeline` uses) so the
//!   two can't collide. `peek_tx_nonce` stays removed (no callers; C10o).
//! - C10l: `orchestrator_bp_hash` forwarding. `OmegaVault` keys pending profit and ZK
//!   public-inputs binding on the Orchestrator's domain-separated `bpHash`, not on
//!   `ExecutionBlueprint::compute_hash()`. The `Kms` arm forwards to a method the KMS
//!   adapter must also provide.
//! - C10k: the KMS backend is feature-gated (`--features kms`), OFF by default. It depends
//!   on `aws_kms_signer` (not a real crate name) and `aws_kms_omega_adapter` (never
//!   written), so `--features kms` fails until a real adapter crate exists. A default
//!   build needs only the `local` backend.

use std::collections::HashMap;
use std::sync::Arc;

use alloy_primitives::Address;
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
#[cfg(feature = "kms")]
use aws_kms_omega_adapter::AwsKmsTransactionSigner;
#[cfg(feature = "kms")]
use aws_kms_signer::AwsKmsSigner;
use omega_core::ExecutionBlueprint;
use omega_execution::signer::KeyManagerTransactionSigner;
use omega_execution::{ExecutionError, SignedTransaction, TransactionSigner};
use omega_security::{BlueprintSigner, KeyManager};

/// Unified production signer used by `ExecutionPipeline` and the ZK keeper.
///
/// Implements `TransactionSigner` for the orchestrator path and exposes
/// `sign_call_gwei` for `OmegaVault.submitProof`.
pub enum ProductionTxSigner {
    Local(KeyManagerTransactionSigner),
    #[cfg(feature = "kms")]
    Kms(AwsKmsTransactionSigner),
}

impl ProductionTxSigner {
    pub fn active_address(&self) -> [u8; 20] {
        match self {
            Self::Local(s) => s.active_address(),
            #[cfg(feature = "kms")]
            Self::Kms(s) => s.active_address(),
        }
    }

    pub fn backend_name(&self) -> &'static str {
        match self {
            Self::Local(_) => "local",
            #[cfg(feature = "kms")]
            Self::Kms(_) => "kms",
        }
    }

    /// The Orchestrator's domain-separated `bpHash` for `bp` — the key `OmegaVault`
    /// uses for `pending_profit` / `proofInputsBound` / `boundProofInputsOf`.
    pub fn orchestrator_bp_hash(
        &self,
        bp: &ExecutionBlueprint,
        chain_id: u64,
    ) -> Result<[u8; 32], ExecutionError> {
        match self {
            Self::Local(s) => s.orchestrator_bp_hash(bp, chain_id),
            #[cfg(feature = "kms")]
            Self::Kms(s) => s.orchestrator_bp_hash(bp, chain_id),
        }
    }

    /// Seed the shared outer-envelope nonce counter.
    ///
    /// `pending_count` must be `eth_getTransactionCount(active_address(), "pending")`.
    /// Call once at startup and again after any rejected/dropped send.
    pub fn seed_tx_nonce(&self, pending_count: u64) {
        match self {
            Self::Local(s) => s.seed_tx_nonce(pending_count),
            #[cfg(feature = "kms")]
            Self::Kms(s) => s.seed_tx_nonce(pending_count),
        }
    }

    /// Take the next nonce from the shared outer-envelope counter (used by the
    /// `ExecutionPipeline` and the submitProof keeper, so they never collide).
    /// Errors if the counter has not been seeded.
    pub fn next_tx_nonce(&self) -> Result<u64, ExecutionError> {
        match self {
            Self::Local(s) => s.next_tx_nonce(),
            #[cfg(feature = "kms")]
            Self::Kms(s) => s.next_tx_nonce(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn sign_call_gwei(
        &self,
        chain_id: u64,
        nonce: u64,
        to: [u8; 20],
        data: &[u8],
        gas_limit: u64,
        priority_fee_gwei: u64,
        max_fee_gwei: u64,
    ) -> Result<SignedTransaction, ExecutionError> {
        match self {
            Self::Local(s) => s.sign_call_gwei(
                chain_id,
                nonce,
                to,
                data,
                gas_limit,
                priority_fee_gwei,
                max_fee_gwei,
            ),
            #[cfg(feature = "kms")]
            Self::Kms(s) => {
                s.sign_call_gwei(
                    chain_id,
                    nonce,
                    to,
                    data,
                    gas_limit,
                    priority_fee_gwei,
                    max_fee_gwei,
                )
                .await
            }
        }
    }
}

#[async_trait]
impl TransactionSigner for ProductionTxSigner {
    async fn sign_transaction(
        &self,
        bp: &ExecutionBlueprint,
        chain_id: u64,
    ) -> Result<SignedTransaction, ExecutionError> {
        match self {
            Self::Local(s) => s.sign_transaction(bp, chain_id).await,
            #[cfg(feature = "kms")]
            Self::Kms(s) => s.sign_transaction(bp, chain_id).await,
        }
    }
}

/// Build the production outer-envelope + blueprint-auth signer pair.
///
/// `orchestrator` and `strategy_onchain_ids` are shared; only the gas key
/// backend differs.
pub async fn build_production_tx_signer(
    chain_id: u64,
    orchestrator: Address,
    strategy_onchain_ids: HashMap<String, [u8; 32]>,
) -> Result<Arc<ProductionTxSigner>> {
    let backend = std::env::var("OMEGA_TX_SIGNER")
        .unwrap_or_else(|_| "local".into())
        .to_lowercase();

    let blueprint_signing_key_hex = std::env::var("OMEGA_BLUEPRINT_SIGNING_KEY").context(
        "OMEGA_BLUEPRINT_SIGNING_KEY must be set — hex secp256k1 key whose address must match \
         OmegaOrchestrator.execution_key (or pending_key) on-chain",
    )?;
    let blueprint_key_manager = Arc::new(
        KeyManager::from_hex(&blueprint_signing_key_hex, chain_id)
            .context("constructing blueprint_key_manager from OMEGA_BLUEPRINT_SIGNING_KEY")?,
    );
    let blueprint_signer = Arc::new(BlueprintSigner::new(blueprint_key_manager));

    match backend.as_str() {
        "local" | "" => {
            let tx_signing_key_hex = std::env::var("OMEGA_TX_SIGNING_KEY")
                .context("OMEGA_TX_SIGNING_KEY must be set when OMEGA_TX_SIGNER=local (default)")?;
            let tx_key_manager = Arc::new(
                KeyManager::from_hex(&tx_signing_key_hex, chain_id)
                    .context("constructing tx_key_manager from OMEGA_TX_SIGNING_KEY")?,
            );
            let local = KeyManagerTransactionSigner::new(
                tx_key_manager,
                orchestrator,
                strategy_onchain_ids,
                blueprint_signer,
            );
            tracing::info!(
                backend = "local",
                tx_signer_address = %hex::encode(local.active_address()),
                "ProductionTxSigner: local KeyManager gas key"
            );
            Ok(Arc::new(ProductionTxSigner::Local(local)))
        }
        "kms" => {
            #[cfg(not(feature = "kms"))]
            {
                bail!(
                    "OMEGA_TX_SIGNER=kms requires this binary to be built with \
                     `--features kms` — and even then, the KMS backend currently \
                     depends on a not-yet-written local `aws-kms-omega-adapter` crate \
                     (see this file's own C10k changelog entry). Build with \
                     OMEGA_TX_SIGNER=local (or unset) until that adapter crate exists."
                );
            }
            #[cfg(feature = "kms")]
            {
                let key_id = std::env::var("AWS_KMS_KEY_ID")
                    .context("AWS_KMS_KEY_ID must be set when OMEGA_TX_SIGNER=kms")?;
                let expected = match std::env::var("OMEGA_TX_EXPECTED_ADDRESS") {
                    Ok(s) => Some(parse_eth_address(&s).context("OMEGA_TX_EXPECTED_ADDRESS")?),
                    Err(_) => None,
                };

                // aws-config lives only at the binary edge — not inside aws-kms-signer.
                let aws_cfg = aws_config::load_from_env().await;
                let client = aws_sdk_kms::Client::new(&aws_cfg);
                let kms = AwsKmsSigner::new(client, key_id, expected)
                    .await
                    .map_err(|e| anyhow::anyhow!("AwsKmsSigner::new failed: {e}"))?;

                let kms_signer = AwsKmsTransactionSigner::new(
                    kms,
                    orchestrator,
                    strategy_onchain_ids,
                    blueprint_signer,
                );
                tracing::info!(
                    backend = "kms",
                    tx_signer_address = %hex::encode(kms_signer.active_address()),
                    "ProductionTxSigner: AWS KMS gas key (private key never leaves KMS)"
                );
                Ok(Arc::new(ProductionTxSigner::Kms(kms_signer)))
            }
        }
        other => bail!("unknown OMEGA_TX_SIGNER={other:?}; expected \"local\" or \"kms\""),
    }
}

#[cfg(feature = "kms")]
fn parse_eth_address(s: &str) -> Result<[u8; 20]> {
    let s = s.trim().strip_prefix("0x").unwrap_or(s.trim());
    let bytes = hex::decode(s).context("hex decode")?;
    if bytes.len() != 20 {
        bail!("expected 20-byte address, got {}", bytes.len());
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&bytes);
    Ok(out)
}
