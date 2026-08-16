// crates/omega-rpc/src/flashloan_liq.rs
//
// Live flashloan liquidity reads via sol! + Provider::call — same shape as
// chainlink_agg.rs (no generic contract Instance; Arc<dyn Provider> is not
// Sized for Instance::new).
//
// ## Purpose (C4-A)
//
// Populate omega_risk::context::FlashloanSnapshot.available for check 10
// (MissLiquidity). Semantics are balance reads, not event-log accumulators:
//   - Aave V3: Protocol Data Provider → aToken → underlying balanceOf(aToken)
//     is wrong; available-to-borrow for flashloan is the underlying held by
//     the aToken / pool reserve. We read balanceOf(underlying) on the aToken
//     address (aTokens hold the underlying), which is the standard "how much
//     can be flash-loaned" proxy used by searchers.
//   - Balancer V2: ERC-20 balanceOf(token) on the Vault — the Vault is the
//     single custody contract for all pool liquidity.
//
// ## Addresses (Arbitrum, search-verified 2026-08)
//
//   AAVE_V3_POOL                 0x794a61358D6845594F94dc1DB02A252b5b4814aD
//   AAVE_PROTOCOL_DATA_PROVIDER  0x69FA688f1Dc47d4B5d8029D5a35FB7a548310654
//     (canonical historical provider; address-book may list 0x243Aa95c… —
//      both implement getReserveTokensAddresses; pin deliberately)
//   BALANCER_V2_VAULT            0xBA12222222228d8Ba445958a75a0704d566BF2C8
//   WETH                         0x82aF49447D8a07e3bd95BD0d56f35241523fBab1
//   USDC_NATIVE                  0xaf88d065e77c8cC2239327C5EDb3A432268e5831
//
// Uniswap V3 deliberately omitted — per-pair pools, no registry in-repo.
//
// ## Fail-closed
//
// Any eth_call / decode failure returns Err. Callers must not invent a
// plausible available figure; CheckContext should treat a failed read as
// available = 0 (or skip the provider) so check 10 rejects rather than
// silently under-estimating risk.

use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, Bytes, U256, address};
use alloy::rpc::types::TransactionRequest;
use alloy::sol;
use alloy::sol_types::SolCall;

use crate::client::OmegaRpcClient;

// No #[sol(rpc)] — only Call/Return types; see chainlink_agg.rs module note.
sol! {
    function balanceOf(address account) external view returns (uint256);

    function getReserveTokensAddresses(address asset)
        external
        view
        returns (
            address aTokenAddress,
            address stableDebtTokenAddress,
            address variableDebtTokenAddress
        );
}

// ── Canonical Arbitrum addresses ─────────────────────────────────────────────

/// Aave V3 Pool (Arbitrum). Not required for the balance path below, but
/// kept as the protocol entry-point constant for callers / future use.
pub const AAVE_V3_POOL: Address = address!("794a61358D6845594F94dc1DB02A252b5b4814aD");

/// Aave V3 Protocol Data Provider (Arbitrum) — exposes
/// getReserveTokensAddresses(asset) → (aToken, stableDebt, variableDebt).
pub const AAVE_PROTOCOL_DATA_PROVIDER: Address =
    address!("69FA688f1Dc47d4B5d8029D5a35FB7a548310654");

/// Balancer V2 Vault — same address on Ethereum and Arbitrum.
pub const BALANCER_V2_VAULT: Address = address!("BA12222222228d8Ba445958a75a0704d566BF2C8");

/// Canonical WETH on Arbitrum.
pub const WETH: Address = address!("82aF49447D8a07e3bd95BD0d56f35241523fBab1");

/// Native (Circle) USDC on Arbitrum — not USDC.e.
pub const USDC_NATIVE: Address = address!("af88d065e77c8cC2239327C5EDb3A432268e5831");

// ── Helpers ──────────────────────────────────────────────────────────────────

fn u256_to_u128(v: U256) -> anyhow::Result<u128> {
    v.try_into()
        .map_err(|_| anyhow::anyhow!("U256 balance does not fit u128: {v}"))
}

// ── OmegaRpcClient methods ───────────────────────────────────────────────────

impl OmegaRpcClient {
    /// Rate-limited ERC-20 `balanceOf(holder)` via raw eth_call.
    pub async fn fetch_erc20_balance(
        &self,
        token: Address,
        holder: Address,
    ) -> anyhow::Result<u128> {
        self.gated_read(None, || async move {
            let provider = self.get_or_connect().await?;

            let data: Bytes = balanceOfCall { account: holder }.abi_encode().into();
            let tx = TransactionRequest::default()
                .with_to(token)
                .with_input(data);

            let raw = provider
                .call(&tx)
                .await
                .map_err(|e| anyhow::anyhow!("balanceOf eth_call failed: {e}"))?;

            let decoded = balanceOfCall::abi_decode_returns(&raw, true)
                .map_err(|e| anyhow::anyhow!("balanceOf decode failed: {e}"))?;

            // Unnamed single return → positional field `_0` (same as
            // decimalsCall in chainlink_agg.rs).
            u256_to_u128(decoded._0)
        })
        .await
    }

    /// Aave V3 available flashloan liquidity for `asset` (raw token units).
    ///
    /// 1. Data Provider `getReserveTokensAddresses(asset)` → aToken
    /// 2. `balanceOf(asset)` on the aToken address — aToken holds the
    ///    underlying reserve that flashloans draw from.
    ///
    /// Fail-closed on any call/decode error. Does not fabricate zero on
    /// success with an empty reserve — a real zero balance is returned as
    /// `Ok(0)` and is a valid input for check 10.
    pub async fn fetch_aave_available(&self, asset: Address) -> anyhow::Result<u128> {
        self.gated_read(None, || async move {
            let provider = self.get_or_connect().await?;

            // ── getReserveTokensAddresses(asset) ─────────────────────────
            let addr_data: Bytes = getReserveTokensAddressesCall { asset }.abi_encode().into();
            let addr_tx = TransactionRequest::default()
                .with_to(AAVE_PROTOCOL_DATA_PROVIDER)
                .with_input(addr_data);

            let addr_raw = provider
                .call(&addr_tx)
                .await
                .map_err(|e| anyhow::anyhow!("getReserveTokensAddresses eth_call failed: {e}"))?;

            let addrs = getReserveTokensAddressesCall::abi_decode_returns(&addr_raw, true)
                .map_err(|e| anyhow::anyhow!("getReserveTokensAddresses decode failed: {e}"))?;

            let a_token = addrs.aTokenAddress;
            if a_token == Address::ZERO {
                anyhow::bail!("Aave aToken for asset {asset} is zero address");
            }

            // ── balanceOf(asset) on aToken ───────────────────────────────
            // Nested gated_read would double-charge the rate limiter; do the
            // second eth_call on the same provider handle inside this closure.
            let bal_data: Bytes = balanceOfCall { account: a_token }.abi_encode().into();
            let bal_tx = TransactionRequest::default()
                .with_to(asset)
                .with_input(bal_data);

            let bal_raw = provider
                .call(&bal_tx)
                .await
                .map_err(|e| anyhow::anyhow!("aToken underlying balanceOf eth_call failed: {e}"))?;

            let bal = balanceOfCall::abi_decode_returns(&bal_raw, true)
                .map_err(|e| anyhow::anyhow!("aToken underlying balanceOf decode failed: {e}"))?;

            u256_to_u128(bal._0)
        })
        .await
    }

    /// Balancer V2 available liquidity for `token` (raw token units).
    ///
    /// Reads `balanceOf(token)` on the Vault — all pool balances live there.
    pub async fn fetch_balancer_available(&self, token: Address) -> anyhow::Result<u128> {
        self.fetch_erc20_balance(token, BALANCER_V2_VAULT).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_nonzero() {
        assert_ne!(AAVE_V3_POOL, Address::ZERO);
        assert_ne!(AAVE_PROTOCOL_DATA_PROVIDER, Address::ZERO);
        assert_ne!(BALANCER_V2_VAULT, Address::ZERO);
        assert_ne!(WETH, Address::ZERO);
        assert_ne!(USDC_NATIVE, Address::ZERO);
    }

    #[test]
    fn u256_to_u128_accepts_small() {
        assert_eq!(u256_to_u128(U256::from(42u64)).unwrap(), 42u128);
    }

    #[test]
    fn u256_to_u128_rejects_overflow() {
        // U256::MAX does not fit in u128
        assert!(u256_to_u128(U256::MAX).is_err());
    }
}