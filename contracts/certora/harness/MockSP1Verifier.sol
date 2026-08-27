// contracts/certora/harness/MockSP1Verifier.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// @title MockSP1Verifier — local-deploy harness matching SP1's real ISP1Verifier interface
/// @notice Matches the REAL `ISP1Verifier` interface declared in
///         `contracts/src/verifiers/SP1StarkVerifierAdapter.sol` exactly:
///         `verifyProof(bytes32 programVKey, bytes calldata publicValues, bytes calldata
///         proofBytes) external view` — three arguments, NO return value. Per that file's
///         own header comment, SP1's real verifier signals success by simply not reverting,
///         and failure by reverting.
/// @dev    SCOPE NOTE: this is for exercising the deploy/wiring path locally, not for
///         testing real STARK proof verification. `shouldPass` defaults to `true` so a bare
///         deploy-and-wire-up flow works out of the box.
contract MockSP1Verifier {
    bool public shouldPass = true;

    error MockSP1VerificationFailed();

    function setShouldPass(bool v) external {
        shouldPass = v;
    }

    function verifyProof(
        bytes32 /* programVKey */,
        bytes calldata /* publicValues */,
        bytes calldata /* proofBytes */
    ) external view {
        if (!shouldPass) revert MockSP1VerificationFailed();
    }
}