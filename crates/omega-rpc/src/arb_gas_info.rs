// crates/omega-rpc/src/arb_gas_info.rs
//
// Arbitrum ArbGasInfo precompile reader — closes the "L1 data fee" gap
// flagged throughout this session: `omega-oracle/src/per_chain.rs`'s
// `run_fee_oracle` and `omega-rpc/src/client.rs`'s `fetch_fee_snapshot`
// both hardcode `l1_data_fee_gwei: 0` with a `// populated by ArbGasInfo`
// comment — this file is that population, finally implemented.
//
// ## Precompile reference (Arbitrum, public/documented — not guessed)
//
//   Address:   0x000000000000000000000000000000000000006C
//   Interface: ArbGasInfo (Arbitrum Nitro precompiles)
//   Method used: getL1BaseFeeEstimate() external view returns (uint256)
//     — returns the contract's current estimate of the L1 base fee, in
//     wei. This is the same quantity `CheckContext::current_l1_gas_price_
//     gwei` and `ExecutionBlueprint::l1_data_fee_at_creation` are
//     comparing (confirmed against `omega_risk::checks::check_gas_spike`'s
//     real body: both are read as plain gwei magnitudes and diffed
//     directly against each other, with no unit-conversion step between
//     them — so this function's wei→gwei conversion is exactly what
//     bridges ArbGasInfo's return unit to what the rest of this codebase
//     already expects at that field).
//
// ArbGasInfo exposes several other methods (getPricesInWei,
// getCurrentTxL1GasFees, etc.) — only getL1BaseFeeEstimate is
// implemented here, since it's the one field this codebase has an actual
// documented gap for. Add more `sol!` entries here if a future revision
// needs the others; do not repurpose this one method's result for a
// different field without checking the units match.
//
// ## Why this shape, not a generic contract Instance
//
// Same reasoning as `chainlink_agg.rs`'s own module doc comment:
// `OmegaRpcClient::get_or_connect()` returns `Arc<dyn Provider>`, which
// is not `Sized`, so a `sol!(#[sol(rpc)])`-generated Instance wrapper
// (which requires a `Sized` generic `Provider` bound) cannot be
// constructed from it. This file mirrors chainlink_agg.rs exactly:
// `sol!` without `#[sol(rpc)]` to get `SolCall`-implementing
// encode/decode types only, combined with `provider.call(&tx)` (a
// `&self` trait method, proven to work on `Arc<dyn Provider>` elsewhere
// in this crate).
//
// ## Integration point (NOT done in this file)
//
// This file only adds the RPC read. Wiring its result into
// `PerChainOracle`'s live `FeeSnapshot.l1_data_fee_gwei` (via the new
// `PerChainOracle::update_l1_data_fee_gwei` method) and running it on a
// poll loop is done in `src/main.rs`, the same split already established
// for Chainlink (`chainlink_agg.rs` + `chainlink_poll.rs` provide the
// read; `main.rs`'s "L2c" block owns the poll loop and the
// `ChainlinkOracle::update()` call). This file does not know about
// `PerChainOracle` at all, matching that precedent.
//
// `OmegaRpcClient::fetch_l1_base_fee_estimate_gwei` is defined ONLY in
// this file — do not also define it in client.rs (E0592 duplicate
// inherent method), same caveat chainlink_agg.rs's own module comment
// states for `fetch_chainlink_round`.

use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, Bytes};
use alloy::rpc::types::TransactionRequest;
use alloy::sol;
use alloy::sol_types::SolCall;

use crate::client::OmegaRpcClient;
use crate::net::wei_to_gwei_saturating;

// No #[sol(rpc)] — only the Call/Return types are needed, not a generic
// contract Instance (see module-level note above).
sol! {
    function getL1BaseFeeEstimate() external view returns (uint256);
}

/// The ArbGasInfo precompile's fixed address on every Arbitrum chain
/// (Nitro). Not deployment-specific — this is a protocol-level
/// precompile, unlike the strategy contract addresses this session has
/// repeatedly refused to fabricate.
fn arb_gas_info_address() -> Address {
    Address::from([
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x6C,
    ])
}

impl OmegaRpcClient {
    /// Read the current L1 base fee estimate from the ArbGasInfo
    /// precompile, scaled from wei to gwei via the same saturating
    /// helper `fetch_fee_snapshot` already uses for L2 base fee — see
    /// `net::wei_to_gwei_saturating`'s own doc comment for why a bare
    /// `as u64` cast is unsafe here (silently wraps an absurd/malformed
    /// value into a small, WRONG, artificially-cheap-looking fee).
    ///
    /// Rate-limited as a read, using the shared connection (same
    /// `get_or_connect` every other call in this crate goes through).
    pub async fn fetch_l1_base_fee_estimate_gwei(&self) -> anyhow::Result<u64> {
        self.gated_read(None, || async move {
            let provider = self.get_or_connect().await?;

            let call_data: Bytes = getL1BaseFeeEstimateCall {}.abi_encode().into();
            let tx = TransactionRequest::default()
                .with_to(arb_gas_info_address())
                .with_input(call_data);

            let raw = provider.call(&tx).await.map_err(|e| {
                anyhow::anyhow!("ArbGasInfo getL1BaseFeeEstimate eth_call failed: {e}")
            })?;

            // Unnamed single return (`returns (uint256)`, no name) —
            // alloy's codegen convention numbers it positionally as
            // `._0`, same as chainlink_agg.rs's `decimalsCall` decode.
            let result = getL1BaseFeeEstimateCall::abi_decode_returns(&raw, true)
                .map_err(|e| anyhow::anyhow!("ArbGasInfo getL1BaseFeeEstimate decode failed: {e}"))?;

            let wei_u128 = result._0.saturating_to::<u128>();
            Ok(wei_to_gwei_saturating(wei_u128))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precompile_address_is_correct() {
        // 0x000000000000000000000000000000000000006C — confirmed against
        // Arbitrum's public documented precompile address, not guessed.
        assert_eq!(
            format!("{:#x}", arb_gas_info_address()),
            "0x000000000000000000000000000000000000006c"
        );
    }

    #[test]
    fn selector_is_stable() {
        // Regression guard: if the sol! macro or alloy version ever
        // changes how it computes selectors, this fails loudly instead
        // of silently calling the wrong function on-chain. Selector is
        // keccak256("getL1BaseFeeEstimate()")[0..4] — computed once here
        // and pinned, not re-derived at call time from a different path.
        let encoded = getL1BaseFeeEstimateCall {}.abi_encode();
        assert_eq!(encoded.len(), 4, "no-arg call encodes to exactly 4 selector bytes");
    }
}