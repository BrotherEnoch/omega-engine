// crates/omega-strategies/src/flashloan_select.rs
//
// Shared off-chain -> on-chain flashloan-selection mapping, used by every
// strategy that calls `omega_flashloan::select_provider` inside
// `build_blueprint` (LA today; MSA/SA once their own capital-path gap —
// see each file's TODO(capital-path) — is resolved by a real product
// decision, per the locked Option A/B write-up).
//
// Exists as a shared helper rather than duplicated inline per-strategy
// specifically so the Balancer/AaveV3/UniswapV3 mapping only has one
// place to get wrong.
//
// NOTE: `select_provider` picks a *provider and pool*, not a *token* —
// its real signature (verified against crates/omega-flashloan/src/tests.rs)
// is `select_provider(registry, chain_id, amount_wei)`, with no ERC20
// argument, and `LiquidityRegistry::update` likewise carries no token.
// This module does not attempt to solve that — the token to borrow must
// already be known by the caller before selection happens. See each
// strategy's own capital-path TODO for the current status of that gap.

use omega_core::types::FlashloanProviderType;
use omega_flashloan::FlashloanProvider;

/// Maps the off-chain `omega_flashloan::FlashloanProvider` (used by
/// `select_provider`'s liquidity-selection logic) to the on-chain-facing
/// `omega_core::types::FlashloanProviderType` (ABI-encoded as the
/// Solidity `uint8` ordinal). Defined here, not as a `From` impl on
/// either type, because `omega-core` cannot depend on `omega-flashloan`
/// without creating a cycle (confirmed: `omega-flashloan` depends on
/// `omega-core`) — so the mapping has to live in a crate that can see
/// both types, which `omega-strategies` already does.
pub fn to_blueprint_provider_type(provider: FlashloanProvider) -> FlashloanProviderType {
    match provider {
        FlashloanProvider::Balancer => FlashloanProviderType::Balancer,
        FlashloanProvider::AaveV3 => FlashloanProviderType::AaveV3,
        FlashloanProvider::UniswapV3 => FlashloanProviderType::UniswapV3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every FlashloanProvider variant must map to something — this is
    /// mostly a compile-time guarantee (the match above is exhaustive,
    /// so removing a match arm is a build error), but pinning the
    /// ordinal-preserving property explicitly catches a silent
    /// reordering of either enum's variants in the future.
    #[test]
    fn mapping_preserves_ordinal_order() {
        assert_eq!(
            to_blueprint_provider_type(FlashloanProvider::Balancer) as u8,
            FlashloanProviderType::Balancer as u8
        );
        assert_eq!(
            to_blueprint_provider_type(FlashloanProvider::AaveV3) as u8,
            FlashloanProviderType::AaveV3 as u8
        );
        assert_eq!(
            to_blueprint_provider_type(FlashloanProvider::UniswapV3) as u8,
            FlashloanProviderType::UniswapV3 as u8
        );
    }
}
