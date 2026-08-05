// certora/specs/Orchestrator.spec
//
// Certora CVL spec for OmegaOrchestrator — replay, nonce, and key-rotation
// properties on the real public surface of contracts/src/OmegaOrchestrator.sol.
//
// Flashloan-callback safety and emergency pause are owned by
// OmegaOrchestratorRescue.spec. This file does not declare getters for private
// state (_flashloanInProgress, _activeUniswapV3Pool): those fields have no
// public accessors on the current contract, and activeFlashloanCount does not
// exist. Callback safety is proven observationally in the Rescue spec (cold
// callbacks always revert).
//
// Authored against CVL2. Run via certoraRun; provider/strategy summaries are
// approximate without linked real provider contracts.

methods {
    function executed_blueprints(bytes32) external returns (bool) envfree;
    function next_nonce(bytes32) external returns (uint64) envfree;
    function execution_key() external returns (address) envfree;
    function pending_key() external returns (address) envfree;
    function rotation_window_end_block() external returns (uint64) envfree;
    function strategy_frozen(bytes32) external returns (bool) envfree;

    function execute(bytes, bytes) external;
    function initiateKeyRotation(address, uint64) external;
    function finalizeKeyRotation() external;
    function registerStrategy(bytes32, address) external;
    function freezeStrategy(bytes32) external;

    function _.pendingProofCount() external => NONDET;
    function _.flashLoan(address, address[], uint256[], bytes) external => NONDET;
    function _.flashLoanSimple(address, address, uint256, bytes, uint16) external => NONDET;
    function _.flash(address, uint256, uint256, bytes) external => NONDET;
    function _.execute(bytes, uint256) external returns (uint256) => NONDET;
    function _.profit_token() external returns (address) => NONDET;
    function _.receivePendingProfit(bytes32, uint256) external => NONDET;
    function _.balanceOf(address) external returns (uint256) => DISPATCHER(true);
    function _.transfer(address, uint256) external returns (bool) => DISPATCHER(true);
    function _.transferFrom(address, address, uint256) external returns (bool) => DISPATCHER(true);
    function _.approve(address, uint256) external returns (bool) => DISPATCHER(true);
    function _.forceApprove(address, uint256) external => DISPATCHER(true);
}

//////////////////////////////////////////////////////////////////////////////
// Replay protection
//////////////////////////////////////////////////////////////////////////////

/// Once a blueprint executes successfully, the same blueprintCalldata must not
/// execute successfully again (domain-separated hash is marked in
/// executed_blueprints inside execute).
rule blueprintReplayIsImpossible(bytes blueprintCalldata, bytes sig) {
    env e;

    execute(e, blueprintCalldata, sig);
    storage afterFirst = lastStorage;

    execute@withrevert(e, blueprintCalldata, sig) at afterFirst;

    assert lastReverted,
        "identical blueprint calldata must never execute successfully twice";
}

//////////////////////////////////////////////////////////////////////////////
// Nonce monotonicity
//////////////////////////////////////////////////////////////////////////////

/// For any chain-scoped nonce key, a successful execute either leaves that
/// bucket unchanged or advances it by exactly one.
rule nonceMonotonic(bytes32 chainScopedKey) {
    env e;
    uint64 before = next_nonce(chainScopedKey);

    calldataarg blueprintCalldata;
    calldataarg sig;
    execute(e, blueprintCalldata, sig);

    uint64 afterCall = next_nonce(chainScopedKey);

    assert afterCall == before || afterCall == assert_uint64(before + 1),
        "a strategy nonce must advance by exactly one, or not move if this call used a different key";
}

//////////////////////////////////////////////////////////////////////////////
// Key rotation
//////////////////////////////////////////////////////////////////////////////

/// finalizeKeyRotation clears pending_key; a second finalize with no new
/// initiation must revert.
rule rotationIsOneShotPerInitiation() {
    env e1;
    env e2;
    require pending_key() != 0;

    finalizeKeyRotation(e1);

    assert pending_key() == 0,
        "pending_key must be cleared after a successful finalizeKeyRotation";

    finalizeKeyRotation@withrevert(e2);

    assert lastReverted,
        "finalizeKeyRotation must revert if there is no pending rotation to finalize";
}

/// initiateKeyRotation while a rotation is already pending must revert.
rule cannotInitiateWhileRotationPending(env e, address newKey, uint64 windowBlocks) {
    require pending_key() != 0;

    initiateKeyRotation@withrevert(e, newKey, windowBlocks);

    assert lastReverted,
        "initiateKeyRotation must revert while a rotation window is already active";
}