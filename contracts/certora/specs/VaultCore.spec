// contracts/certora/specs/VaultCore.spec
//
// Tier A OmegaVault properties: C6 release gates, depth monotonicity, C9 fee-split
// bounds. Deliberately self-contained and ghost/hook-free -- no dependency on the
// pending-profit sum invariant or rescue non-interference rules, which live in
// OmegaVaultRescueAndPending.spec and need the ghost-mapping machinery this file
// intentionally avoids. Per the proof-strategy ordering this was requested against:
// this is meant to be the FIRST spec run, before VaultRescue's heavier scene.
//
// Method signatures below are copied verbatim from the actual OmegaVault.sol in this
// project -- releaseProfit(bytes32) takes exactly one argument, not a proof argument;
// see this file's own commit history in this conversation for why that specific claim
// needed checking rather than trusting a secondhand summary.
//
// Linked against contracts/certora/harness/DummyERC20.sol (profit_token) and
// contracts/certora/harness/MockStarkVerifier.sol (stark_verifier) -- see
// contracts/certora/conf/vault_core.conf for the exact link wiring.
//
// NOT run against the Certora Prover in this environment -- same caveat as every
// other spec in this project's history: this is a draft grounded in the real source,
// not a confirmed passing verification result.

using DummyERC20 as profitToken;

methods {
    // Vault state getters
    function pending_profit(bytes32) external returns (uint256) envfree;
    function confirmation_depth(bytes32) external returns (uint8) envfree;
    function proof_verified(bytes32) external returns (bool) envfree;
    function released(bytes32) external returns (bool) envfree;
    function totalPendingProfit() external returns (uint256) envfree;
    function dao_fee_bps() external returns (uint256) envfree;
    function MAX_DAO_FEE_BPS() external returns (uint256) envfree;
    function MIN_CONFIRMATION_DEPTH() external returns (uint256) envfree;
    function PER_TRANSFER_CAP() external returns (uint256) envfree;
    function dailyCapRemaining() external returns (uint256) envfree;
    function pil_treasury() external returns (address) envfree;
    function dao_fee_address() external returns (address) envfree;
    function profit_token() external returns (address) envfree;
    function computePublicInputsHash(bytes32, uint256) external returns (bytes32) envfree;
    function proofInputsBound(bytes32) external returns (bool) envfree;
    function boundProofInputsOf(bytes32) external returns (bytes32) envfree;
    function ORCHESTRATOR_ROLE() external returns (bytes32) envfree;
    function DEPTH_UPDATER_ROLE() external returns (bytes32) envfree;
    function hasRole(bytes32, address) external returns (bool) envfree;

    // State-changing entry points under test
    function receivePendingProfit(bytes32, uint256) external;
    function updateConfirmationDepth(bytes32, uint8) external;
    function submitProof(bytes32, bytes32, bytes) external;
    function releaseProfit(bytes32) external;

    function profitToken.balanceOf(address) external returns (uint256) envfree;
}

//////////////////////////////////////////////////////////////////////////////
// C6 — release gates: each individual gate, stated as its own cheap negative
// rule ("violating this one condition alone is enough to force a revert").
// These don't need exhaustive preconditions the way a "must succeed" rule
// would -- a revert for ANY reason still satisfies a "must revert" claim.
//////////////////////////////////////////////////////////////////////////////

rule releaseRevertsWithoutVerifiedProof(env e, bytes32 bp) {
    require !proof_verified(bp);

    releaseProfit@withrevert(e, bp);

    assert lastReverted,
        "releaseProfit must revert when proof_verified(bp) is false, regardless of any other state";
}

rule releaseRevertsBelowMinDepth(env e, bytes32 bp) {
    require assert_uint256(confirmation_depth(bp)) < MIN_CONFIRMATION_DEPTH();

    releaseProfit@withrevert(e, bp);

    assert lastReverted,
        "releaseProfit must revert when confirmation_depth(bp) is below MIN_CONFIRMATION_DEPTH";
}

rule releaseRevertsIfAlreadyReleased(env e, bytes32 bp) {
    require released(bp);

    releaseProfit@withrevert(e, bp);

    assert lastReverted,
        "releaseProfit must revert if this blueprintHash was already released";
}

rule releaseRevertsWithZeroPending(env e, bytes32 bp) {
    require pending_profit(bp) == 0;

    releaseProfit@withrevert(e, bp);

    assert lastReverted,
        "releaseProfit must revert when there is no pending profit recorded for this hash";
}

rule releaseRevertsAboveDailyCap(env e, bytes32 bp) {
    require pending_profit(bp) > dailyCapRemaining();

    releaseProfit@withrevert(e, bp);

    assert lastReverted,
        "releaseProfit must revert when the pending amount exceeds remaining daily capacity";
}

//////////////////////////////////////////////////////////////////////////////
// Release clears pending and marks released (lightweight, self-contained
// version -- OmegaVaultRescueAndPending.spec's releaseProfitDecreasesTotalByExactlyNet
// covers the totalPendingProfit side of this same transition; this rule is
// kept here too so VaultCore.spec doesn't depend on that file to state the
// most basic form of the property).
//////////////////////////////////////////////////////////////////////////////

rule releaseClearsPendingAndMarksReleased(env e, bytes32 bp) {
    require !released(bp);
    require proof_verified(bp);
    require assert_uint256(confirmation_depth(bp)) >= MIN_CONFIRMATION_DEPTH();
    require pending_profit(bp) > 0;
    require pending_profit(bp) <= PER_TRANSFER_CAP();
    require pending_profit(bp) <= dailyCapRemaining();
    require profitToken.balanceOf(currentContract) >= pending_profit(bp);

    releaseProfit@withrevert(e, bp);

    assert !lastReverted,
        "releaseProfit must succeed given every documented precondition satisfied";
    assert pending_profit(bp) == 0,
        "pending_profit[bp] must be zero after a successful release";
    assert released(bp),
        "released[bp] must be true after a successful release";
}

//////////////////////////////////////////////////////////////////////////////
// Depth monotonicity -- updateConfirmationDepth only ever advances, never
// regresses, for any caller/value combination.
//////////////////////////////////////////////////////////////////////////////

rule depthNeverDecreases(env e, bytes32 bp, uint8 newDepth) {
    uint8 before = confirmation_depth(bp);

    updateConfirmationDepth(e, bp, newDepth);

    assert confirmation_depth(bp) >= before,
        "confirmation_depth must never decrease after any successful updateConfirmationDepth call";
}

//////////////////////////////////////////////////////////////////////////////
// C9 — fee bounds
//////////////////////////////////////////////////////////////////////////////

/// dao_fee_bps can never exceed the hard cap, at any point -- both the immediate
/// setter path (there isn't one; it's timelocked) and the timelock's own
/// executeDaoFeeBpsChange are covered automatically by this being a plain
/// invariant, not a rule scoped to one entry point.
invariant daoFeeBpsNeverExceedsMax()
    dao_fee_bps() <= MAX_DAO_FEE_BPS();

/// On a successful release, the actual token amounts moved to pil_treasury and
/// dao_fee_address sum to exactly the released amount, and the DAO's share never
/// exceeds 10% of it -- checked via real balance deltas on the linked token, not
/// by re-deriving the split formula independently (which would just be checking
/// the spec's own arithmetic against itself).
rule c9FeeSplitCorrectOnRelease(env e, bytes32 bp) {
    require !released(bp);
    require proof_verified(bp);
    require assert_uint256(confirmation_depth(bp)) >= MIN_CONFIRMATION_DEPTH();
    uint256 net = pending_profit(bp);
    require net > 0;
    require net <= PER_TRANSFER_CAP();
    require net <= dailyCapRemaining();
    require profitToken.balanceOf(currentContract) >= net;

    address pil = pil_treasury();
    address dao = dao_fee_address();
    require pil != dao; // avoid the degenerate case where both balances are the same slot

    uint256 pilBefore = profitToken.balanceOf(pil);
    uint256 daoBefore = profitToken.balanceOf(dao);

    releaseProfit@withrevert(e, bp);
    assert !lastReverted,
        "release must succeed under these preconditions";

    uint256 pilAfter = profitToken.balanceOf(pil);
    uint256 daoAfter = profitToken.balanceOf(dao);
    uint256 pilShare = assert_uint256(pilAfter - pilBefore);
    uint256 daoShare = assert_uint256(daoAfter - daoBefore);

    assert pilShare + daoShare == net,
        "C9: pil_share + dao_fee must equal net exactly, no dust left unaccounted for";
    assert daoShare * 10 <= net,
        "C9: dao_fee must never exceed 10% of net (mirrors the contract's own belt-and-suspenders check)";
}

//////////////////////////////////////////////////////////////////////////////
// submitProof: binding match is enforced -- uses the ACTUAL stored binding
// (proofInputsBound / boundProofInputsOf), not a recomputed stand-in. An
// earlier draft of this rule tried to reconstruct the expected hash from
// computePublicInputsHash(bp, pending_profit(bp)) -- that's wrong once a
// blueprint has been released (pending_profit resets to 0, but the ORIGINAL
// bound hash was computed against the nonzero deposit amount, so the
// reconstruction would silently diverge from what's actually stored). Reading
// the real bound value directly avoids that class of bug entirely.
//////////////////////////////////////////////////////////////////////////////

rule submitProofRequiresBoundMatch(env e, bytes32 bp, bytes32 suppliedHash, bytes proof) {
    bool wasBound = proofInputsBound(bp);
    bytes32 boundHash = boundProofInputsOf(bp);

    submitProof@withrevert(e, bp, suppliedHash, proof);

    assert !lastReverted => (wasBound && suppliedHash == boundHash),
        "submitProof must not succeed unless this blueprintHash was previously bound AND the supplied publicInputsHash exactly matches what was bound at deposit time";
    assert !lastReverted => proof_verified(bp),
        "a successful submitProof call must leave proof_verified(bp) set to true";
}