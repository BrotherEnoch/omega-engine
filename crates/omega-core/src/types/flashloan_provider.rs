// crates/omega-core/src/types/flashloan_provider.rs
//
// On-chain-facing flashloan provider discriminant.
//
// Mirrors `OmegaOrchestrator.sol`'s `FlashloanProviderType` enum ordinal
// order (Balancer=0, AaveV3=1, UniswapV3=2) — confirmed directly against
// the real contract source:
//
//     enum FlashloanProviderType { Balancer, AaveV3, UniswapV3 }
//
// Deliberately a blueprint-local type rather than a re-export of
// `omega_flashloan::FlashloanProvider`: `omega-flashloan` depends on
// `omega-core` (confirmed via `cargo tree -p omega-flashloan`), so the
// reverse dependency (`omega-core` -> `omega-flashloan`) would create a
// cycle. Whatever calls `omega_flashloan::select_provider` inside
// `build_blueprint` (a crate that CAN see both types) is responsible for
// mapping `omega_flashloan::FlashloanProvider` into this type — that
// mapping is not implemented here, since `omega-core` cannot reference
// `omega_flashloan::FlashloanProvider` to provide a `From` impl without
// the same cycle.
//
// IMPORTANT: the discriminant values below are load-bearing. They are
// ABI-encoded directly as the Solidity `uint8` enum ordinal when the
// blueprint encoder is built. Do not reorder these variants — reordering
// silently changes which flashloan provider an already-signed blueprint
// calls into on-chain, with no compiler error to catch it.

use serde::{Deserialize, Serialize};

/// Flashloan provider selected for a given `ExecutionBlueprint`, chosen
/// off-chain by `omega_flashloan::select_provider` at blueprint-build
/// time. Ordinal values must match `OmegaOrchestrator.sol`'s
/// `FlashloanProviderType` enum exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum FlashloanProviderType {
    /// Balancer V2 Vault. 0 bps premium — preferred whenever it has
    /// sufficient liquidity (see `omega_flashloan`'s selection order).
    /// On-chain, the Orchestrator calls its own admin-configured
    /// `flashloanProvider` address for this variant — `provider_contract`
    /// on the blueprint is not read for Balancer.
    Balancer = 0,
    /// Aave v3 Pool. 9 bps premium — first fallback. Same as Balancer:
    /// the Orchestrator uses its own admin-configured `aavePool` address;
    /// `provider_contract` is not read for AaveV3 either.
    AaveV3 = 1,
    /// Uniswap V3 pool. 30 bps premium — last resort. Unlike the other
    /// two, there is no single canonical contract for this provider type
    /// — a different pool exists per token pair and fee tier. The
    /// specific pool to flash against MUST be carried on
    /// `provider_contract`; the Orchestrator reads it directly and reads
    /// the pool's own `token0()`/`token1()` on-chain to determine
    /// argument placement, rather than trusting an off-chain-supplied
    /// flag.
    UniswapV3 = 2,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the ordinal values against the Solidity enum. This test
    /// passing proves this Rust enum's own discriminants haven't
    /// silently changed — it does NOT prove the Solidity side hasn't
    /// drifted independently. Cross-check against
    /// `OmegaOrchestrator.sol`'s actual `enum FlashloanProviderType`
    /// declaration whenever either file changes; there is no automated
    /// link between the two today.
    #[test]
    fn ordinals_match_solidity_enum_order() {
        assert_eq!(FlashloanProviderType::Balancer as u8, 0);
        assert_eq!(FlashloanProviderType::AaveV3 as u8, 1);
        assert_eq!(FlashloanProviderType::UniswapV3 as u8, 2);
    }

    /// Round-trips cleanly through JSON, matching the pattern already
    /// established by `Lane`/`Simulator` in this crate (snake_case
    /// serde rename). Not load-bearing for the on-chain encoding path
    /// (which uses the raw `u8` discriminant, not serde), but this type
    /// is also likely to cross process boundaries off-chain (logging,
    /// RPC, dashboards) where the JSON shape matters.
    #[test]
    fn serde_round_trip() {
        for variant in [
            FlashloanProviderType::Balancer,
            FlashloanProviderType::AaveV3,
            FlashloanProviderType::UniswapV3,
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            let back: FlashloanProviderType = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, back);
        }
    }
}
