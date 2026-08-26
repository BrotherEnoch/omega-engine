// crates/omega-execution/src/flashloan_provider_table.rs
//
// Gap 7 — real Arbitrum address -> protocol mapping for
// `resolve_flashloan_provider_id()`. See ProductionIntegrationPlan.md
// Gap 7 for why this table exists and why it must fail closed for any
// address not present in it.
//
// ## Verification status of each entry (as of this revision)
//
// Every entry below has:
//   1. An authoritative source named in its comment (official docs,
//      protocol deployment manifest, or block-explorer label that matches
//      the protocol's own documentation).
//   2. Non-empty bytecode confirmed via eth_getCode against
//      https://arb1.arbitrum.io/rpc (chain id 42161 / 0xa4b1) on
//      2026-08-25. Prefix and approximate size recorded at verification time.
//
// Entries deliberately still omitted: Balancer Vault (mainnet address
// 0xBA1222… has empty code on Arbitrum One; no verified Arbitrum deployment
// was confirmed this session).
//
// ## Before adding any further address to this table
//
// 1. Confirm it against an authoritative source: the protocol's own
//    deployment docs/registry, or a GitHub deployments manifest with
//    commit history you can inspect — not a random aggregator.
// 2. Run `cast code <address> --rpc-url <arbitrum_rpc>` (or equivalent
//    eth_getCode) and confirm non-empty bytecode is actually live at that
//    address on Arbitrum One (chain id 42161) specifically — NOT Ethereum
//    mainnet, NOT another L2. Reusing a mainnet address for the same-named
//    protocol is the exact failure mode Gap 7's acceptance criteria calls
//    out: it would silently defeat the no-self-flash check rather than
//    fail loudly.
// 3. Only then add the entry below, with a comment naming your source,
//    and deliberately update the table-length assertion in tests.

use std::collections::HashMap;
use alloy_primitives::{address, Address};

/// Real, verified Arbitrum One flashloan-provider addresses.
/// See module doc for the verification bar applied to every entry.
pub fn arbitrum_flashloan_provider_table() -> HashMap<Address, &'static str> {
    let mut m = HashMap::new();

    // Aave V3 Pool — Arbiscan label "Aave: Pool V3"; aave-dao/aave-address-book
    // AaveV3Arbitrum.sol POOL constant. eth_getCode 2026-08-25: ~2400 bytes.
    m.insert(
        address!("794a61358D6845594F94dc1DB02A252b5b4814aD"),
        "aave_v3",
    );

    // Compound V3 native USDC Comet — Compound forum deployment post
    // (cUSDCv3: 0x9c4ec768…); Arbiscan token tracker "Compound USDC (cUSDCv3)".
    // eth_getCode 2026-08-25: ~1878 bytes.
    m.insert(
        address!("9c4ec768c28520B50860ea7a15bd7213a9fF58bf"),
        "compound_v3",
    );

    // Compound V3 USDC.e Comet — Dune Compound V3 markets list + Compound
    // forum/wrapper deployment notes (cUSDCev3 on Arbitrum).
    // eth_getCode 2026-08-25: ~1878 bytes.
    m.insert(
        address!("a5EDBDD9646f8dFF606d7448e414884C7d905dCA"),
        "compound_v3",
    );

    // Compound V3 WETH Comet — Dune Compound V3 markets list (cWETHv3 Arbitrum).
    // eth_getCode 2026-08-25: ~1878 bytes.
    m.insert(
        address!("6f7d514bbd4aff3bcd1140b7344b32f063dee486"),
        "compound_v3",
    );

    // Compound V3 USDT Comet — Dune Compound V3 markets list (cUSDTv3 Arbitrum).
    // eth_getCode 2026-08-25: ~1878 bytes.
    m.insert(
        address!("d98be00b5d27fc98112bde293e487f8d4ca57d07"),
        "compound_v3",
    );

    // Morpho Blue singleton — official Morpho docs
    // (docs.morpho.org addresses, Arbitrum row) + Arbiscan label
    // "Morpho: Morpho". eth_getCode 2026-08-25: ~15582 bytes.
    // Note: mainnet Morpho Blue (0xBBBBBb…) has empty code on Arbitrum; do not use it.
    m.insert(
        address!("6c247b1F6182318877311737BaC0844bAa518F5e"),
        "morpho_blue",
    );

    // Euler V2 Ethereum Vault Connector (EVC) — Arbiscan label
    // "Euler: Ethereum Vault Connector"; verified source name
    // EthereumVaultConnector. eth_getCode 2026-08-25: ~22050 bytes.
    m.insert(
        address!("6302ef0F34100CDDFb5489fbcB6eE1AA95CD1066"),
        "euler_v2",
    );

    m
}

/// Resolves a flashloan-provider contract address to the protocol
/// identifier used elsewhere in this workspace (matching
/// `config/arbitrum.toml`'s `[la].protocols` string values). Fails
/// closed — returns `None`, never a guessed or partial match — for any
/// address not in the verified table above, INCLUDING the zero address.
pub fn resolve_flashloan_provider_id(addr: Address) -> Option<&'static str> {
    if addr == Address::ZERO {
        return None;
    }
    arbitrum_flashloan_provider_table().get(&addr).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_address_never_resolves() {
        assert_eq!(resolve_flashloan_provider_id(Address::ZERO), None);
    }

    #[test]
    fn known_aave_v3_pool_resolves() {
        let addr = address!("794a61358D6845594F94dc1DB02A252b5b4814aD");
        assert_eq!(resolve_flashloan_provider_id(addr), Some("aave_v3"));
    }

    #[test]
    fn known_compound_usdc_comet_resolves() {
        let addr = address!("9c4ec768c28520B50860ea7a15bd7213a9fF58bf");
        assert_eq!(resolve_flashloan_provider_id(addr), Some("compound_v3"));
    }

    #[test]
    fn known_compound_usdce_comet_resolves() {
        let addr = address!("a5EDBDD9646f8dFF606d7448e414884C7d905dCA");
        assert_eq!(resolve_flashloan_provider_id(addr), Some("compound_v3"));
    }

    #[test]
    fn known_compound_weth_comet_resolves() {
        let addr = address!("6f7d514bbd4aff3bcd1140b7344b32f063dee486");
        assert_eq!(resolve_flashloan_provider_id(addr), Some("compound_v3"));
    }

    #[test]
    fn known_compound_usdt_comet_resolves() {
        let addr = address!("d98be00b5d27fc98112bde293e487f8d4ca57d07");
        assert_eq!(resolve_flashloan_provider_id(addr), Some("compound_v3"));
    }

    #[test]
    fn known_morpho_blue_resolves() {
        let addr = address!("6c247b1F6182318877311737BaC0844bAa518F5e");
        assert_eq!(resolve_flashloan_provider_id(addr), Some("morpho_blue"));
    }

    #[test]
    fn known_euler_v2_evc_resolves() {
        let addr = address!("6302ef0F34100CDDFb5489fbcB6eE1AA95CD1066");
        assert_eq!(resolve_flashloan_provider_id(addr), Some("euler_v2"));
    }

    #[test]
    fn unverified_address_fails_closed() {
        // Arbitrary address NOT in the table must resolve to None.
        let addr = address!("0000000000000000000000000000000000dEaD");
        assert_eq!(resolve_flashloan_provider_id(addr), None);
    }

    #[test]
    fn mainnet_morpho_blue_does_not_resolve_on_arbitrum() {
        // Guard against the exact Gap 7 failure mode: mainnet Morpho Blue
        // address must not resolve on Arbitrum (empty code / wrong chain).
        let addr = address!("BBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb");
        assert_eq!(resolve_flashloan_provider_id(addr), None);
    }

    #[test]
    fn table_has_no_duplicate_protocol_confusion() {
        // Sanity check: every address maps to exactly one protocol string.
        // Table currently has exactly the seven verified entries — update
        // this count deliberately when adding a new address, and confirm
        // its sourcing per this file's module doc before doing so.
        let table = arbitrum_flashloan_provider_table();
        assert_eq!(
            table.len(),
            7,
            "table should have exactly the verified entries — update this \
             count deliberately when adding a new address, and confirm its \
             sourcing per this file's module doc before doing so"
        );
    }
}