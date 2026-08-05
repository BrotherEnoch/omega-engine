// contracts/certora/harness/DummyERC20.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/ERC20.sol";

/// @title DummyERC20 — harness token for Certora scenes
/// @notice Inherits real OpenZeppelin ERC20 rather than a hand-rolled implementation —
///         consistent with the choice made for OmegaVaultRescueAndPending.spec and
///         OmegaOrchestratorRescue.spec earlier in this project (link a real, audited
///         implementation instead of inventing one that could itself hide a bug). `mint`
///         is deliberately unguarded — this is a Certora scene harness, not a deployable
///         contract, so there is no meaningful "who can mint" property to protect here.
/// @dev    SCOPE NOTE, same as those other specs: this makes rules that rely on this
///         token's transfer semantics sound for a real, well-behaved ERC20 specifically —
///         not for an arbitrary IERC20. `profit_token` is deployment-time-fixed and
///         immutable on the real OmegaVault; this harness models the well-behaved case.
contract DummyERC20 is ERC20 {
    constructor() ERC20("Dummy Profit", "DPROF") {}

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }
}
