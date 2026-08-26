// omega-engine\crates\omega-simulation\src\submitter.rs
//! The only `BundleSubmitter` implementation this crate provides. It holds
//! a provider bound to a `ForkHandle` and nothing else — there is no
//! constructor path that accepts a relay URL, an auth token, or a signing
//! key ID. If the engine's wiring code ever tries to build this with
//! anything other than a `ForkHandle`, it won't compile.

use crate::error::{Result, SimError};
use crate::fork::ForkHandle;
use crate::traits::{Bundle, BundleSubmitter, Receipt};
use async_trait::async_trait;
use ethers::prelude::*;
use ethers::providers::Middleware;
use std::sync::Arc;

pub struct SimulationSubmitter {
    provider: Arc<Provider<Http>>,
    /// Dev-funded local signer, derived from Anvil's well-known test
    /// mnemonic. This key has never held, and will never hold, real value —
    /// it only exists inside this process's ephemeral fork.
    signer: LocalWallet,
}

impl SimulationSubmitter {
    /// Bind a submitter to a running fork. `signer_index` selects which of
    /// the fork's dev-funded accounts to execute from.
    pub async fn bound_to(fork: &ForkHandle, signer_index: u32) -> Result<Self> {
        const ANVIL_DEV_MNEMONIC: &str =
            "test test test test test test test test test test test junk";

        let provider = fork.provider();
        let chain_id = provider
            .get_chainid()
            .await
            .map_err(SimError::Provider)?
            .as_u64();

        let wallet = MnemonicBuilder::<coins_bip39::English>::default()
            .phrase(ANVIL_DEV_MNEMONIC)
            .index(signer_index)
            .map_err(|e| SimError::Other(anyhow::anyhow!(e)))?
            .build()
            .map_err(|e| SimError::Other(anyhow::anyhow!(e)))?
            .with_chain_id(chain_id);

        Ok(Self { provider, signer: wallet })
    }

    /// Explicit guard used by config validation and tests: any string that
    /// looks like a relay/auth destination is rejected before it ever
    /// reaches this submitter. Defense in depth on top of the type-level
    /// guarantee above (`bound_to` only accepts a `ForkHandle`).
    pub fn reject_if_live_looking(candidate: &str) -> Result<()> {
        const FORBIDDEN_MARKERS: &[&str] = &[
            "flashbots", "bloxroute", "titan", "eden", "relay.", "mev-share",
        ];
        let lower = candidate.to_lowercase();
        if FORBIDDEN_MARKERS.iter().any(|m| lower.contains(m)) {
            return Err(SimError::LiveTransportForbidden(candidate.to_string()));
        }
        Ok(())
    }

    pub fn signer_address(&self) -> Address {
        self.signer.address()
    }

    pub fn provider(&self) -> Arc<Provider<Http>> {
        self.provider.clone()
    }

    /// Check if the signer account has sufficient balance to cover the
    /// bundle's gas costs plus value transfer.
    pub async fn has_sufficient_balance(&self, bundle: &Bundle) -> Result<bool> {
        let balance = self
            .provider
            .get_balance(self.signer.address(), None)
            .await
            .map_err(SimError::Provider)?;

        let required = bundle.value + (bundle.gas_limit * bundle.max_fee_per_gas);
        Ok(balance >= required)
    }

    pub fn estimated_cost(&self, bundle: &Bundle) -> U256 {
        bundle.value + (bundle.gas_limit * bundle.max_fee_per_gas)
    }
}

#[async_trait]
impl BundleSubmitter for SimulationSubmitter {
    async fn submit(&self, bundle: Bundle) -> Result<Receipt> {
        let client = SignerMiddleware::new(self.provider.clone(), self.signer.clone());

        let tx = Eip1559TransactionRequest::new()
            .to(bundle.target_contract)
            .data(bundle.calldata.clone())
            .value(bundle.value)
            .gas(bundle.gas_limit)
            .max_fee_per_gas(bundle.max_fee_per_gas)
            .max_priority_fee_per_gas(bundle.max_priority_fee_per_gas);

        // FIX (this revision): `client` here is the `SignerMiddleware`
        // constructed just above, not `self.provider` directly — its
        // `get_balance` fails with `SignerMiddlewareError<Provider<Http>,
        // Wallet<...>>`, a distinct type from the plain `ProviderError`
        // that `SimError::Provider`'s `#[from]` converts. Passing
        // `SimError::Provider` as a bare function pointer to `map_err`
        // requires the closure's input type to match exactly, which it
        // doesn't here (E0631) — the two calls on `self.provider` itself
        // above (`get_chainid`, `has_sufficient_balance`'s `get_balance`)
        // are fine as-is since those genuinely run through the bare
        // `Provider<Http>` and do fail with `ProviderError`. Fixed by
        // routing these two `client.get_balance` calls through
        // `SimError::Contract(e.to_string())` instead — the same variant
        // this function already uses for `send_transaction`/`pending`
        // errors a few lines below, so this isn't a new error category,
        // just applying the one already in use here consistently to
        // every `SignerMiddleware`-sourced error in this function.
        let balance_before = client
            .get_balance(client.address(), None)
            .await
            .map_err(|e| SimError::Contract(e.to_string()))?;

        let pending = client
            .send_transaction(tx, None)
            .await
            .map_err(|e| SimError::Contract(e.to_string()))?;

        let receipt = pending
            .await
            .map_err(|e| SimError::Contract(e.to_string()))?
            .ok_or_else(|| SimError::Reverted("no receipt returned".into()))?;

        let balance_after = client
            .get_balance(client.address(), None)
            .await
            .map_err(|e| SimError::Contract(e.to_string()))?;

        let success = receipt.status.map(|s| s.as_u64() == 1).unwrap_or(false);

        // Same-width reinterpretation: subtracting as u128 with wrapping
        // semantics and reading the bit pattern back as i128 recovers the
        // correct signed delta even when balance_after < balance_before.
        // This is exact for any balances that fit in 128 bits — not
        // silently-ignored overflow, so don't "fix" it into a checked_sub.
        let realized_profit_wei = if success {
            Some(
                balance_after
                    .as_u128()
                    .wrapping_sub(balance_before.as_u128()) as i128,
            )
        } else {
            None
        };

        Ok(Receipt {
            opportunity_id: bundle.opportunity_id,
            tx_hash: format!("{:?}", receipt.transaction_hash),
            success,
            gas_used: receipt.gas_used.unwrap_or_default(),
            realized_profit_wei,
            revert_reason: if success {
                None
            } else {
                Some("transaction reverted on fork".to_string())
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fork::ForkHandle;
    use ethers::core::types::H160;
    use std::str::FromStr;

    #[tokio::test]
    #[ignore] // requires TEST_FORK_RPC_URL and network access
    async fn test_bound_to_creates_valid_submitter() {
        let fork = ForkHandle::new_test().await.unwrap();
        let submitter = SimulationSubmitter::bound_to(&fork, 0).await.unwrap();

        assert_ne!(submitter.signer_address(), Address::zero());
        // provider() returns Arc<Provider<Http>>, not an Option — confirm
        // it actually responds instead of asserting a type it can't have.
        assert!(submitter.provider().get_chainid().await.is_ok());
    }

    #[tokio::test]
    async fn test_reject_if_live_looking() {
        assert!(SimulationSubmitter::reject_if_live_looking("flashbots").is_err());
        assert!(SimulationSubmitter::reject_if_live_looking("https://relay.flashbots.net").is_err());
        assert!(SimulationSubmitter::reject_if_live_looking("bloxroute").is_err());
        assert!(SimulationSubmitter::reject_if_live_looking("titan").is_err());
        assert!(SimulationSubmitter::reject_if_live_looking("eden").is_err());
        assert!(SimulationSubmitter::reject_if_live_looking("mev-share").is_err());

        assert!(SimulationSubmitter::reject_if_live_looking("localhost:8545").is_ok());
        assert!(SimulationSubmitter::reject_if_live_looking("http://127.0.0.1:8545").is_ok());
        assert!(SimulationSubmitter::reject_if_live_looking("anvil").is_ok());
    }

    #[test]
    fn test_estimated_cost() {
        let bundle = Bundle {
            opportunity_id: "test".to_string(),
            target_contract: H160::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            calldata: Vec::<u8>::new().into(),
            value: U256::from(100),
            gas_limit: U256::from(21000),
            max_fee_per_gas: U256::from(50),
            max_priority_fee_per_gas: U256::from(2),
        };

        let provider = Arc::new(Provider::<Http>::try_from("http://localhost:8545").unwrap());
        let signer = LocalWallet::from_bytes(&[1u8; 32]).unwrap();
        let submitter = SimulationSubmitter { provider, signer };

        let cost = submitter.estimated_cost(&bundle);
        assert_eq!(cost, U256::from(1_050_100u64));
    }

    #[tokio::test]
    #[ignore] // requires TEST_FORK_RPC_URL and network access
    async fn test_has_sufficient_balance() {
        let fork = ForkHandle::new_test().await.unwrap();
        let submitter = SimulationSubmitter::bound_to(&fork, 0).await.unwrap();

        let bundle = Bundle {
            opportunity_id: "test".to_string(),
            target_contract: H160::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            calldata: Vec::<u8>::new().into(),
            value: U256::from(1_000_000_000_000_000_000u128),
            gas_limit: U256::from(21000),
            max_fee_per_gas: U256::from(100_000_000_000u128),
            max_priority_fee_per_gas: U256::from(10_000_000_000u128),
        };

        let result = submitter.has_sufficient_balance(&bundle).await.unwrap();
        assert!(result);
    }
}