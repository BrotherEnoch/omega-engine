// certora/specs/OmegaOrchestratorRescue.spec
//
// PURPOSE
// -------
// Certora CVL formal verification of OmegaOrchestrator flashloan-callback safety
// and emergency pause controls.
//
// Theorem (callback safety):
//   receiveFlashLoan, executeOperation, and uniswapV3FlashCallback cannot execute
//   their privileged body (token transfers, strategy call, _pendingNetProfit write)
//   unless the call is nested inside an in-progress _executeFlashloan started by
//   execute() on this contract.
//
// Mechanism under test (contracts/src/OmegaOrchestrator.sol):
//   - Only _executeFlashloan sets _flashloanInProgress = true (and, on the Uniswap
//     V3 branch, _activeUniswapV3Pool).
//   - Every callback requires that flag (and the correct msg.sender / initiator).
//   - Therefore any direct or provider-relayed callback outside execute reverts
//     before the body runs (OnlyFlashloanProvider or UnexpectedFlashloanCallback).
//
// Emergency surface (no ERC20/ETH sweep on this contract):
//   - emergencyPause: EMERGENCY_ROLE only
//   - unpause: DEFAULT_ADMIN_ROLE only
//   - execute is whenNotPaused
//
// Replay, nonce, and key-rotation properties live in Orchestrator.spec.
//
// Linked via: certora/confs/OmegaOrchestrator.foundry.conf
// Method signatures match contracts/src/OmegaOrchestrator.sol (v13).
// Authored against CVL2. Provider/strategy/token summaries are approximate;
// a full multi-contract run should link real or mocked Balancer/Aave/UniswapV3
// and a strategy harness.

methods {
    function EMERGENCY_ROLE() external returns (bytes32) envfree;
    function DEFAULT_ADMIN_ROLE() external returns (bytes32) envfree;
    function hasRole(bytes32, address) external returns (bool) envfree;
    function paused() external returns (bool) envfree;

    function flashloanProvider() external returns (address) envfree;
    function aavePool() external returns (address) envfree;
    function vault() external returns (address) envfree;
    function execution_key() external returns (address) envfree;
    function pending_key() external returns (address) envfree;

    function execute(bytes, bytes) external;
    function emergencyPause() external;
    function unpause() external;

    function receiveFlashLoan(
        address[] tokens,
        uint256[] amounts,
        uint256[] feeAmounts,
        bytes userData
    ) external;

    function executeOperation(
        address asset,
        uint256 amount,
        uint256 premium,
        address initiator,
        bytes params
    ) external returns (bool);

    function uniswapV3FlashCallback(
        uint256 fee0,
        uint256 fee1,
        bytes data
    ) external;

    function _.flashLoan(address, address[], uint256[], bytes) external => NONDET;
    function _.flashLoanSimple(address, address, uint256, bytes, uint16) external => NONDET;
    function _.flash(address, uint256, uint256, bytes) external => NONDET;
    function _.execute(bytes, uint256) external returns (uint256) => NONDET;
    function _.token0() external returns (address) => NONDET;
    function _.token1() external returns (address) => NONDET;
    function _.profit_token() external returns (address) => NONDET;
    function _.receivePendingProfit(bytes32, uint256) external => NONDET;
    function _.balanceOf(address) external returns (uint256) => DISPATCHER(true);
    function _.transfer(address, uint256) external returns (bool) => DISPATCHER(true);
    function _.transferFrom(address, address, uint256) external returns (bool) => DISPATCHER(true);
    function _.approve(address, uint256) external returns (bool) => DISPATCHER(true);
    function _.forceApprove(address, uint256) external => DISPATCHER(true);
}

//////////////////////////////////////////////////////////////////////////////
// Flashloan callback safety
//
// Public-surface encoding of Cases I–II of the safety theorem:
// when no execute()-owned _executeFlashloan frame is open, every callback
// reverts before the body. Private _flashloanInProgress is not exposed; the
// observable consequence is lastReverted on a cold call.
//////////////////////////////////////////////////////////////////////////////

rule receiveFlashLoanRevertsWithoutInProgress(
    env e,
    address[] tokens,
    uint256[] amounts,
    uint256[] feeAmounts,
    bytes userData
) {
    receiveFlashLoan@withrevert(e, tokens, amounts, feeAmounts, userData);

    assert lastReverted,
        "receiveFlashLoan must revert outside an in-progress flashloan (OnlyFlashloanProvider or UnexpectedFlashloanCallback)";
}

rule executeOperationRevertsWithoutInProgress(
    env e,
    address asset,
    uint256 amount,
    uint256 premium,
    address initiator,
    bytes params
) {
    executeOperation@withrevert(e, asset, amount, premium, initiator, params);

    assert lastReverted,
        "executeOperation must revert outside an in-progress flashloan (OnlyFlashloanProvider, UnexpectedFlashloanCallback, or InvalidFlashloanCallback)";
}

rule uniswapV3FlashCallbackRevertsWithoutInProgress(
    env e,
    uint256 fee0,
    uint256 fee1,
    bytes data
) {
    uniswapV3FlashCallback@withrevert(e, fee0, fee1, data);

    assert lastReverted,
        "uniswapV3FlashCallback must revert outside an in-progress flashloan (OnlyFlashloanProvider or UnexpectedFlashloanCallback)";
}

/// Wrong Balancer sender cannot pass the provider gate even if a loan were armed.
/// On a cold call this is subsumed by the flag check; the sender check is still
/// the first line of defence when the attacker does not control flashloanProvider.
rule receiveFlashLoanRequiresProviderSender(
    env e,
    address[] tokens,
    uint256[] amounts,
    uint256[] feeAmounts,
    bytes userData
) {
    require e.msg.sender != flashloanProvider();

    receiveFlashLoan@withrevert(e, tokens, amounts, feeAmounts, userData);

    assert lastReverted,
        "receiveFlashLoan must revert when msg.sender is not flashloanProvider";
}

/// Wrong Aave sender cannot pass the provider gate.
rule executeOperationRequiresPoolSender(
    env e,
    address asset,
    uint256 amount,
    uint256 premium,
    address initiator,
    bytes params
) {
    require e.msg.sender != aavePool();

    executeOperation@withrevert(e, asset, amount, premium, initiator, params);

    assert lastReverted,
        "executeOperation must revert when msg.sender is not aavePool";
}

//////////////////////////////////////////////////////////////////////////////
// Emergency pause — access control and execute gating
//////////////////////////////////////////////////////////////////////////////

rule onlyEmergencyRoleCanPause(env e) {
    bool hasEmergency = hasRole(EMERGENCY_ROLE(), e.msg.sender);

    emergencyPause@withrevert(e);

    assert !hasEmergency => lastReverted,
        "caller without EMERGENCY_ROLE must never succeed at emergencyPause";
}

rule onlyAdminCanUnpause(env e) {
    bool isAdmin = hasRole(DEFAULT_ADMIN_ROLE(), e.msg.sender);

    unpause@withrevert(e);

    assert !isAdmin => lastReverted,
        "caller without DEFAULT_ADMIN_ROLE must never succeed at unpause";
}

rule emergencyPauseSetsPaused(env e) {
    require hasRole(EMERGENCY_ROLE(), e.msg.sender);
    require !paused();

    emergencyPause(e);

    assert paused(),
        "after a successful emergencyPause the contract must be paused";
}

rule unpauseClearsPaused(env e) {
    require hasRole(DEFAULT_ADMIN_ROLE(), e.msg.sender);
    require paused();

    unpause(e);

    assert !paused(),
        "after a successful unpause the contract must not be paused";
}

rule executeRevertsWhenPaused(env e, bytes blueprintCalldata, bytes sig) {
    require paused();

    execute@withrevert(e, blueprintCalldata, sig);

    assert lastReverted,
        "execute must revert while the contract is paused";
}

//////////////////////////////////////////////////////////////////////////////
// Combined: after emergencyPause, execute and cold callbacks stay gated
//////////////////////////////////////////////////////////////////////////////

rule afterEmergencyPauseExecutionAndCallbacksRemainGated(
    env ePause,
    env eExec,
    env eCb,
    bytes blueprintCalldata,
    bytes sig,
    address[] tokens,
    uint256[] amounts,
    uint256[] feeAmounts,
    bytes userData,
    address asset,
    uint256 amount,
    uint256 premium,
    address initiator,
    bytes params,
    uint256 fee0,
    uint256 fee1,
    bytes uniData
) {
    require hasRole(EMERGENCY_ROLE(), ePause.msg.sender);
    require !paused();

    emergencyPause(ePause);
    assert paused();

    execute@withrevert(eExec, blueprintCalldata, sig);
    assert lastReverted,
        "execute must still revert after emergencyPause";

    receiveFlashLoan@withrevert(eCb, tokens, amounts, feeAmounts, userData);
    assert lastReverted,
        "receiveFlashLoan must still revert after emergencyPause when no loan is in progress";

    executeOperation@withrevert(eCb, asset, amount, premium, initiator, params);
    assert lastReverted,
        "executeOperation must still revert after emergencyPause when no loan is in progress";

    uniswapV3FlashCallback@withrevert(eCb, fee0, fee1, uniData);
    assert lastReverted,
        "uniswapV3FlashCallback must still revert after emergencyPause when no loan is in progress";
}