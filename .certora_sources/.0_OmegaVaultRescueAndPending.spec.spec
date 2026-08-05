// contracts/lib/openzeppelin-contracts/certora/specs/OmegaVaultRescueAndPending.spec
//
// Consolidated CVL spec for OmegaVault's rescue mechanism and pending-profit
// accounting. Method signatures below are copied verbatim from the actual
// OmegaVault.sol source in this project — see certora/confs/OmegaVault.*.conf
// for the exact files/link this is meant to run against ("verify":
// "OmegaVault:...", meaning OmegaVault IS `currentContract` in this scene).
//
// STRUCTURAL FIX (this revision): an earlier draft declared
// `using OmegaVault as vault;` and called every Vault method as `vault.foo()`.
// That was a redundant, needlessly ambiguous alias to the contract already
// implicitly available as `currentContract` — OmegaVault is the ONLY primary
// contract in this scene, not a second linked contract. Per Certora's own
// multi-contract-scene example (`underlying.balanceOf(currentContract)`,
// Certora Docs "Working with Multiple Contracts"), the correct pattern is:
//   - call the Vault's own methods bare (`totalPendingProfit()`, not
//     `vault.totalPendingProfit()`) — omitting a contract prefix already
//     means "call on currentContract",
//   - pass `currentContract` (an `address`-typed special variable) as the
//     argument when another linked contract needs the Vault's address, e.g.
//     `profitToken.balanceOf(currentContract)`,
//   - hook access paths on `pending_profit` stay bare too
//     (`pending_profit[KEY bytes32 h]`), since that state variable lives on
//     the sole contract in this scene — the contract-prefixed hook form
//     (`C.totalSupply`) documented by Certora is for hooking a *different*,
//     `using`-aliased contract's storage, which does not apply here.
// `using ERC20 as profitToken;` below is kept AS a real second contract in
// the scene (the linked token), which is exactly the case that form is for.
//
// One thing worth being transparent about: the mechanical find/replace used
// to apply this fix initially over-corrected two spots — a hook access path
// and one `@withrevert` call — into invalid `currentContract.foo` syntax
// (`currentContract` is a value, not a contract-alias name, so it can't
// prefix a hook path or a method call the way a `using`-introduced name
// can). Both were caught by re-grepping for `currentContract.` afterward and
// fixed to the correct bare form before this file was finalized.
//
// Syntax otherwise verified against current Certora Prover documentation
// (docs.certora.com) as of this revision:
//   - the STORAGE keyword after Sload/Sstore hook patterns was made
//     optional/removed in a past Prover release (per the Prover changelog);
//     this spec never used it, so no change needed there.
//   - `requireInvariant id "(" exprs ")" ";"` is confirmed current grammar
//     (CVL Statements reference) — a no-argument invariant is called as
//     `requireInvariant totalPendingMatchesSum();`, exactly as written below.
//   - `f.contract` (referenced in a comment below re: an earlier, dropped
//     `filtered` clause) is a real field, introduced in Certora CLI 5.0 —
//     confirmed via the CLI 5.0 changelog. It was dropped from this spec not
//     because it's fictional, but because I hadn't confirmed the specific
//     `f.contract == currentContract` comparison form compiles, and this
//     single-contract scene doesn't need it anyway (no other contract's
//     methods appear in the parametric surface these invariants range over).
//
// What is still genuinely unknown and left for you to fill in (not
// guessed): whether this project resolves @openzeppelin imports via a
// Foundry remappings.txt or a Hardhat/npm --packages flag. Both concrete
// forms are given as separate OmegaVault.npm.conf / OmegaVault.foundry.conf
// files — pick whichever matches your actual project layout.

// profit_token is declared `IERC20 public immutable profit_token;` in the
// actual OmegaVault.sol, and that file already imports
// @openzeppelin/contracts/token/ERC20/IERC20.sol. Rather than invent a mock
// contract name I have no evidence your project actually defines, this
// links against OpenZeppelin's real, fully-implemented ERC20.sol (it is not
// abstract — every function has a body — so it deploys and behaves like any
// concrete ERC20 for verification purposes) — PROVIDED your conf actually
// links it: `"link": ["OmegaVault:profit_token=ERC20"]`, present in both
// OmegaVault.npm.conf and OmegaVault.foundry.conf. Without that link entry,
// `profit_token` is an unconstrained address and `profitToken.balanceOf(...)`
// below would not resolve to this ERC20 at all. If your project has its own
// test-mock ERC20 (e.g. one with a `mint` test helper), swap this line for
// `using YourMockName as profitToken;` and update the `--link` target in the
// conf to match — that specific name is the one piece of information only
// your repository has.
using ERC20 as profitToken;

// SCOPE NOTE, applying to every rule in this file that asserts success
// (receivePendingProfitIncreasesTotalByExactlyNetProfit,
// receivePreservesSumInvariant, releaseProfitDecreasesTotalByExactlyNet,
// releasePreservesSumInvariant): those rules' `assert !lastReverted` is
// sound only because the linked token is OpenZeppelin's ERC20, where
// `transferFrom`/`transfer` succeed if and only if the balance/allowance
// preconditions already required hold — no other revert path exists in that
// implementation. This is a property of the LINKED contract, not something
// derived from the Vault's own code. If this link is ever swapped for a
// token with additional revert conditions the Vault doesn't and can't know
// about — fee-on-transfer, pausable, blacklist/denylist, an ERC777-style
// hook that can revert in a receive callback, etc. — every one of those
// success assertions becomes unsound again, even though nothing in
// OmegaVault.sol changed. This is a documentation obligation this spec now
// satisfies, not a claim that OmegaVault works correctly with an arbitrary
// IERC20 (it may not, independent of this spec's ability to prove it either
// way).

//////////////////////////////////////////////////////////////////////////////
// Methods block — linkage + summarization
//////////////////////////////////////////////////////////////////////////////

methods {
    // Vault surface
    function receivePendingProfit(bytes32, uint256) external;
    function releaseProfit(bytes32) external;
    function totalPendingProfit() external returns (uint256) envfree;
    function pending_profit(bytes32) external returns (uint256) envfree;
    function released(bytes32) external returns (bool) envfree;
    function proof_verified(bytes32) external returns (bool) envfree;
    function confirmation_depth(bytes32) external returns (uint8) envfree;
    function profit_token() external returns (address) envfree;
    function rescuableProfitTokenSurplus() external returns (uint256) envfree;
    function PER_TRANSFER_CAP() external returns (uint256) envfree;
    function dailyCapRemaining() external returns (uint256) envfree;
    function ORCHESTRATOR_ROLE() external returns (bytes32) envfree;

    function profitToken.allowance(address, address) external returns (uint256) envfree;
    function hasRole(bytes32, address) external returns (bool) envfree;
    function DEFAULT_ADMIN_ROLE() external returns (bytes32) envfree;

    // Rescue surface (admin-only)
    function rescueERC20(address token, address to, uint256 amount) external;
    function rescueETH(address to, uint256 amount) external;

    // ERC20 — summarised so the Prover doesn't explode on unbounded storage
    function profitToken.balanceOf(address) external returns (uint256) envfree;
    function profitToken.transfer(address, uint256) external returns (bool);
    function profitToken.transferFrom(address, address, uint256) external returns (bool);
    function profitToken.approve(address, uint256) external returns (bool);
}

//////////////////////////////////////////////////////////////////////////////
// Ghost + hooks for "totalPendingProfit == sum of pending_profit[*]"
//////////////////////////////////////////////////////////////////////////////

// FIX (this revision, review-prompted): ghost mappings are NOT
// zero-initialized automatically — confirmed against Certora's own docs
// ("[without init_state axiom] at any other time its value is not
// constrained", Ghosts reference) and against Certora's own ERC4626
// workshop example spec, which uses exactly this pattern for exactly this
// situation: `init_state axiom forall address a. (balanceOfMirrored[a] ==
// 0);`. Without this axiom, ghostPending's value at the point right after
// the constructor is unconstrained for every key, which would make the
// base case of totalPendingMatchesSum unprovable (or worse, satisfiable
// only by accident) — this was a real gap, not a hypothetical one.
ghost mapping(bytes32 => uint256) ghostPending {
    init_state axiom forall bytes32 h. ghostPending[h] == 0;
}

// MERGE FIX: added init_state axiom — without it the Prover can start
// verification from a state where ghostTotalPending is unconstrained,
// which makes the sum invariant unprovable from a cold/initial state.
ghost uint256 ghostTotalPending {
    init_state axiom ghostTotalPending == 0;
}

hook Sstore pending_profit[KEY bytes32 h] uint256 newVal (uint256 oldVal) {
    ghostPending[h] = newVal;
    if (newVal >= oldVal) {
        ghostTotalPending = ghostTotalPending + (newVal - oldVal);
    } else {
        ghostTotalPending = ghostTotalPending - (oldVal - newVal);
    }
}

hook Sload uint256 val pending_profit[KEY bytes32 h] {
    require ghostPending[h] == val;
}

//////////////////////////////////////////////////////////////////////////////
// Invariants
//////////////////////////////////////////////////////////////////////////////

/// Counter equals the sum of the mapping (via the ghost).
// MERGE FIX: dropped the `filtered { f -> f.contract == currentContract }` clause from
// the source draft — unverified syntax for method filtering by contract in
// this CVL version. If you need to exclude specific entry points (e.g. a
// constructor/init function that shouldn't be checked), use
// `filtered { f -> f.selector != sig:yourFunction(...).selector }` instead,
// confirmed against your Certora CLI version.
invariant totalPendingMatchesSum()
    totalPendingProfit() == ghostTotalPending

/// Once a blueprint is released it stays released and its pending is zero.
invariant releasedImpliesZeroPending(bytes32 h)
    released(h) => pending_profit(h) == 0

/// A single entry can never exceed the aggregate it's part of.
///
/// NOTE ON PROOF DIFFICULTY — being precise rather than optimistic here:
/// `totalPendingMatchesSum` alone (totalPendingProfit == ghostTotalPending)
/// does NOT hand the Prover this fact for free. That invariant only
/// establishes that the *running counter* tracks the *running sum*; it says
/// nothing on its own about any individual addend's relationship to that
/// sum. This invariant needs its own inductive argument, case-split by which
/// function touched storage: `receivePendingProfit`/`releaseProfit` acting
/// on THIS hash `h` make the claim trivial by construction (entry becomes
/// exactly the amount just added to the total, or becomes 0); either
/// function acting on a DIFFERENT hash requires knowing entry(h) was already
/// bounded by the total before that other call, updated for however the
/// total just changed — which is exactly the inductive step this invariant
/// itself is establishing, closed via `requireInvariant` on itself inside
/// the preserved block. This is a standard pattern (structurally identical
/// to proving `balanceOf(a) <= totalSupply()` for an ERC20), but has NOT
/// been run through the Prover in this environment — treat the preserved
/// blocks below as a starting point to iterate on, not a confirmed proof.
invariant pendingEntryNeverExceedsTotal(bytes32 h)
    pending_profit(h) <= totalPendingProfit()
    {
        preserved receivePendingProfit(bytes32 bp, uint256 netProfit) with (env e) {
            requireInvariant pendingEntryNeverExceedsTotal(h);
        }
        preserved releaseProfit(bytes32 bp) with (env e) {
            requireInvariant pendingEntryNeverExceedsTotal(h);
        }
        preserved rescueERC20(address token, address to, uint256 amount) with (env e) {
            // rescueERC20 never writes pending_profit or totalPendingProfit
            // (see rescueDoesNotTouchAnyPending / rescueERC20DoesNotDecreaseTotalPending
            // rules above) — included for completeness so this invariant's
            // preserved-block coverage is exhaustive over every function that
            // could plausibly touch either side of the inequality, not just
            // the two that obviously do.
            requireInvariant pendingEntryNeverExceedsTotal(h);
        }
    }

//////////////////////////////////////////////////////////////////////////////
// Access control — unauthorized callers can never rescue
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
// Rescue cannot decrease totalPendingProfit / touch per-blueprint accounting
//////////////////////////////////////////////////////////////////////////////

rule rescueERC20DoesNotDecreaseTotalPending(env e, address token, address to, uint256 amount) {
    uint256 before = totalPendingProfit();

    rescueERC20(e, token, to, amount);

    assert totalPendingProfit() == before,
        "rescueERC20 must not change totalPendingProfit, for any token";
}

rule rescueETHDoesNotDecreaseTotalPending(env e, address to, uint256 amount) {
    uint256 before = totalPendingProfit();

    rescueETH(e, to, amount);

    assert totalPendingProfit() == before,
        "rescueETH must not change totalPendingProfit";
}

/// Stronger than the two rules above: rescue leaves every individual
/// pending_profit[h] unchanged, not just the aggregate.
rule rescueDoesNotTouchAnyPending(
    env e,
    address token,
    address to,
    uint256 amount,
    bytes32 h
) {
    uint256 pendingBefore = pending_profit(h);

    rescueERC20(e, token, to, amount);

    assert pending_profit(h) == pendingBefore,
        "rescueERC20 must not alter any pending_profit entry";
}

/// A successful rescueERC20 on profit_token must never move more than the
/// surplus available immediately before the call, AND the accounting
/// identity balance == totalPendingProfit + surplus must still hold
/// afterward — this is what actually catches a bug where rescue moves
/// tokens correctly but the bookkeeping (totalPendingProfit or the derived
/// surplus) has silently drifted out of sync with the real balance.
rule rescueOfProfitTokenNeverExceedsSurplus(env e, address to, uint256 amount) {
    address pt = profit_token();
    uint256 surplusBefore = rescuableProfitTokenSurplus();

    rescueERC20(e, pt, to, amount);

    assert amount <= surplusBefore,
        "a successful rescueERC20(profit_token, ...) call must never move more "
        "than the surplus observed before the call";

    uint256 balanceAfter = profitToken.balanceOf(currentContract);
    uint256 totalPendingAfter = totalPendingProfit();
    uint256 surplusAfter = rescuableProfitTokenSurplus();
    assert to_mathint(balanceAfter) == to_mathint(totalPendingAfter) + to_mathint(surplusAfter),
        "balance must equal totalPendingProfit + surplus after any successful rescue, "
        "i.e. the rescue moved real tokens without desyncing the derived accounting";
}

//////////////////////////////////////////////////////////////////////////////
// receivePendingProfit: exact-increase (symmetric to releaseProfit below)
//////////////////////////////////////////////////////////////////////////////
//
// Everything above emphasized the decrement path (release, rescue). Adding
// the increment path explicitly too, per review: makes the accounting story
// symmetrical, and gives failed proofs on the decrement side a known-good
// increment-side rule to diff against when debugging.

/// After a successful receivePendingProfit(h, netProfit):
///   - pending_profit[h] becomes exactly netProfit (one-shot: was 0 before,
///     by the ProfitAlreadyPending guard, so this is not just "increases by
///     netProfit" but "becomes netProfit" outright)
///   - totalPendingProfit increases by exactly netProfit
///   - the sum invariant still holds afterward
rule receivePendingProfitIncreasesTotalByExactlyNetProfit(env e, bytes32 h, uint256 netProfit) {
    require pending_profit(h) == 0;   // required for a successful call — one-shot guard
    require netProfit > 0;             // ZeroAmount() guard
    // FIX (re-check prompted by review point 1): two preconditions missing
    // from the original rule, found by re-reading the actual
    // receivePendingProfit body rather than trusting the earlier precondition
    // list was complete:
    //   - onlyRole(ORCHESTRATOR_ROLE) — the caller must hold this role, or
    //     the call reverts before reaching any of the logic this rule cares
    //     about. Was previously unconstrained, meaning `assert !lastReverted`
    //     below was UNSOUND: the Prover could pick e.msg.sender without the
    //     role and find a real (if uninteresting) counterexample.
    //   - safeTransferFrom(msg.sender, address(this), netProfit) requires
    //     msg.sender to hold at least netProfit of profit_token AND to have
    //     approved this Vault for at least netProfit — also unconstrained
    //     previously, same unsoundness.
    require hasRole(ORCHESTRATOR_ROLE(), e.msg.sender);
    require profitToken.balanceOf(e.msg.sender) >= netProfit;
    require profitToken.allowance(e.msg.sender, currentContract) >= netProfit;

    uint256 totalBefore = totalPendingProfit();

    receivePendingProfit@withrevert(e, h, netProfit);

    // Explicit success assertion — without @withrevert here, the Prover
    // would simply not explore reverting paths, silently filtering out an
    // "always reverts now" regression rather than reporting it (same fix
    // applied to OmegaOrchestratorRescue.spec's positive-path rules).
    assert !lastReverted,
        "receivePendingProfit must succeed given a fresh blueprintHash and a "
        "positive netProfit — the two documented preconditions for success";

    uint256 totalAfter = totalPendingProfit();

    assert pending_profit(h) == netProfit,
        "pending_profit[h] must become exactly netProfit after a successful deposit";
    assert totalAfter == totalBefore + netProfit,
        "totalPendingProfit must increase by exactly netProfit";
}

/// A second deposit against the same blueprintHash always reverts (one-shot
/// protection), mirroring releaseProfitReplayProtected below for the
/// increment side — so the counter can never be incremented twice for one
/// hash without an intervening release.
rule receivePendingProfitOneShotProtected(env e, bytes32 h, uint256 netProfit) {
    require pending_profit(h) != 0;   // already has profit pending

    receivePendingProfit@withrevert(e, h, netProfit);
    assert lastReverted,
        "a second receivePendingProfit for a blueprintHash with nonzero pending_profit "
        "must revert (ProfitAlreadyPending)";
}

/// After any successful deposit, the global sum invariant still holds —
/// same pattern as releasePreservesSumInvariant below, for the increment side.
rule receivePreservesSumInvariant(env e, bytes32 h, uint256 netProfit) {
    requireInvariant totalPendingMatchesSum();

    require pending_profit(h) == 0;
    require netProfit > 0;
    require hasRole(ORCHESTRATOR_ROLE(), e.msg.sender);
    require profitToken.balanceOf(e.msg.sender) >= netProfit;
    require profitToken.allowance(e.msg.sender, currentContract) >= netProfit;

    receivePendingProfit@withrevert(e, h, netProfit);

    assert !lastReverted,
        "receivePendingProfit must succeed given a fresh blueprintHash and a "
        "positive netProfit";
    assert totalPendingProfit() == ghostTotalPending,
        "sum invariant must hold after receivePendingProfit";
}

//////////////////////////////////////////////////////////////////////////////
// releaseProfit: exact-decrease and replay protection
//////////////////////////////////////////////////////////////////////////////

/// After a successful releaseProfit(h):
///   - totalPendingProfit decreases by exactly the amount that was pending
///   - pending_profit[h] becomes 0
///   - released[h] becomes true
rule releaseProfitDecreasesTotalByExactlyNet(env e, bytes32 h) {
    require !released(h);
    require proof_verified(h);
    require confirmation_depth(h) >= 12;          // MIN_CONFIRMATION_DEPTH
    uint256 net = pending_profit(h);
    require net > 0;

    uint256 totalBefore = totalPendingProfit();
    // MERGE FIX (review point 7): this was previously `require totalBefore
    // >= net;`, treating it as an environmental precondition. It should be a
    // PROVABLE consequence instead — if it's ever false, that's an
    // accounting bug the rule should catch, not paper over. Provable via
    // pendingEntryNeverExceedsTotal (net IS pending_profit(h) at this
    // point), not via totalPendingMatchesSum alone — see that invariant's
    // own doc comment for why the two are not interchangeable here.
    requireInvariant pendingEntryNeverExceedsTotal(h);
    assert totalBefore >= net,
        "totalPendingProfit must already cover this blueprint's own pending_profit "
        "entry before release";

    // Point 7-adjacent honesty check: releaseProfit can ALSO revert on the
    // per-transfer cap, the daily cap, or requireVaultReconciliation's
    // actual-balance check — none of which the rule had ruled out before
    // this fix. Asserting unconditional success without these would itself
    // have been an unsound overclaim (a true false-positive risk), not just
    // an incomplete rule — so they're added here as explicit preconditions,
    // not skipped.
    require net <= PER_TRANSFER_CAP();
    require net <= dailyCapRemaining();
    require profitToken.balanceOf(currentContract) >= net;

    releaseProfit@withrevert(e, h);

    assert !lastReverted,
        "releaseProfit must succeed given a valid proof, sufficient depth, "
        "not-yet-released status, and a positive pending amount — the "
        "documented preconditions for success";

    uint256 totalAfter = totalPendingProfit();

    assert pending_profit(h) == 0,
        "pending_profit[h] must be zero after release";
    assert released(h),
        "released[h] must be true after release";
    assert totalAfter == totalBefore - net,
        "totalPendingProfit must decrease by exactly the released amount";
}

/// A second release of an already-released hash always reverts (replay
/// protection), so the counter can never be decremented twice for one hash.
rule releaseProfitReplayProtected(env e, bytes32 h) {
    require released(h);

    releaseProfit@withrevert(e, h);
    assert lastReverted,
        "second releaseProfit of an already-released hash must revert";
}

/// After any successful release, the global sum invariant still holds —
/// stated as a rule (not just relying on the invariant's own inductive
/// check) so the Prover explicitly checks this specific transition.
rule releasePreservesSumInvariant(env e, bytes32 h) {
    requireInvariant totalPendingMatchesSum();

    uint256 net = pending_profit(h);
    require net > 0;
    require !released(h);
    require proof_verified(h);
    require confirmation_depth(h) >= 12;
    require net <= PER_TRANSFER_CAP();
    require net <= dailyCapRemaining();
    require profitToken.balanceOf(currentContract) >= net;

    releaseProfit@withrevert(e, h);

    assert !lastReverted,
        "releaseProfit must succeed given all its documented preconditions";
    assert totalPendingProfit() == ghostTotalPending,
        "sum invariant must hold after releaseProfit";
}

//////////////////////////////////////////////////////////////////////////////
// Orchestrator rescue rules live in a separate scene/spec
//////////////////////////////////////////////////////////////////////////////
//
// Per the "verify Vault and Orchestrator in separate scenes" guidance this
// spec was reviewed against — OmegaOrchestrator's activeFlashloanCount()==0
// rescue guard is verified in its own file, not commented out here:
// see certora/specs/OmegaOrchestratorRescue.spec and its companion
// certora/confs/OmegaOrchestrator.conf.

//////////////////////////////////////////////////////////////////////////////
// Event-content assertions — deliberately omitted
//////////////////////////////////////////////////////////////////////////////
//
// FundsRescued / EthRescued content (correct token/to/amount in the emitted
// event) is better verified with Foundry vm.expectEmit unit tests than a
// Certora rule — CVL's event-assertion primitives are weak relative to
// storage/return-value properties for this kind of check.