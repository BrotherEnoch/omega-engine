// contracts/certora/harness/MockSP1Verifier.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// @title MockSP1Verifier — local-deploy harness matching SP1's real ISP1Verifier interface
/// @notice Matches the REAL `ISP1Verifier` interface declared in
///         `contracts/src/verifiers/SP1StarkVerifierAdapter.sol` exactly:
///         `verifyProof(bytes32 programVKey, bytes calldata publicValues, bytes calldata
///         proofBytes) external view` — three arguments, NO return value. Per that file's
///         own header comment, SP1's real verifier signals success by simply not reverting,
///         and failure by reverting — this is a fundamentally different signaling
///         convention from `IStarkVerifier.verify(...) returns (bool)` (the interface
///         `MockStarkVerifier.sol`, elsewhere in this harness directory, correctly
///         implements instead — that one is what OmegaVault itself expects from the
///         ADAPTER's output, one layer downstream of this file; the two mocks are for two
///         different interfaces and are not interchangeable).
/// @dev    SCOPE NOTE, same as this directory's other harnesses: this is for exercising the
///         deploy/wiring path locally (e.g. `DeployCore.s.sol` against a local Anvil
///         instance), not for testing real STARK proof verification, which is out of scope
///         for a Solidity-side mock entirely. `shouldPass` defaults to `true` so a bare
///         deploy-and-wire-up flow works out of the box; flip it to `false` via
///         `setShouldPass` if a test specifically needs to exercise
///         `SP1StarkVerifierAdapter.verify()`'s try/catch failure path.
contract MockSP1Verifier {
    bool public shouldPass = true;

    error MockSP1VerificationFailed();

    function setShouldPass(bool v) external {
        shouldPass = v;
    }

    /// @dev Deliberately NO return value, matching ISP1Verifier.verifyProof's real
    ///      signature — success is "did not revert", not a returned `true`.
    function verifyProof(
        bytes32 /* programVKey */,
        bytes calldata /* publicValues */,
        bytes calldata /* proofBytes */
    ) external view {
        if (!shouldPass) revert MockSP1VerificationFailed();
    }
}