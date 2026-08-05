// contracts/certora/specs/OmegaSystem.spec
//
// Orchestrator (C1-C5) + OpilToken (C7-C8) system-level rules.
//
// SCOPE CORRECTION (this revision): the previous version of this file also
// contained Vault C6/C9 rules duplicating VaultCore.spec, plus several rules
// that verify nothing despite looking well-formed. Both problems fixed here.
// Per this project's own spec-ownership principle (one rule, one home), the
// Vault-specific rules are REMOVED from this file entirely rather than
// fixed-in-place -- VaultCore.spec already owns C6/C9 correctly.
//
// Real bugs found and fixed in this revision, from actually reading this
// file's content for the first time (it had previously only been described
// secondhand, and that description turned out to be inaccurate about WHICH
// file had the problems, not about whether the problems existed):
//   - C2's replay hash was `keccak256(blueprintCalldata)` -- the real
//     contract uses the domain-separated
//     `keccak256(abi.encode(address(this), EXPECTED_CHAIN_ID, blueprintCalldata))`.
//   - C4's bytecode check re-hashed `stratAddr.codehash` via
//     `keccak256(abi.encodePacked(...))` -- the real contract compares
//     `stratAddr.codehash` DIRECTLY against the stored expected hash.
//   - Three invariants were syntactic tautologies (`X => X`), true
//     regardless of contract behavior: C2_ExecutedBlueprintNeverCleared,
//     C5_FreezeIsIrreversible (old version), VaultReleaseIrreversible.
//     C5_FreezeIsIrreversible is rewritten below as a real parametric rule;
//     the other two are removed (the Vault one with the rest of the Vault
//     rules; the Orchestrator one replaced by the fixed C5 rule below).
//   - C7_VotesUnlockAfterLock ended in a bare `assert true`.
//   - VaultDepthMonotone's own assertion was true by trichotomy for ANY two
//     values and never called the function under test -- removed with the
//     Vault rules (the real version already exists correctly as
//     depthNeverDecreases in VaultCore.spec).
//   - VaultNoPendingAfterRelease called releaseProfit with zero
//     preconditions and no @withrevert -- removed with the Vault rules
//     (releaseClearsPendingAndMarksReleased in VaultCore.spec is the
//     precondition-complete version of this same property).
//   - C8's pairwise `balanceOf(a) + balanceOf(b) <= totalSupply()` only
//     ever checks two holders at a time -- replaced with a proper ghost-sum
//     invariant, same pattern already used for totalPendingProfit in
//     OmegaVaultRescueAndPending.spec.
//
// NOT run against the Certora Prover in this environment -- same caveat as
// every other spec in this project's history.

using OpilToken as opil;

methods {
    // OmegaOrchestrator (currentContract in this scene)
    function execute(bytes, bytes) external;
    function executed_blueprints(bytes32) external returns (bool) envfree;
    function next_nonce(bytes32) external returns (uint64) envfree;
    function strategy_registry(bytes32) external returns (address) envfree;
    function strategy_bytecode_hashes(bytes32) external returns (bytes32) envfree;
    function strategy_frozen(bytes32) external returns (bool) envfree;
    function EXPECTED_CHAIN_ID() external returns (uint64) envfree;

    // OpilToken
    function opil.totalSupply() external returns (uint256) envfree;
    function opil.balanceOf(address) external returns (uint256) envfree;
    function opil.holding_since(address) external returns (uint256) envfree;
    function opil.getVotes(address) external returns (uint256) envfree;
    function opil.VOTE_LOCK_DURATION() external returns (uint256) envfree;
}

//////////////////////////////////////////////////////////////////////////////
// C1 — Chain ID guard
//////////////////////////////////////////////////////////////////////////////

rule C1_ChainIdGuard(env e, bytes blueprintCalldata, bytes sig) {
    require e.block.chainid != EXPECTED_CHAIN_ID();

    execute@withrevert(e, blueprintCalldata, sig);

    assert lastReverted, "C1: execute must revert on the wrong chain";
}

//////////////////////////////////////////////////////////////////////////////
// C2 — Replay protection
//////////////////////////////////////////////////////////////////////////////

rule C2_ReplayProtection(env e1, env e2, bytes blueprintCalldata, bytes sig) {
    // FIX: domain-separated hash, matching OmegaOrchestrator.sol's own
    // documented formula exactly. `keccak256(blueprintCalldata)` alone was
    // the wrong key and would have checked a mapping slot the contract
    // never actually writes to.
    bytes32 bpHash = keccak256(abi.encode(currentContract, EXPECTED_CHAIN_ID(), blueprintCalldata));

    execute(e1, blueprintCalldata, sig);
    assert executed_blueprints(bpHash),
        "C2: executed_blueprints must be set for the domain-separated hash after a successful execute";

    execute@withrevert(e2, blueprintCalldata, sig);
    assert lastReverted,
        "C2: re-executing identical blueprint calldata must revert";
}

//////////////////////////////////////////////////////////////////////////////
// C3 — Nonce monotonicity
//////////////////////////////////////////////////////////////////////////////

rule C3_NonceMonotonicity(env e, bytes32 strategyId, bytes blueprintCalldata, bytes sig) {
    bytes32 nonceKey = keccak256(abi.encode(strategyId, EXPECTED_CHAIN_ID()));
    uint64 nonceBefore = next_nonce(nonceKey);

    execute(e, blueprintCalldata, sig);

    uint64 nonceAfter = next_nonce(nonceKey);
    assert nonceAfter >= nonceBefore,
        "C3: a strategy's nonce must never decrease after any successful execute";
}

//////////////////////////////////////////////////////////////////////////////
// C4 — Bytecode integrity
//////////////////////////////////////////////////////////////////////////////

rule C4_BytecodeIntegrity(env e, bytes32 strategyId, bytes blueprintCalldata, bytes sig) {
    address stratAddr = strategy_registry(strategyId);
    bytes32 registered = strategy_bytecode_hashes(strategyId);

    require stratAddr != 0;
    // FIX: compare stratAddr.codehash DIRECTLY against the registered hash.
    // The old version re-hashed via
    // keccak256(abi.encodePacked(stratAddr.codehash)) -- doesn't correspond
    // to anything the real contract computes (`if (stratAddr.codehash !=
    // expectedHash) revert BytecodeMismatch(...)` is a direct bytes32
    // comparison, no extra hashing step).
    require stratAddr.codehash != registered;

    execute@withrevert(e, blueprintCalldata, sig);

    assert lastReverted,
        "C4: execute must revert when a registered strategy's on-chain codehash "
        "no longer matches the hash recorded at registration time";
}

//////////////////////////////////////////////////////////////////////////////
// C5 — Frozen strategy
//////////////////////////////////////////////////////////////////////////////

rule C5_FrozenStrategyCannotExecute(env e, bytes32 strategyId, bytes blueprintCalldata, bytes sig) {
    require strategy_frozen(strategyId);

    execute@withrevert(e, blueprintCalldata, sig);

    assert lastReverted,
        "C5: execute must revert whenever the decoded strategyId is frozen";
}

/// Freeze is one-directional: once true for a given strategyId, no reachable
/// call can ever set it back to false. Stated as a real parametric rule over
/// every method (`method f`), not the `X => X` tautology the old version of
/// this file had -- that proved nothing regardless of what freezeStrategy,
/// or any other function, actually did.
rule C5_FreezeIsIrreversible(bytes32 strategyId, method f) {
    require strategy_frozen(strategyId);

    env e;
    calldataarg args;
    f(e, args);

    assert strategy_frozen(strategyId),
        "C5: no method may ever clear strategy_frozen once it has been set";
}

//////////////////////////////////////////////////////////////////////////////
// C7 — OPIL vote lock
//////////////////////////////////////////////////////////////////////////////

rule C7_VoteLockEnforced(env e, address account) {
    require e.block.timestamp < opil.holding_since(account) + opil.VOTE_LOCK_DURATION();

    assert opil.getVotes(e, account) == 0,
        "C7: votes must be zero while still inside the 7-day weighted-average lock window";
}

//////////////////////////////////////////////////////////////////////////////
// C8 — OPIL supply integrity (ghost sum, not the weak pairwise version)
//////////////////////////////////////////////////////////////////////////////
//
// FIX: the old version only checked `balanceOf(a) + balanceOf(b) <=
// totalSupply()` for two arbitrary holders -- true but weak, since it says
// nothing about the sum over every actual holder. Same ghost-mapping +
// Sstore-hook pattern already used for totalPendingProfit in
// OmegaVaultRescueAndPending.spec.
//
// UNVERIFIED DETAIL, flagged rather than silently assumed: this hooks
// `opil._balances`, OpenZeppelin ERC20's standard internal balance mapping
// name across the 4.x line as far as I'm aware -- but I have not directly
// confirmed this against your specific installed lib/openzeppelin-contracts
// source the way other claims in this project have been checked. If the
// Prover reports this hook doesn't resolve, check the actual field name in
// lib/openzeppelin-contracts/contracts/token/ERC20/ERC20.sol first.

ghost mathint sumOpilBalances {
    init_state axiom sumOpilBalances == 0;
}

hook Sstore opil._balances[KEY address a] uint256 newVal (uint256 oldVal) {
    sumOpilBalances = sumOpilBalances - oldVal + newVal;
}

invariant C8_TotalSupplyEqualsSumOfBalances()
    to_mathint(opil.totalSupply()) == sumOpilBalances;
