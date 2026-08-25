// contracts/test/DomainSeparatedBlueprintHash.t.sol
// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import "forge-std/Test.sol";

/// Golden-vector oracle for `KeyManagerTransactionSigner::compute_bp_hash`
/// (crates/omega-execution/src/signer.rs). Mirrors the domain-separated
/// hash `OmegaOrchestrator.sol` itself signs/replay-tracks against:
///
///   keccak256(abi.encode(address(this), EXPECTED_CHAIN_ID, blueprintCalldata))
///
/// i.e. THREE top-level parameters (address, uint64, bytes) encoded as a
/// flat tuple — NOT a struct-typed single value. See signer.rs's top doc
/// comment, "BUG FOUND AND FIXED (2026-08-24)", for exactly why that
/// distinction matters: `Blueprint::abi_encode()` (the struct-as-single-
/// dynamic-value form) silently produced the wrong bytes for the sibling
/// `blueprintCalldata` encoding, and `compute_bp_hash`'s
/// `DomainSeparatedBlueprint::abi_encode_params()` fix was originally
/// made BY ANALOGY to that bug. This test file closes that gap: a real
/// `forge test` run against solc 0.8.24 confirmed the fix produces the
/// exact value both this file and signer.rs's
/// `compute_bp_hash_matches_solc_golden_vector` expect.
///
/// Standalone by design (does not call into `OmegaOrchestrator.sol`
/// itself), for the same reason `BlueprintCalldataAbi.t.sol` is
/// standalone: it isolates "does solc's `abi.encode` of these exact
/// types, in this exact order, match the Rust side" from any other
/// contract behavior, so a failure here can only mean an ABI mismatch,
/// never some unrelated contract bug.
contract DomainSeparatedBlueprintHashTest is Test {
    // Fixture MUST match
    // omega-execution/src/signer.rs::tests::compute_bp_hash_matches_solc_golden_vector.
    // orchestrator = alloy_primitives::Address::from([0x01; 20]) on the Rust side.
    address constant ORCHESTRATOR =
        address(uint160(0x0101010101010101010101010101010101010101));

    // chain_id = 42161 (Arbitrum One) on the Rust side — matches every
    // other golden-vector / regression fixture in this workspace that
    // pins chain_id, so a reader cross-checking against
    // BlueprintCalldataAbi.t.sol or signer.rs's other tests sees the
    // same number everywhere.
    uint64 constant CHAIN_ID = 42161;

    // blueprint_calldata = the EXACT golden bytes emitted by
    // BlueprintCalldataAbi.t.sol's own
    // test_print_golden_blueprint_calldata, copied verbatim — NOT a
    // fresh/unrelated bytes fixture. Using the real blueprintCalldata
    // golden vector here (rather than an arbitrary placeholder bytes
    // value) means this test's own bytes input is itself already
    // solc-verified, and it keeps the two golden vectors mutually
    // consistent: if either one is ever regenerated, staleness in the
    // other becomes visible instead of two independently-drifting
    // fixtures.
    bytes constant BLUEPRINT_CALLDATA =
        hex"000000000000000000000000000000000000000000000000000000000000044c"
        hex"0000000000000000000000000000000000000000000000000000000000000000"
        hex"c4bb1c851b1c74593f61f8d1f99ec07e2960d847a94d4a736e321ba387d4d2d7"
        hex"0000000000000000000000000000000000000000000000000000000000000000"
        hex"0000000000000000000000009999999999999999999999999999999999999999"
        hex"0000000000000000000000000000000000000000000000000000000000000000"
        hex"0000000000000000000000000000000000000000000000000000000000000140"
        hex"00000000000000000000000000000000000000000000000000000000000f4240"
        hex"00000000000000000000000000000000000000000000000000000000000186a0"
        hex"00000000000000000000000000000000000000000000000000000006fc23ac00"
        hex"0000000000000000000000000000000000000000000000000000000000000004"
        hex"deadbeef00000000000000000000000000000000000000000000000000000000";

    /// Prints the domain-separated hash for the fixture above so it can
    /// be pasted into
    /// omega-execution/src/signer.rs::tests::compute_bp_hash_matches_solc_golden_vector's
    /// `solc_golden` constant — same copy-the-log-line workflow as
    /// `test_print_golden_blueprint_calldata`.
    ///
    /// Run with:
    ///   forge test --match-test test_print_golden_domain_separated_blueprint_hash -vv
    function test_print_golden_domain_separated_blueprint_hash() public pure {
        bytes32 bpHash = keccak256(
            abi.encode(ORCHESTRATOR, CHAIN_ID, BLUEPRINT_CALLDATA)
        );
        console.logBytes32(bpHash);
    }

    /// Same computation, asserted rather than merely printed, so this
    /// fixture can't silently drift once the golden value below has been
    /// filled in on the Rust side and cross-pasted back here.
    ///
    /// CONFIRMED (2026-08-24): `forge test --match-test
    /// test_print_golden_domain_separated_blueprint_hash -vv` printed
    /// this exact value on a real run against solc 0.8.24 — an EXACT
    /// match to the value originally seeded here via an independent
    /// Python (`eth_abi`) computation, so nothing needed to change once
    /// solc's own answer came back.
    function test_domain_separated_blueprint_hash_matches_expected() public pure {
        bytes32 expected = 0x74d1c22598dab6e5f1cb1a1809d3b7255728a0c10c971a9f05257cd6758b356b;
        bytes32 actual = keccak256(
            abi.encode(ORCHESTRATOR, CHAIN_ID, BLUEPRINT_CALLDATA)
        );
        assertEq(actual, expected, "domain-separated bp_hash must match the solc-confirmed golden value");
    }
}