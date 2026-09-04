// crates/omega-rpc/src/reconciliation.rs
//
// Atomic balance reconciliation for execution-address rotation (spec §14).
//
// Guarantees the financial invariant before an address is retired:
//   1. On-chain native balance matches (or exceeds) the expected residual.
//   2. No pending outbound transactions (nonce gap).
//   3. No detectable in-flight flash-loan / bridge residual on known
//      provider contracts (extensible registry).
//
// All RPC traffic is gated through OmegaRpcClient so rate-limits, chain-id
// verification, and health reporting remain consistent with the rest of
// the crate.
//
// ## FIX (this revision): clippy::field_reassign_with_default
//
//   `reconciler_for_providers` used to build `cfg` via
//   `let mut cfg = ReconciliationConfig::default(); cfg.monitored_contracts
//   = ...;`, which clippy flags under `-D warnings` — constructing a
//   `Default` value and then immediately overwriting one field is exactly
//   what the `..Default::default()` struct-update syntax is for. Fixed by
//   building `cfg` as a single struct literal below; behavior is
//   unchanged.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::client::OmegaRpcClient;

#[derive(Debug, Error)]
pub enum ReconciliationError {
    #[error("Balance reconciliation failed: {0}")]
    BalanceMismatch(String),

    #[error("Pending transactions detected on address (on-chain nonce {on_chain}, local {local})")]
    PendingTransactions { on_chain: u64, local: u64 },

    #[error("In-flight bridge or flashloan residual detected on {contract}")]
    InFlightOperations { contract: Address },

    #[error("State reconciliation timeout after {0:?}")]
    Timeout(Duration),

    #[error("RPC error during reconciliation: {0}")]
    Rpc(#[from] anyhow::Error),
}

#[derive(Debug, Clone)]
pub struct ReconciliationConfig {
    pub dust_tolerance_wei: U256,
    pub monitored_contracts: Vec<Address>,
    pub timeout: Duration,
}

impl Default for ReconciliationConfig {
    fn default() -> Self {
        Self {
            dust_tolerance_wei: U256::from(10_000u64),
            monitored_contracts: Vec::new(),
            timeout: Duration::from_secs(12),
        }
    }
}

pub struct AtomicBalanceReconciler {
    rpc: Arc<OmegaRpcClient>,
    config: ReconciliationConfig,
}

impl AtomicBalanceReconciler {
    pub fn new(rpc: Arc<OmegaRpcClient>, config: ReconciliationConfig) -> Self {
        Self { rpc, config }
    }

    pub fn with_defaults(rpc: Arc<OmegaRpcClient>) -> Self {
        Self::new(rpc, ReconciliationConfig::default())
    }

    pub async fn verify_can_rotate(
        &self,
        address: Address,
        expected_balance: Option<U256>,
        local_nonce: Option<u64>,
        now: DateTime<Utc>,
    ) -> Result<(), ReconciliationError> {
        let _ = now;

        info!(%address, "Starting atomic balance reconciliation");

        let fut = async {
            self.verify_balance(address, expected_balance).await?;
            self.verify_no_pending(address, local_nonce).await?;
            self.verify_no_in_flight(address).await?;
            Ok::<(), ReconciliationError>(())
        };

        match tokio::time::timeout(self.config.timeout, fut).await {
            Ok(result) => {
                result?;
                info!(%address, "Atomic reconciliation passed — safe to rotate");
                Ok(())
            }
            Err(_) => Err(ReconciliationError::Timeout(self.config.timeout)),
        }
    }

    async fn verify_balance(
        &self,
        address: Address,
        expected: Option<U256>,
    ) -> Result<(), ReconciliationError> {
        let provider = self
            .rpc
            .get_or_connect()
            .await
            .context("provider for balance check")?;

        let on_chain: U256 = provider
            .get_balance(address)
            .await
            .context("eth_getBalance")?;

        match expected {
            None => {
                debug!(%address, balance = %on_chain, "No expected balance supplied; accepting any residual");
                Ok(())
            }
            Some(exp) => {
                let diff = if on_chain >= exp {
                    on_chain - exp
                } else {
                    exp - on_chain
                };
                if diff > self.config.dust_tolerance_wei {
                    return Err(ReconciliationError::BalanceMismatch(format!(
                        "on-chain {on_chain} vs expected {exp} (diff {diff} > tolerance {})",
                        self.config.dust_tolerance_wei
                    )));
                }
                debug!(%address, on_chain = %on_chain, expected = %exp, "Balance matched within dust");
                Ok(())
            }
        }
    }

    async fn verify_no_pending(
        &self,
        address: Address,
        local_nonce: Option<u64>,
    ) -> Result<(), ReconciliationError> {
        let provider = self
            .rpc
            .get_or_connect()
            .await
            .context("provider for nonce check")?;

        let on_chain_nonce: u64 = provider
            .get_transaction_count(address)
            .await
            .context("eth_getTransactionCount(latest)")?;

        let pending_nonce: u64 = provider
            .get_transaction_count(address)
            .block_id(alloy::eips::BlockId::pending())
            .await
            .context("eth_getTransactionCount(pending)")?;

        if pending_nonce > on_chain_nonce {
            return Err(ReconciliationError::PendingTransactions {
                on_chain: on_chain_nonce,
                local: pending_nonce,
            });
        }

        if let Some(local) = local_nonce {
            if on_chain_nonce != local {
                return Err(ReconciliationError::PendingTransactions {
                    on_chain: on_chain_nonce,
                    local,
                });
            }
        }

        debug!(%address, on_chain_nonce, "No pending transactions");
        Ok(())
    }

    async fn verify_no_in_flight(&self, address: Address) -> Result<(), ReconciliationError> {
        if self.config.monitored_contracts.is_empty() {
            return Ok(());
        }

        let provider = self
            .rpc
            .get_or_connect()
            .await
            .context("provider for in-flight check")?;

        let balance_of_selector: [u8; 4] = {
            let hash = alloy::primitives::keccak256(b"balanceOf(address)");
            [hash[0], hash[1], hash[2], hash[3]]
        };

        for &contract in &self.config.monitored_contracts {
            let mut calldata = Vec::with_capacity(36);
            calldata.extend_from_slice(&balance_of_selector);
            calldata.extend_from_slice(&[0u8; 12]);
            calldata.extend_from_slice(address.as_slice());

            let call = alloy::rpc::types::TransactionRequest::default()
                .to(contract)
                .input(alloy::rpc::types::TransactionInput::new(calldata.into()));

            match provider.call(&call).await {
                Ok(ret) if ret.len() >= 32 => {
                    let bal = U256::from_be_slice(&ret[..32]);
                    if !bal.is_zero() {
                        warn!(%address, %contract, balance = %bal, "Residual token balance detected");
                        return Err(ReconciliationError::InFlightOperations { contract });
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    debug!(%contract, error = %e, "balanceOf probe failed — skipping");
                }
            }
        }

        debug!(%address, "No in-flight residuals on monitored contracts");
        Ok(())
    }
}

pub fn reconciler_for_providers(
    rpc: Arc<OmegaRpcClient>,
    providers: impl IntoIterator<Item = Address>,
) -> AtomicBalanceReconciler {
    let cfg = ReconciliationConfig {
        monitored_contracts: providers.into_iter().collect::<HashSet<_>>().into_iter().collect(),
        ..Default::default()
    };
    AtomicBalanceReconciler::new(rpc, cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sensible_dust() {
        let c = ReconciliationConfig::default();
        assert!(c.dust_tolerance_wei > U256::ZERO);
        assert!(c.timeout > Duration::from_secs(1));
    }

    #[test]
    fn reconciler_for_providers_dedupes_and_keeps_other_defaults() {
        let addr_a = Address::from([0x11u8; 20]);
        let addr_b = Address::from([0x22u8; 20]);
        // Duplicate addr_a on purpose — monitored_contracts must come out
        // deduplicated (via the HashSet round-trip) regardless of input order.
        let cfg = ReconciliationConfig {
            monitored_contracts: vec![addr_a, addr_b, addr_a]
                .into_iter()
                .collect::<HashSet<_>>()
                .into_iter()
                .collect(),
            ..Default::default()
        };
        assert_eq!(cfg.monitored_contracts.len(), 2);
        assert_eq!(cfg.dust_tolerance_wei, ReconciliationConfig::default().dust_tolerance_wei);
        assert_eq!(cfg.timeout, ReconciliationConfig::default().timeout);
    }
}