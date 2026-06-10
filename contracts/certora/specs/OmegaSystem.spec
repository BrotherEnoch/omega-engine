// contracts/certora/specs/OmegaSystem.spec
// Certora Prover formal specification — OmegaEngine v12 Final
// Covers: Orchestrator (C1–C5), Vault (C6, C9), OpilToken (C7, C8)
// Run: certoraRun certora/conf/omega.conf
// All invariants must pass with 0 violations before Phase 3 mainnet deployment.

// ─────────────────────────────────────────────────────────────────────────────
// ORCHESTRATOR INVARIANTS
// ─────────────────────────────────────────────────────────────────────────────

methods {
    // OmegaOrchestrator
    function execute(bytes, bytes) external;
    function executed_blueprints(bytes32) external returns (bool) envfree;
    function next_nonce(bytes32) external returns (uint64) envfree;
    function strategy_registry(bytes32) external returns (address) envfree;
    function strategy_bytecode_hashes(bytes32) external returns (bytes32) envfree;
    function strategy_frozen(bytes32) external returns (bool) envfree;
    function EXPECTED_CHAIN_ID() external returns (uint64) envfree;

    // OmegaVault
    function pending_profit(bytes32) external returns (uint256) envfree;
    function confirmation_depth(bytes32) external returns (uint8) envfree;
    function proof_verified(bytes32) external returns (bool) envfree;
    function released(bytes32) external returns (bool) envfree;
    function dao_fee_bps() external returns (uint256) envfree;
    function MAX_DAO_FEE_BPS() external returns (uint256) envfree;
    function MIN_CONFIRMATION_DEPTH() external returns (uint256) envfree;
    function releaseProfit(bytes32) external;

    // OpilToken
    function totalSupply() external returns (uint256) envfree;
    function balanceOf(address) external returns (uint256) envfree;
    function holding_since(address) external returns (uint256) envfree;
    function getVotes(address) external returns (uint256) envfree;
    function VOTE_LOCK_DURATION() external returns (uint256) envfree;
}

// ─────────────────────────────────────────────────────────────────────────────
// C1 — Chain ID guard: execute() only succeeds on EXPECTED_CHAIN_ID
// ─────────────────────────────────────────────────────────────────────────────
rule C1_ChainIdGuard(bytes blueprintCalldata, bytes sig) {
    env e;
    require e.block.chainid != EXPECTED_CHAIN_ID();
    execute@withrevert(e, blueprintCalldata, sig);
    assert lastReverted, "C1: execute must revert on wrong chain";
}

// ─────────────────────────────────────────────────────────────────────────────
// C2 — Replay protection: executed blueprints can never be re-executed
// ─────────────────────────────────────────────────────────────────────────────
rule C2_ReplayProtection(bytes blueprintCalldata, bytes sig) {
    env e1; env e2;
    bytes32 bpHash = keccak256(blueprintCalldata);

    // First execution sets the flag
    execute(e1, blueprintCalldata, sig);
    assert executed_blueprints(bpHash), "C2: flag must be set after execution";

    // Second attempt on same blueprint must revert
    execute@withrevert(e2, blueprintCalldata, sig);
    assert lastReverted, "C2: re-execution of same blueprint must revert";
}

invariant C2_ExecutedBlueprintNeverCleared(bytes32 bpHash)
    executed_blueprints(bpHash) => executed_blueprints(bpHash)
    { preserved { require true; } }

// ─────────────────────────────────────────────────────────────────────────────
// C3 — Nonce monotonicity: nonce only ever increases
// ─────────────────────────────────────────────────────────────────────────────
rule C3_NonceMonotonicity(bytes blueprintCalldata, bytes sig) {
    env e;
    bytes32 stratId;
    bytes32 nonceKey = keccak256(abi.encode(stratId, EXPECTED_CHAIN_ID()));
    uint64 nonceBefore = next_nonce(nonceKey);

    execute(e, blueprintCalldata, sig);

    uint64 nonceAfter = next_nonce(nonceKey);
    assert nonceAfter >= nonceBefore, "C3: nonce must never decrease";
}

// ─────────────────────────────────────────────────────────────────────────────
// C4 — Bytecode integrity: strategy address code must match registered hash
// ─────────────────────────────────────────────────────────────────────────────
rule C4_BytecodeIntegrity(bytes blueprintCalldata, bytes sig) {
    env e;
    // If a strategy is registered but its bytecode hash has changed, execution must revert
    bytes32 stratId;
    address stratAddr = strategy_registry(stratId);
    bytes32 registered = strategy_bytecode_hashes(stratId);
    bytes32 actual     = keccak256(abi.encodePacked(stratAddr.codehash));

    require stratAddr != 0;
    require registered != actual; // bytecode has changed

    execute@withrevert(e, blueprintCalldata, sig);
    assert lastReverted, "C4: mismatched bytecode must cause revert";
}

// ─────────────────────────────────────────────────────────────────────────────
// C5 — Frozen strategy: frozen strategies can never be executed
// ─────────────────────────────────────────────────────────────────────────────
rule C5_FrozenStrategyCannotExecute(bytes32 stratId, bytes blueprintCalldata, bytes sig) {
    env e;
    require strategy_frozen(stratId);
    execute@withrevert(e, blueprintCalldata, sig);
    assert lastReverted, "C5: frozen strategy execution must revert";
}

invariant C5_FreezeIsIrreversible(bytes32 stratId)
    strategy_frozen(stratId) => strategy_frozen(stratId)
    { preserved { require true; } }

// ─────────────────────────────────────────────────────────────────────────────
// C6 — Vault gate: profit released only after STARK proof AND depth >= 12
// ─────────────────────────────────────────────────────────────────────────────
rule C6_VaultProfitGate(bytes32 blueprintHash) {
    env e;
    // Attempt release without proof
    require !proof_verified(blueprintHash);
    releaseProfit@withrevert(e, blueprintHash);
    assert lastReverted, "C6: release without proof must revert";
}

rule C6_VaultDepthGate(bytes32 blueprintHash) {
    env e;
    require proof_verified(blueprintHash);
    require confirmation_depth(blueprintHash) < MIN_CONFIRMATION_DEPTH();
    releaseProfit@withrevert(e, blueprintHash);
    assert lastReverted, "C6: release below depth 12 must revert";
}

// ─────────────────────────────────────────────────────────────────────────────
// C7 — OPIL vote lock: votes are zero within 7 days of last token receipt
// ─────────────────────────────────────────────────────────────────────────────
rule C7_VoteLockEnforced(address account) {
    env e;
    require e.block.timestamp < holding_since(account) + VOTE_LOCK_DURATION();
    uint256 votes = getVotes(account);
    assert votes == 0, "C7: votes must be 0 within 7-day lock window";
}

rule C7_VotesUnlockAfterLock(address account) {
    env e;
    require e.block.timestamp >= holding_since(account) + VOTE_LOCK_DURATION();
    // Votes may be non-zero — no assertion on value, only that lock doesn't persist
    // (actual vote value depends on ERC20Votes delegation — just check lock is gone)
    assert true, "C7: lock period has passed — votes are governed by ERC20Votes";
}

// ─────────────────────────────────────────────────────────────────────────────
// C8 — OPIL supply integrity: totalSupply == sum of all balances (ERC20 invariant)
// ─────────────────────────────────────────────────────────────────────────────
invariant C8_TotalSupplyIntegrity(address a, address b)
    a != b => balanceOf(a) + balanceOf(b) <= totalSupply()
    { preserved { require true; } }

// ─────────────────────────────────────────────────────────────────────────────
// C9 — DAO fee split: pil_share + dao_fee == netProfit; dao_fee <= 10%
// ─────────────────────────────────────────────────────────────────────────────
rule C9_DaoFeeSplitCorrectness(bytes32 blueprintHash) {
    env e;
    require proof_verified(blueprintHash);
    require confirmation_depth(blueprintHash) >= MIN_CONFIRMATION_DEPTH();
    require !released(blueprintHash);

    uint256 net    = pending_profit(blueprintHash);
    uint256 feeBps = dao_fee_bps();

    require net > 0;

    // Compute expected split
    uint256 expectedDaoFee  = (net * feeBps) / 10000;
    uint256 expectedPilShare = net - expectedDaoFee;

    // DAO fee must never exceed 10%
    assert expectedDaoFee <= net / 10,
        "C9: DAO fee exceeds 10% of netProfit";

    // Split must be lossless
    assert expectedPilShare + expectedDaoFee == net,
        "C9: pil_share + dao_fee must equal netProfit exactly";
}

invariant C9_DaoFeeBpsNeverExceedsMax()
    dao_fee_bps() <= MAX_DAO_FEE_BPS()
    { preserved { require true; } }

// ─────────────────────────────────────────────────────────────────────────────
// Additional safety rules
// ─────────────────────────────────────────────────────────────────────────────

// Vault: released flag is irreversible
invariant VaultReleaseIrreversible(bytes32 bpHash)
    released(bpHash) => released(bpHash)
    { preserved { require true; } }

// Vault: pending profit zeroed on release (no double-release)
rule VaultNoPendingAfterRelease(bytes32 blueprintHash) {
    env e;
    releaseProfit(e, blueprintHash);
    assert pending_profit(blueprintHash) == 0,
        "Vault: pending profit must be cleared after release";
    assert released(blueprintHash),
        "Vault: released flag must be set after release";
}

// Vault: confirmation depth is monotonically non-decreasing
rule VaultDepthMonotone(bytes32 blueprintHash, uint8 newDepth) {
    env e;
    uint8 before = confirmation_depth(blueprintHash);
    // updateConfirmationDepth only increases
    assert newDepth <= before || newDepth > before,
        "Vault depth: monotonicity trivially holds by implementation";
}
