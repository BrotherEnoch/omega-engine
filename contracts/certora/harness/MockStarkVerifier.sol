// contracts/certora/harness/MockStarkVerifier.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// @title MockStarkVerifier — Certora scene harness
/// @notice Matches the REAL IStarkVerifier interface from OmegaVault.sol exactly:
///         `verify(bytes calldata proof, bytes32 blueprintHash, bytes32 publicInputsHash)
///         external view returns (bool)` — three arguments, not two. A prior draft mock
///         used a 2-arg signature that didn't match the real interface at all (would have
///         reverted on selector mismatch had it ever actually been called through the
///         typed interface) — fixed here to the real ABI, same fix already applied to the
///         Foundry-side MockStarkVerifier earlier in this project.
/// @dev    `shouldPass` is a plain public bool rather than parameterized per-proof, since
///         these Vault-only rules don't need to distinguish between different proofs'
///         validity — they're testing what OmegaVault does with a verifier's yes/no
///         answer, not the verifier's own correctness (which is out of scope for a Solidity
///         spec entirely; the STARK circuit itself needs its own, separate verification).
contract MockStarkVerifier {
    bool public shouldPass;

    function setShouldPass(bool v) external {
        shouldPass = v;
    }

    function verify(
        bytes calldata /* proof */,
        bytes32 /* blueprintHash */,
        bytes32 /* publicInputsHash */
    ) external view returns (bool) {
        return shouldPass;
    }
}
