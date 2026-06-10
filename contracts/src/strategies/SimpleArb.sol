// contracts/src/strategies/SimpleArb.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @title SimpleArb — v12 Final
/// @notice Phase 1: Stateless single-hop 2-DEX arbitrage strategy.
///         Called via call() from OmegaOrchestrator (NOT delegatecall).
///         Zero retained state — all assets flow through in a single transaction.
///
/// @dev    Calldata layout (ABI-encoded):
///           address pool_a        — source DEX pool
///           address pool_b        — destination DEX pool
///           address token_in      — token to borrow via flashloan
///           address token_out     — intermediate token
///           uint256 amount_in     — exact input amount
///           uint256 min_profit    — minimum net profit required (in token_in)
///
///         Flow: flashloan(token_in) → swap A (token_in → token_out)
///               → swap B (token_out → token_in) → repay → profit to Orchestrator
contract SimpleArb {
    using SafeERC20 for IERC20;

    // ─────────────────────────────────────────────────────────────────────────
    // Immutables
    // ─────────────────────────────────────────────────────────────────────────
    address public immutable orchestrator;

    // ─────────────────────────────────────────────────────────────────────────
    // Events
    // ─────────────────────────────────────────────────────────────────────────
    event ArbExecuted(
        address indexed pool_a,
        address indexed pool_b,
        address token_in,
        address token_out,
        uint256 amount_in,
        uint256 netProfit
    );

    // ─────────────────────────────────────────────────────────────────────────
    // Errors
    // ─────────────────────────────────────────────────────────────────────────
    error OnlyOrchestrator();
    error InsufficientProfit(uint256 actual, uint256 minimum);
    error SwapFailed(uint8 leg);
    error ZeroAddress();
    error InvalidCalldata();

    // ─────────────────────────────────────────────────────────────────────────
    // Constructor
    // ─────────────────────────────────────────────────────────────────────────
    constructor(address _orchestrator) {
        if (_orchestrator == address(0)) revert ZeroAddress();
        orchestrator = _orchestrator;
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Modifiers
    // ─────────────────────────────────────────────────────────────────────────
    modifier onlyOrchestrator() {
        if (msg.sender != orchestrator) revert OnlyOrchestrator();
        _;
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Core
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Execute single-hop 2-DEX arbitrage.
    /// @param  strategyCalldata ABI-encoded (pool_a, pool_b, token_in, token_out,
    ///                          amount_in, min_profit).
    /// @param  flashloanAmount  Amount of token_in received from flashloan provider.
    /// @return netOutput        Amount of token_in returned (flashloanAmount + profit).
    function execute(
        bytes calldata strategyCalldata,
        uint256 flashloanAmount
    ) external onlyOrchestrator returns (uint256 netOutput) {
        if (strategyCalldata.length < 6 * 32) revert InvalidCalldata();

        (
            address pool_a,
            address pool_b,
            address token_in,
            address token_out,
            uint256 amount_in,
            uint256 min_profit
        ) = abi.decode(strategyCalldata, (address, address, address, address, uint256, uint256));

        if (pool_a == address(0) || pool_b == address(0) ||
            token_in == address(0) || token_out == address(0)) revert ZeroAddress();

        // Leg 1: swap token_in → token_out on pool_a
        uint256 intermediateOut = _swap(
            pool_a,
            token_in,
            token_out,
            amount_in,
            1
        );

        // Leg 2: swap token_out → token_in on pool_b
        uint256 finalOut = _swap(
            pool_b,
            token_out,
            token_in,
            intermediateOut,
            2
        );

        // Profit check
        if (finalOut <= flashloanAmount) revert InsufficientProfit(0, min_profit);
        uint256 profit = finalOut - flashloanAmount;
        if (profit < min_profit) revert InsufficientProfit(profit, min_profit);

        netOutput = finalOut;

        emit ArbExecuted(pool_a, pool_b, token_in, token_out, amount_in, profit);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Internal swap dispatcher
    // ─────────────────────────────────────────────────────────────────────────

    function _swap(
        address pool,
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint8   leg
    ) internal returns (uint256 amountOut) {
        // Approve pool to pull tokenIn
        IERC20(tokenIn).safeApprove(pool, 0);
        IERC20(tokenIn).safeApprove(pool, amountIn);

        // Generic UniV2-style swap interface.
        // Production: dispatch per DEX type via pool registry or interface detection.
        (bool success, bytes memory result) = pool.call(
            abi.encodeWithSignature(
                "swap(address,address,uint256,uint256,address)",
                tokenIn,
                tokenOut,
                amountIn,
                0,           // min amount out — enforced by our profit check
                address(this)
            )
        );
        if (!success) revert SwapFailed(leg);
        amountOut = abi.decode(result, (uint256));
    }
}