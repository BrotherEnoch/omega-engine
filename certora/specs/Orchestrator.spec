// certora/specs/Orchestrator.spec
/*
 * OmegaOrchestrator.spec
 *
 * Certora CVL spec for OmegaOrchestrator (v14, Gate-integrated).
 *
 * Scope: this file targets the revert-path / transient-state regression property (priority
 * 4 from review) plus the replay/nonce/rotation properties that follow directly from the
 * same "state must return to a clean baseline after every completed transaction" idea.
 *
 * IMPORTANT FRAMING for the transient-state rules below: today's implementation clears
 * `_flashloanInProgress`, `_activeUniswapV3Pool`, and the Gate's transient counter
 * unconditionally at the end of `_executeFlashloan`, and EVM revert semantics unwind ALL
 * state (including transient storage) on any reverted path regardless of where the revert
 * occurs. That means these rules cannot currently fail — they hold for free today. Their
 * value is as REGRESSION GUARDS: if a future change wraps a provider call in try/catch (to
 * make one provider's failure non-fatal instead of reverting the whole transaction) and
 * forgets that the explicit cleanup lines now have to run on that non-reverting failure
 * path too, these rules are what catch it. Do not read a pass here as evidence the current
 * code is defending against something it doesn't already get for free from the EVM.
 *
 * NOTE ON TOOLING: authored against CVL2 syntax for the compiled OmegaOrchestrator.sol /
 * ReconciliationRotationGate.sol in this delivery; not run against the actual Certora
 * Prover in this environment. Run via `certoraRun` and expect minor syntax fixes on first
 * pass, particularly around the flashloan-provider callback summaries, which are
 * necessarily approximate here without the real Balancer/Aave/UniswapV3 contracts linked.
 */

using OmegaOrchestrator as orchestrator;

methods {
    // Public/external getters already exposed by OmegaOrchestrator.sol
    function flashloanInProgress() external returns (bool) envfree;
    function activeUniswapV3Pool() external returns (address) envfree;
    function activeFlashloanCount() external returns (uint256) envfree;
    function executed_blueprints(bytes32) external returns (bool) envfree;
    function next_nonce(bytes32) external returns (uint64) envfree;
    function execution_key() external returns (address) envfree;
    function pending_key() external returns (address) envfree;
    function rotation_window_end_block() external returns (uint64) envfree;
    function strategy_frozen(bytes32) external returns (bool) envfree;

    // State-changing entry points under test
    function execute(bytes, bytes) external;
    function initiateKeyRotation(address, uint64) external;
    function finalizeKeyRotation() external;
    function registerStrategy(bytes32, address) external;
    function freezeStrategy(bytes32) external;

    // Cross-contract call this contract makes into the Vault during rotation/registration —
    // summarized as NONDET here since the real Vault's internals are out of scope for this
    // spec (they're covered by OmegaVault.spec instead); a dedicated multi-contract run
    // that links the real Vault would replace this with DISPATCHER(true).
    function _.pendingProofCount() external => NONDET;

    // Flashloan provider / strategy / token callbacks — approximate summaries. A full run
    // should link real (or faithfully mocked) Balancer/Aave/UniswapV3 provider contracts and
    // a strategy harness instead of NONDET here, since these are exactly the external calls
    // the revert-path rules below care about the ordering of.
    function _.flashLoan(address, address[], uint256[], bytes) external => NONDET;
    function _.flashLoanSimple(address, address, uint256, bytes, uint16) external => NONDET;
    function _.flash(address, uint256, uint256, bytes) external => NONDET;
    function _.execute(bytes, uint256) external => NONDET;
    function _.balanceOf(address) external => DISPATCHER(true);
    function _.transfer(address, uint256) external => DISPATCHER(true);
    function _.transferFrom(address, address, uint256) external => DISPATCHER(true);
}

// ─────────────────────────────────────────────────────────────────────────────
// PRIORITY 4 — Transient flashloan-callback state always returns to a clean baseline
// after any COMPLETED (non-reverted) call to this contract, for every public/external
// function — not just execute() itself. See the framing note at the top of this file for
// why this is a regression guard rather than a live soundness proof under the current code.
// ─────────────────────────────────────────────────────────────────────────────
invariant transientStateClearedOutsideExecution()
    !flashloanInProgress() && activeUniswapV3Pool() == 0 && activeFlashloanCount() == 0
    {
        preserved with (env e) {
            // Trivially true before the first call in any run; the interesting content of
            // this invariant is entirely in the "preserved by every method" obligation
            // Certora checks automatically, not in this precondition.
        }
    }

// Explicit companion rule for execute() specifically, since it's the one function that
// legitimately sets this state true mid-call — proves it's always false again by the time
// execute() returns successfully, for an arbitrary blueprint/signature/provider branch.
rule flashloanStateClearedAfterExecute(bytes blueprintCalldata, bytes sig) {
    env e;
    require !flashloanInProgress();
    require activeUniswapV3Pool() == 0;

    execute(e, blueprintCalldata, sig);

    assert !flashloanInProgress(),
        "flashloanInProgress must be false after any successful execute() call, for every provider branch";
    assert activeUniswapV3Pool() == 0,
        "activeUniswapV3Pool must be reset to zero after any successful execute() call";
    assert activeFlashloanCount() == 0,
        "the Gate's transient flashloan counter must return to zero after any successful execute() call";
}

// ─────────────────────────────────────────────────────────────────────────────
// Replay protection: a blueprint hash, once executed, can never execute again — and the
// per-strategy nonce is strictly increasing by one on every successful execute() for that
// strategy, never reused or skipped.
// ─────────────────────────────────────────────────────────────────────────────
rule blueprintReplayIsImpossible(bytes blueprintCalldata, bytes sig) {
    env e;
    // bpHash is recomputed inside execute(); we can't reference it directly here without
    // duplicating the hashing logic, so this rule is best expressed as: executing the exact
    // same calldata twice in a row (same blueprintCalldata, same or different sig — a valid
    // sig for the same hash is what matters) must have the second call revert once the first
    // succeeded. Run as: call execute once, snapshot storage, call again with identical args.
    storage initial = lastStorage;

    execute(e, blueprintCalldata, sig);
    storage afterFirst = lastStorage;

    execute@withrevert(e, blueprintCalldata, sig) at afterFirst;

    assert lastReverted,
        "identical blueprint calldata must never execute successfully twice";
}

rule nonceMonotonic(bytes32 strategyId, bytes32 chainScopedKey) {
    env e;
    uint64 before = next_nonce(chainScopedKey);

    calldataarg blueprintCalldata; calldataarg sig;
    execute(e, blueprintCalldata, sig);

    uint64 afterCall = next_nonce(chainScopedKey);
    // Either this call touched a different nonce bucket (unaffected), or it advanced by
    // exactly one — never skips, never regresses, never reuses.
    assert afterCall == before || afterCall == assert_uint64(before + 1),
        "a strategy's nonce must advance by exactly one, or not move at all if this call was for a different strategy";
}

// ─────────────────────────────────────────────────────────────────────────────
// Rotation gating: finalizeKeyRotation must not succeed while this contract's own
// flashloan-in-progress state is non-clean. (The Vault side of "no pending proofs" is
// summarized as NONDET above and is out of scope for this file — see the framing note.)
// ─────────────────────────────────────────────────────────────────────────────
rule rotationRequiresCleanFlashloanState() {
    env e;
    require flashloanInProgress() || activeFlashloanCount() > 0;

    finalizeKeyRotation@withrevert(e);

    assert lastReverted,
        "finalizeKeyRotation must revert while this contract has an open flashloan, regardless of Vault state";
}

rule rotationIsOneShotPerInitiation() {
    env e1; env e2;
    require pending_key() != 0;

    finalizeKeyRotation(e1);

    assert pending_key() == 0,
        "pending_key must be cleared after a successful finalizeKeyRotation";

    finalizeKeyRotation@withrevert(e2);

    assert lastReverted,
        "finalizeKeyRotation must revert if there is no pending rotation to finalize";
}
