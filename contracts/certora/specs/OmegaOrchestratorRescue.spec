// certora/specs/OmegaOrchestratorRescue.spec
//
// Verifies OmegaOrchestrator's rescue guard in its own scene, separate from
// OmegaVault (per review: verifying both together risks state-space
// explosion; separate scenes isolate assumptions and speed up proof time).
//
// Method signatures below are copied verbatim from the actual
// OmegaOrchestrator.sol source in this project:
//   function rescueERC20(address token, address to, uint256 amount)
//       external onlyRole(DEFAULT_ADMIN_ROLE) nonReentrant;
//   function rescueETH(address payable to, uint256 amount)
//       external onlyRole(DEFAULT_ADMIN_ROLE) nonReentrant;
//   function activeFlashloanCount() external view returns (uint256);
//
// This scene does NOT link OmegaVault, flashloanProvider, or aavePool —
// none of the rules below call into them. A future spec that verifies
// execute() itself (the full flashloan flow) would need those linked; that
// is out of scope for this file, which only covers the rescue surface.

using OmegaOrchestrator as orchestrator;

methods {
    function rescueERC20(address token, address to, uint256 amount) external;
    function rescueETH(address to, uint256 amount) external;
    function activeFlashloanCount() external returns (uint256) envfree;
    function hasRole(bytes32, address) external returns (bool) envfree;
    function DEFAULT_ADMIN_ROLE() external returns (bytes32) envfree;
}

//////////////////////////////////////////////////////////////////////////////
// Access control
//////////////////////////////////////////////////////////////////////////////

rule onlyAdminCanRescueERC20(env e, address token, address to, uint256 amount) {
    bool isAdmin = hasRole(DEFAULT_ADMIN_ROLE(), e.msg.sender);

    rescueERC20@withrevert(e, token, to, amount);

    assert !isAdmin => lastReverted,
        "a caller without DEFAULT_ADMIN_ROLE must never succeed in calling rescueERC20";
}

rule onlyAdminCanRescueEth(env e, address to, uint256 amount) {
    bool isAdmin = hasRole(DEFAULT_ADMIN_ROLE(), e.msg.sender);

    rescueETH@withrevert(e, to, amount);

    assert !isAdmin => lastReverted,
        "a caller without DEFAULT_ADMIN_ROLE must never succeed in calling rescueETH";
}

//////////////////////////////////////////////////////////////////////////////
// Rescue requires no active flashloan
//////////////////////////////////////////////////////////////////////////////

rule rescueERC20RequiresNoActiveFlashloan(env e, address token, address to, uint256 amount) {
    require orchestrator.activeFlashloanCount() > 0;

    orchestrator.rescueERC20@withrevert(e, token, to, amount);

    assert lastReverted,
        "rescueERC20 must revert while activeFlashloanCount() > 0";
}

rule rescueEthRequiresNoActiveFlashloan(env e, address to, uint256 amount) {
    require orchestrator.activeFlashloanCount() > 0;

    orchestrator.rescueETH@withrevert(e, to, amount);

    assert lastReverted,
        "rescueETH must revert while activeFlashloanCount() > 0";
}

//////////////////////////////////////////////////////////////////////////////
// Sanity: rescue succeeds (for a valid admin caller, valid recipient) when
// no flashloan is active — a complement to the two rules above, since a
// spec that only proves "reverts when active" could vacuously pass if
// rescue also always reverted when INactive due to an unrelated bug.
//////////////////////////////////////////////////////////////////////////////

rule rescueERC20CanSucceedWhenIdle(env e, address token, address to, uint256 amount) {
    require orchestrator.activeFlashloanCount() == 0;
    require hasRole(DEFAULT_ADMIN_ROLE(), e.msg.sender);
    require to != 0;
    require e.msg.value == 0;

    rescueERC20@withrevert(e, token, to, amount);

    satisfy !lastReverted,
        "rescueERC20 must be reachable (not unconditionally reverting) when idle, "
        "called by an admin, with a non-zero recipient";
}
