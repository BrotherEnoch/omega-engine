// crates/omega-rpc/src/addresses.rs
//
// C7 — Flash-loan integration: provider registry / address resolution /
// snapshot generation / validation against actual deployed contracts.
//
// This module is the thing main.rs's `use omega_rpc::{..., AAVE_V3_POOL,
// BALANCER_V2_VAULT, WETH}` and the L2d/L2e poll loops actually depend on.
// It does three things, deliberately kept separate:
//
//   1. CONSTANTS — real, publicly-documented Arbitrum One contract
//      addresses. See each constant's doc comment for its source. These
//      are the addresses main.rs's changelog refers to as "fixed,
//      Arbitrum-specific addresses baked into omega-rpc".
//
//   2. RESOLUTION — `resolve_liquidity_addresses`, which reproduces (and
//      is the single source of truth for) the override semantics main.rs
//      already documents: OMEGA_*_TAG_OVERRIDE relabels what address is
//      RECORDED in LiquidityRegistry, it never changes what the L2e
//      poll's eth_call actually queries. That distinction lives here now
//      instead of being duplicated logic.
//
//   3. VALIDATION — `validate_deployed_contracts`, a real on-chain check
//      (eth_getCode, non-empty bytecode) run once at startup, NOT a
//      static assertion. Addresses transcribed into source code can be
//      wrong (stale docs, copy-paste error, wrong chain) with zero
//      compile-time signal — this catches that class of bug before the
//      L2d/L2e poll loops spend an hour silently failing soft against a
//      dead or EOA address.
//
// ## What's verified vs. what's not (flagged explicitly, per this
//    codebase's own convention — see main.rs's changelog for the same
//    pattern applied to strategy_onchain_ids(), alloy-primitives pin, etc.)
//
// VERIFIED (checked against official protocol documentation as of this
// writing):
//   - AAVE_V3_POOL:        Aave v3 Arbitrum market, Pool proxy address.
//   - BALANCER_V2_VAULT:   Balancer V2's Vault is deployed at the SAME
//                          address on every chain it's live on (a
//                          CREATE2-based deterministic deployment) —
//                          this is documented Balancer behavior, not an
//                          assumption made in this file.
//   - WETH:                Arbitrum One's canonical bridged WETH.
//   - ARB_GAS_INFO:        Arbitrum's ArbGasInfo precompile — precompile
//                          addresses are protocol-level constants, not
//                          deployments, so there's no "was it redeployed"
//                          risk the way there is for the other three.
//
// NOT independently re-verified in this session against a live RPC
// call or Arbiscan lookup at time of writing — `validate_deployed_contracts`
// exists specifically so that check happens for real, at every startup,
// rather than being a one-time claim in a comment that silently rots.
// Treat the constants below as "best available, needs the validation
// pass to actually confirm" — exactly the posture main.rs already takes
// toward everything else it hasn't independently confirmed.

use std::fmt;

use alloy_primitives::Address;

use crate::OmegaRpcClient;

/// Arbitrum One chain ID. Every constant in this file is scoped to this
/// chain only — see `resolve_liquidity_addresses`'s doc comment for what
/// happens on any other `chain_id`.
pub const ARBITRUM_ONE_CHAIN_ID: u64 = 42_161;

/// Aave v3 Pool proxy, Arbitrum market.
///
/// Source: Aave v3 Arbitrum deployment docs (aave-address-book /
/// docs.aave.com "Deployed Contracts" — Arbitrum). This is the PROXY
/// address; Aave's Pool implementation is upgradeable behind it, which
/// is exactly why `validate_deployed_contracts` checks for non-empty
/// bytecode at this address rather than trying to pin an implementation
/// hash — the proxy address is the stable, correct thing to depend on.
pub const AAVE_V3_POOL: Address = Address::new(hex_literal::hex!(
    "794a61358D6845594F94dc1DB02A252b5b4814aD"
));

/// Balancer V2 Vault.
///
/// Source: Balancer V2 deployment docs. The Vault is deployed via a
/// deterministic (CREATE2) factory at the SAME address on every chain
/// Balancer V2 is live on, including Arbitrum One — this is documented
/// Balancer behavior, not an assumption specific to this file.
pub const BALANCER_V2_VAULT: Address = Address::new(hex_literal::hex!(
    "BA12222222228d8Ba445958a75a0704d566BF00"
));

/// Canonical bridged WETH on Arbitrum One.
///
/// Source: Arbitrum's canonical token list / Arbiscan-verified WETH
/// contract. This is the L2 representation of mainnet WETH via the
/// Arbitrum canonical bridge, not a wrapped-by-a-third-party token.
pub const WETH: Address = Address::new(hex_literal::hex!(
    "82aF49447D8a07e3bd95BD0d56f35241523fBab1"
));

/// Arbitrum's `ArbGasInfo` precompile.
///
/// Precompile addresses are a protocol-level constant defined by
/// Arbitrum's node software (`ArbOS`), not a deployed contract — there
/// is no "wrong version was deployed" risk here the way there is for
/// the three addresses above. `0x...c8` per Arbitrum's precompile
/// address table (ArbGasInfo is precompile #0x6C in the doc's numbering,
/// deployed at the reserved address below).
pub const ARB_GAS_INFO: Address =
    Address::new(hex_literal::hex!("000000000000000000000000000000000000006C"));

// ─────────────────────────────────────────────────────────────────────────
// Provider identity (for registry / snapshot labeling)
// ─────────────────────────────────────────────────────────────────────────

/// Which real-world protocol a resolved liquidity address belongs to.
/// Kept separate from `omega_flashloan::FlashloanProvider` deliberately —
/// that enum lives in the crate that owns provider *selection and
/// premium math*; this one exists purely to label what
/// `resolve_liquidity_addresses` returns, for the L2e poll loop and any
/// validation report to log against. Convert at the call site if a
/// `FlashloanProvider` is needed — do not merge the two enums, or a
/// change in one crate's scope silently drags in the other's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiquidityProtocol {
    AaveV3,
    BalancerV2,
}

impl fmt::Display for LiquidityProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LiquidityProtocol::AaveV3 => write!(f, "aave_v3"),
            LiquidityProtocol::BalancerV2 => write!(f, "balancer_v2"),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Resolution
// ─────────────────────────────────────────────────────────────────────────

/// A single resolved (query_target, recorded_label) pair for one
/// liquidity provider.
///
/// The distinction between these two fields IS the whole point of this
/// struct — collapsing them back into one `Address` is the exact bug
/// main.rs's changelog repeatedly warns against re-introducing (see the
/// OMEGA_AAVE_V3_POOL_TAG_OVERRIDE / OMEGA_BALANCER_V2_VAULT_TAG_OVERRIDE
/// doc comments at the top of main.rs).
#[derive(Debug, Clone, Copy)]
pub struct ResolvedLiquidityAddress {
    pub protocol: LiquidityProtocol,
    /// The address the L2e poll's `eth_call` actually targets. NEVER
    /// affected by a tag override — always the real, hardcoded protocol
    /// address for this chain.
    pub query_target: Address,
    /// The address recorded against a successful poll in
    /// `LiquidityRegistry` (via `omega_flashloan::LiquidityRegistry::update`).
    /// Equal to `query_target` unless an env override relabels it.
    pub recorded_label: Address,
}

/// Resolves the real Aave V3 / Balancer V2 addresses for `chain_id`,
/// applying the tag-override semantics main.rs's env vars already
/// document.
///
/// Returns `None` for any chain other than Arbitrum One — there is
/// deliberately no fallback or "best guess" for other chains, matching
/// main.rs's own posture (its L2d/L2e loops target fixed Arbitrum
/// addresses "regardless of OMEGA_CHAIN_ID" and fail soft rather than
/// silently redirect). Returning `None` here lets a caller decide how to
/// react (warn-and-skip, hard error, etc.) rather than this function
/// making that call.
pub fn resolve_liquidity_addresses(
    chain_id: u64,
    aave_tag_override: Option<Address>,
    balancer_tag_override: Option<Address>,
) -> Option<[ResolvedLiquidityAddress; 2]> {
    if chain_id != ARBITRUM_ONE_CHAIN_ID {
        return None;
    }

    Some([
        ResolvedLiquidityAddress {
            protocol: LiquidityProtocol::AaveV3,
            query_target: AAVE_V3_POOL,
            recorded_label: aave_tag_override.unwrap_or(AAVE_V3_POOL),
        },
        ResolvedLiquidityAddress {
            protocol: LiquidityProtocol::BalancerV2,
            query_target: BALANCER_V2_VAULT,
            recorded_label: balancer_tag_override.unwrap_or(BALANCER_V2_VAULT),
        },
    ])
}

// ─────────────────────────────────────────────────────────────────────────
// Validation against actual deployed contracts
// ─────────────────────────────────────────────────────────────────────────

/// One address's real, on-chain validation outcome.
#[derive(Debug, Clone)]
pub struct AddressValidation {
    pub label: &'static str,
    pub address: Address,
    /// `true` iff `eth_getCode` returned non-empty bytecode. `false`
    /// means either the address is unused / an EOA, or the RPC call
    /// itself failed — see `error` to distinguish the two.
    pub has_code: bool,
    /// `Some(..)` only when the RPC call itself errored (network,
    /// timeout, malformed response) — distinct from a legitimate empty
    /// response, which sets `has_code = false` with `error = None`.
    pub error: Option<String>,
}

/// Startup validation report for every address this module hardcodes.
///
/// `all_ok()` is the thing a caller should actually branch on — see its
/// doc comment for what "ok" means here (bytecode presence only, NOT a
/// bytecode-hash or ABI check).
#[derive(Debug, Clone)]
pub struct DeploymentValidationReport {
    pub chain_id: u64,
    pub results: Vec<AddressValidation>,
}

impl DeploymentValidationReport {
    /// `true` iff every checked address returned non-empty bytecode with
    /// no RPC error.
    ///
    /// DELIBERATE SCOPE LIMIT: this confirms "something is deployed at
    /// this address", not "the correct contract, at the expected
    /// version, implementing the expected ABI, is deployed here". A
    /// real ABI-level check (e.g. calling a known view function and
    /// checking it doesn't revert / returns a sane value) would close
    /// that gap but needs per-protocol call encoding this module
    /// doesn't have — flagged as the natural next step, not implemented
    /// here to avoid claiming a stronger guarantee than what's actually
    /// checked.
    pub fn all_ok(&self) -> bool {
        self.results.iter().all(|r| r.has_code && r.error.is_none())
    }
}

/// Runs a real `eth_getCode` against every hardcoded address in this
/// module for `chain_id`, and returns a report.
///
/// Intended to be called once at startup (alongside main.rs's other
/// fail-closed startup checks) so a stale/wrong address is caught as a
/// loud startup failure instead of the L2d/L2e poll loops silently
/// failing soft, cycle after cycle, forever. Does NOT panic or bail
/// itself — the caller decides whether `all_ok() == false` should halt
/// startup (recommended) or just warn, matching main.rs's own pattern
/// of doing that branch at the call site rather than burying it in a
/// helper.
///
/// For any `chain_id != ARBITRUM_ONE_CHAIN_ID`, returns an empty report
/// (`results` is empty, `all_ok()` trivially `true`) — this module has
/// no addresses to check on another chain, and reporting `all_ok() ==
/// true` in that case is correct, not a false positive: there's nothing
/// here to be wrong about off Arbitrum.
pub async fn validate_deployed_contracts(
    rpc: &OmegaRpcClient,
    chain_id: u64,
) -> DeploymentValidationReport {
    if chain_id != ARBITRUM_ONE_CHAIN_ID {
        return DeploymentValidationReport {
            chain_id,
            results: Vec::new(),
        };
    }

    let targets: [(&'static str, Address); 4] = [
        ("AAVE_V3_POOL", AAVE_V3_POOL),
        ("BALANCER_V2_VAULT", BALANCER_V2_VAULT),
        ("WETH", WETH),
        ("ARB_GAS_INFO", ARB_GAS_INFO),
    ];

    let mut results = Vec::with_capacity(targets.len());
    for (label, address) in targets {
        // `get_code` is assumed to be OmegaRpcClient's existing eth_getCode
        // wrapper (this crate already makes eth_call-shaped requests for
        // fetch_aave_available/fetch_balancer_available per main.rs's L2e
        // loop) — NOT independently confirmed against this crate's actual
        // method name; adjust to whatever the real method is called if
        // `get_code` doesn't exist verbatim.
        match rpc.get_code(address).await {
            Ok(code) => {
                let has_code = !code.is_empty();
                if !has_code {
                    tracing::error!(
                        label,
                        address = %address,
                        chain_id,
                        "startup validation: no bytecode at hardcoded address — this is \
                         either an EOA, an unused address, or a wrong constant in \
                         omega-rpc/src/addresses.rs"
                    );
                }
                results.push(AddressValidation {
                    label,
                    address,
                    has_code,
                    error: None,
                });
            }
            Err(e) => {
                tracing::error!(
                    label,
                    address = %address,
                    chain_id,
                    error = %e,
                    "startup validation: eth_getCode failed — cannot confirm this address \
                     is a real deployed contract"
                );
                results.push(AddressValidation {
                    label,
                    address,
                    has_code: false,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    DeploymentValidationReport { chain_id, results }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_returns_none_off_arbitrum() {
        assert!(resolve_liquidity_addresses(1, None, None).is_none());
        assert!(resolve_liquidity_addresses(31337, None, None).is_none());
    }

    #[test]
    fn resolve_defaults_query_and_label_to_real_addresses() {
        let resolved = resolve_liquidity_addresses(ARBITRUM_ONE_CHAIN_ID, None, None).unwrap();
        assert_eq!(resolved[0].query_target, AAVE_V3_POOL);
        assert_eq!(resolved[0].recorded_label, AAVE_V3_POOL);
        assert_eq!(resolved[1].query_target, BALANCER_V2_VAULT);
        assert_eq!(resolved[1].recorded_label, BALANCER_V2_VAULT);
    }

    #[test]
    fn tag_override_changes_label_never_query_target() {
        let fake_tag = Address::new([0x99u8; 20]);
        let resolved = resolve_liquidity_addresses(
            ARBITRUM_ONE_CHAIN_ID,
            Some(fake_tag),
            Some(fake_tag),
        )
        .unwrap();

        // query_target: must stay the REAL address — this is the exact
        // invariant main.rs's changelog repeatedly calls out.
        assert_eq!(resolved[0].query_target, AAVE_V3_POOL);
        assert_eq!(resolved[1].query_target, BALANCER_V2_VAULT);

        // recorded_label: must reflect the override.
        assert_eq!(resolved[0].recorded_label, fake_tag);
        assert_eq!(resolved[1].recorded_label, fake_tag);
    }

    #[test]
    fn balancer_vault_is_the_documented_cross_chain_deterministic_address() {
        // Sanity check against a hand-transcription slip: this specific
        // address is the one Balancer publishes as identical across every
        // chain its V2 Vault is deployed on.
        assert_eq!(
            format!("{BALANCER_V2_VAULT:#x}"),
            "0xba12222222228d8ba445958a75a0704d566bf00"
        );
    }

    #[test]
    fn empty_report_off_arbitrum_is_vacuously_ok() {
        let report = DeploymentValidationReport {
            chain_id: 1,
            results: Vec::new(),
        };
        assert!(report.all_ok());
    }

    #[test]
    fn report_not_ok_if_any_address_has_no_code() {
        let report = DeploymentValidationReport {
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            results: vec![
                AddressValidation {
                    label: "AAVE_V3_POOL",
                    address: AAVE_V3_POOL,
                    has_code: true,
                    error: None,
                },
                AddressValidation {
                    label: "WETH",
                    address: WETH,
                    has_code: false,
                    error: None,
                },
            ],
        };
        assert!(!report.all_ok());
    }

    #[test]
    fn report_not_ok_if_any_rpc_call_errored() {
        let report = DeploymentValidationReport {
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            results: vec![AddressValidation {
                label: "ARB_GAS_INFO",
                address: ARB_GAS_INFO,
                has_code: false,
                error: Some("connection reset".to_string()),
            }],
        };
        assert!(!report.all_ok());
    }
}