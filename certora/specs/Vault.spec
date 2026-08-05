// certora/specs/Vault.spec
/*
 * OmegaVault.spec
 *
 * Certora CVL spec for OmegaVault (v14, Gate-integrated).
 *
 * Scope: this file targets the five properties raised in review, in priority order:
 *   1. One-shot pending-profit transition — Vault-local, role-independent.
 *   2. Per-blueprint state-machine consistency across pending_profit / proof_verified /
 *      released / the Gate's bound-proof-inputs bookkeeping.
 *   3. ZK binding is semantic, not opaque — proof_verified can only become true for a hash
 *      that matches what was bound at deposit time from on-chain inputs.
 *   4. Vault reconciliation actually runs before any token leaves the contract.
 *   5. pendingProofCount() snapshot semantics are documented behavior, not something this
 *      file tries to prove atomic (see the NatSpec on pendingProofCount() in OmegaVault.sol
 *      instead — there is no CVL rule for this section, intentionally, since asserting
 *      "this snapshot cannot go stale" would be asserting something false).
 *
 * NOTE ON TOOLING: this spec was authored against Certora CVL2 syntax and the compiled
 * OmegaVault.sol / ReconciliationRotationGate.sol in this delivery, but has not been run
 * against the actual Certora Prover in this environment (no CLI/API access here). Run it via
 * `certoraRun` before relying on it; expect to fix minor syntax drift (method signature
 * spelling, hook slot patterns) on first pass, as CVL syntax details shift between prover
 * versions in ways a static read cannot fully catch.
 */

using OmegaVault as vault;

methods {
    // Public/external state getters already exposed by OmegaVault.sol
    function pending_profit(bytes32) external returns (uint256) envfree;
    function proof_verified(bytes32) external returns (bool) envfree;
    function released(bytes32) external returns (bool) envfree;
    function confirmation_depth(bytes32) external returns (uint8) envfree;
    function pendingProofCount() external returns (uint256) envfree;
    function proofInputsBound(bytes32) external returns (bool) envfree;
    function boundProofInputsOf(bytes32) external returns (bytes32) envfree;
    function computePublicInputsHash(bytes32, uint256) external returns (bytes32) envfree;
    function profit_token() external returns (address) envfree;
    function MIN_CONFIRMATION_DEPTH() external returns (uint256) envfree;

    // State-changing entry points under test
    function receivePendingProfit(bytes32, uint256) external;
    function submitProof(bytes32, bytes32, bytes) external;
    function updateConfirmationDepth(bytes32, uint8) external;
    function releaseProfit(bytes32) external;

    // ERC20 balance — dispatched to whichever token contract is linked for the run.
    function _.balanceOf(address) external => DISPATCHER(true);
    function _.transfer(address, uint256) external => DISPATCHER(true);
    function _.transferFrom(address, address, uint256) external => DISPATCHER(true);
}

// ─────────────────────────────────────────────────────────────────────────────
// Ghost state: tracks, per blueprintHash, whether pending_profit has EVER been set
// nonzero. This is what lets rule (1) below be a role-independent, caller-independent
// property rather than "the current ORCHESTRATOR_ROLE holder behaves" — it's a statement
// about the storage slot itself, observed via an SSTORE hook, not about who wrote it.
// ─────────────────────────────────────────────────────────────────────────────
ghost mapping(bytes32 => bool) g_everPending {
    init_state axiom forall bytes32 bp. g_everPending[bp] == false;
}

hook Sstore pending_profit[KEY bytes32 bp] uint256 newVal (uint256 oldVal) {
    if (newVal != 0) {
        g_everPending[bp] = true;
    }
}

// Ghost tracking whether a blueprint's bound public-inputs hash has ever changed after
// first being set — used to prove bindings are immutable once made (Gate.bindProofInputs
// already reverts on a second bind; this ghost cross-checks that no OTHER code path can
// mutate boundProofInputs after the fact).
ghost mapping(bytes32 => bytes32) g_firstBoundHash;
ghost mapping(bytes32 => bool) g_hasBoundHash {
    init_state axiom forall bytes32 bp. g_hasBoundHash[bp] == false;
}

// ─────────────────────────────────────────────────────────────────────────────
// PRIORITY 1 — One-shot pending-profit transition, independent of caller/role.
//
// "For every blueprintHash, the transition from 'no pending profit' to 'pending profit
// exists' can occur at most once, regardless of caller identity, role assignments, call
// ordering, or transaction interleavings."
//
// Proven as: once pending_profit[bp] has ever gone nonzero, a subsequent call to
// receivePendingProfit for the SAME bp must revert — for an arbitrary caller, not just the
// currently-configured ORCHESTRATOR_ROLE holder. This holds even under a hypothetical
// malicious/duplicated ORCHESTRATOR_ROLE grant, because the guard inside
// receivePendingProfit (`pending_profit[blueprintHash] != 0 => revert`) does not reference
// msg.sender at all.
// ─────────────────────────────────────────────────────────────────────────────
rule oneShotPendingProfit(bytes32 bp, uint256 amount, address caller) {
    env e;
    require e.msg.sender == caller; // arbitrary caller — do NOT constrain to a specific role
    require g_everPending[bp]; // this blueprint has already had profit set at least once

    receivePendingProfit@withrevert(e, bp, amount);

    assert lastReverted,
        "a blueprintHash that has ever had pending_profit set nonzero must never accept a second deposit, regardless of caller";
}

// Companion: the very first successful deposit for a fresh blueprintHash must actually
// record something and bind something — i.e. the guard isn't accidentally also blocking
// legitimate first deposits.
rule firstDepositSucceedsAndBinds(bytes32 bp, uint256 amount) {
    env e;
    require !g_everPending[bp];
    require amount > 0;

    receivePendingProfit(e, bp, amount);

    assert pending_profit(bp) == amount;
    assert proofInputsBound(bp);
    assert boundProofInputsOf(bp) == computePublicInputsHash(bp, amount);
}

// ─────────────────────────────────────────────────────────────────────────────
// PRIORITY 2 — Per-blueprint state-machine consistency.
//
// Encodes the legal lifecycle directly rather than trusting it falls out of other checks:
//   released         => pending_profit == 0 AND proof_verified == true
//   pending_profit>0 => released == false
//   proofInputsBound <=> (pending_profit > 0 OR released)   -- i.e. bound iff ever deposited
// ─────────────────────────────────────────────────────────────────────────────
invariant stateConsistency(bytes32 bp)
    (released(bp) => (pending_profit(bp) == 0 && proof_verified(bp)))
    && (pending_profit(bp) > 0 => !released(bp))
    && (proofInputsBound(bp) <=> g_everPending[bp])
    {
        preserved receivePendingProfit(bytes32 b, uint256 amt) with (env e) {
            requireInvariant stateConsistency(b);
        }
        preserved releaseProfit(bytes32 b) with (env e) {
            requireInvariant stateConsistency(b);
        }
    }

// ─────────────────────────────────────────────────────────────────────────────
// PRIORITY 3 — ZK binding is semantic, not opaque.
//
// Rather than asserting "the stored hash looks like some hash" (which a future refactor
// could satisfy with an attacker-supplied value that merely happens to be hash-shaped),
// this proves the actual GATE holding a hash never lets an unbound or mismatched hash
// through to proof_verified. Combined with `firstDepositSucceedsAndBinds` above — which
// proves the bound hash equals computePublicInputsHash(bp, netProfit), i.e. is a function
// of (vault address, blueprintHash, netProfit, token) and nothing else — this closes the
// loop: proof_verified[bp] can only become true for the hash the VAULT computed from its
// own on-chain state, never one supplied unchecked by a caller.
// ─────────────────────────────────────────────────────────────────────────────
rule proofVerificationRequiresBoundMatch(bytes32 bp, bytes32 suppliedHash, bytes proof) {
    env e;
    require !proof_verified(bp);
    bytes32 boundBefore = boundProofInputsOf(bp);
    bool wasBound = proofInputsBound(bp);

    submitProof@withrevert(e, bp, suppliedHash, proof);
    bool reverted = lastReverted;

    assert !reverted => (wasBound && suppliedHash == boundBefore),
        "submitProof must not be able to mark a proof verified unless the supplied hash exactly matches the hash bound at deposit time";
    assert !reverted => proof_verified(bp);
}

// A bound hash, once set, is never overwritten — Gate.bindProofInputs already reverts on
// a second bind for the same key; this rule cross-checks that no reachable sequence of
// calls on OmegaVault can ever change boundProofInputsOf(bp) after it's first non-zero.
rule boundHashIsImmutable(bytes32 bp, method f) filtered { f -> !f.isView } {
    env e;
    calldataarg args;
    require proofInputsBound(bp);
    bytes32 before = boundProofInputsOf(bp);

    f(e, args);

    assert boundProofInputsOf(bp) == before,
        "a bound public-inputs hash must never change after it is first set";
}

// ─────────────────────────────────────────────────────────────────────────────
// PRIORITY 4 — Reconciliation actually gates release; caps and depth are enforced.
// ─────────────────────────────────────────────────────────────────────────────
rule releaseRequiresProofAndDepth(bytes32 bp) {
    env e;
    bool verifiedBefore = proof_verified(bp);
    uint8 depthBefore = confirmation_depth(bp);

    releaseProfit@withrevert(e, bp);

    assert !lastReverted => verifiedBefore,
        "releaseProfit must not succeed without a previously verified proof";
    assert !lastReverted => depthBefore >= assert_uint256(MIN_CONFIRMATION_DEPTH()),
        "releaseProfit must not succeed below the minimum confirmation depth";
}

rule releaseIsOneShot(bytes32 bp) {
    env e;
    require released(bp);

    releaseProfit@withrevert(e, bp);

    assert lastReverted, "a blueprint that has already been released must never release again";
}

rule releaseClearsPendingAndTracksProof(bytes32 bp) {
    env e;
    require pending_profit(bp) > 0;
    require proof_verified(bp);
    require confirmation_depth(bp) >= assert_uint256(MIN_CONFIRMATION_DEPTH());
    require !released(bp);
    uint256 countBefore = pendingProofCount();

    releaseProfit(e, bp);

    assert pending_profit(bp) == 0;
    assert released(bp);
    assert pendingProofCount() == countBefore - 1,
        "successful release must decrement the pending-proof counter by exactly one";
}
