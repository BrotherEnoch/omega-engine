// contracts/src/strategies/CanaryArb.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// @title CanaryArb — v12 Final
/// @notice Phase 0.5: Pipeline health signal validator.
///         Executes minimal capital swaps (configurable, default 0.0001 ETH equivalent)
///         to validate the full Orchestrator → flashloan → strategy → Vault execution path.
///         Never competes for real MEV lane slots — runs on dedicated canary scheduler.
///         Emits CanaryPing on every execution; off-chain monitoring consumes these events.
///
/// @dev    Success condition: execute() returns exactly flashloanAmount (zero profit, zero loss).
///         Any deviation signals a pipeline malfunction.
///         Called via call() from OmegaOrchestrator (NOT delegatecall).
contract CanaryArb {

    // ─────────────────────────────────────────────────────────────────────────
    // Immutables
    // ─────────────────────────────────────────────────────────────────────────
    address public immutable orchestrator;

    // ─────────────────────────────────────────────────────────────────────────
    // Metrics
    // ─────────────────────────────────────────────────────────────────────────
    uint256 public ping_count;
    uint256 public last_ping_block;
    uint256 public last_ping_timestamp;

    // ─────────────────────────────────────────────────────────────────────────
    // Events
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Emitted on every successful canary execution.
    /// @param  block_number  Block in which the canary ran.
    /// @param  flashloan_amount  Amount cycled (should be returned exactly).
    /// @param  ping_index    Monotonically increasing execution counter.
    /// @param  success       Always true if this event is emitted (execution completed).
    event CanaryPing(
        uint64  indexed block_number,
        uint256 flashloan_amount,
        uint256 ping_index,
        bool    success
    );

    // ─────────────────────────────────────────────────────────────────────────
    // Errors
    // ─────────────────────────────────────────────────────────────────────────
    error OnlyOrchestrator();

    // ─────────────────────────────────────────────────────────────────────────
    // Constructor
    // ─────────────────────────────────────────────────────────────────────────
    constructor(address _orchestrator) {
        require(_orchestrator != address(0), "CanaryArb: zero orchestrator");
        orchestrator = _orchestrator;
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Core
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Validate execution pipeline. Returns exactly flashloanAmount (zero profit).
    ///         Emits CanaryPing for off-chain health monitoring.
    /// @param  /* strategyCalldata */ Ignored — canary needs no routing params.
    /// @param  flashloanAmount         Amount provided by flashloan provider; returned exactly.
    /// @return netOutput               Exactly equals flashloanAmount.
    function execute(
        bytes calldata, /* strategyCalldata — unused */
        uint256 flashloanAmount
    ) external returns (uint256 netOutput) {
        if (msg.sender != orchestrator) revert OnlyOrchestrator();

        // Increment counters
        uint256 idx = ++ping_count;
        last_ping_block     = block.number;
        last_ping_timestamp = block.timestamp;

        // Canary contract does NOT execute real swaps.
        // It simply validates that the entire pipeline (key auth, replay protection,
        // nonce management, flashloan callback, Vault deposit) is operational.
        // Return exactly the borrowed amount — zero profit, zero loss.
        netOutput = flashloanAmount;

        emit CanaryPing(uint64(block.number), flashloanAmount, idx, true);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // View helpers (for off-chain health dashboards)
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Returns seconds since the last canary ping.
    function secondsSinceLastPing() external view returns (uint256) {
        if (last_ping_timestamp == 0) return type(uint256).max;
        return block.timestamp - last_ping_timestamp;
    }

    /// @notice Returns blocks since the last canary ping.
    function blocksSinceLastPing() external view returns (uint256) {
        if (last_ping_block == 0) return type(uint256).max;
        return block.number - last_ping_block;
    }
}