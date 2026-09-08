// crates/omega-rpc/src/aave_positions.rs
//
// Aave V3 Pool.getUserAccountData eth_call — feeds PositionRegistry for LA.
//
// Aggregate USD (base-currency) collateral/debt and health factor only.
// Per-reserve debt/collateral token identity is supplied by the operator
// (watchlist config), not by this call.

use alloy::rpc::types::{TransactionInput, TransactionRequest};
use alloy::sol_types::SolCall;
use alloy_primitives::{Address, TxKind, U256};

use crate::client::OmegaRpcClient;
use crate::flashloan_liq::AAVE_V3_POOL;

alloy::sol! {
    function getUserAccountData(address user) external view returns (
        uint256 totalCollateralBase,
        uint256 totalDebtBase,
        uint256 availableBorrowsBase,
        uint256 currentLiquidationThreshold,
        uint256 ltv,
        uint256 healthFactor
    );
}

/// Decoded Aave V3 account data for one borrower.
#[derive(Debug, Clone, Copy)]
pub struct AaveUserAccountData {
    pub total_collateral_base: U256,
    pub total_debt_base: U256,
    pub health_factor: U256,
}

impl OmegaRpcClient {
    /// Live Aave V3 `getUserAccountData` for `user`.
    ///
    /// Collateral/debt are in Aave base currency (USD, typically 8 decimals).
    /// Health factor is 1e18-scaled (liquidatable when strictly < 1e18).
    pub async fn fetch_aave_user_account_data(
        &self,
        user: Address,
    ) -> anyhow::Result<AaveUserAccountData> {
        self.gated_read(None, || async move {
            let provider = self.get_or_connect().await?;
            let call = getUserAccountDataCall { user };
            let tx = TransactionRequest {
                to: Some(TxKind::Call(AAVE_V3_POOL)),
                input: TransactionInput::new(call.abi_encode().into()),
                ..Default::default()
            };
            let raw = provider.call(&tx).await.map_err(|e| {
                anyhow::anyhow!("Aave getUserAccountData({user}) eth_call failed: {e}")
            })?;
            let decoded = getUserAccountDataCall::abi_decode_returns(&raw, true)
                .map_err(|e| anyhow::anyhow!("decoding getUserAccountData failed: {e}"))?;
            Ok(AaveUserAccountData {
                total_collateral_base: decoded.totalCollateralBase,
                total_debt_base: decoded.totalDebtBase,
                health_factor: decoded.healthFactor,
            })
        })
        .await
    }
}
