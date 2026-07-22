// contracts/src/strategies/SimpleArb.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @title SimpleArb — v13 Final
/// @notice Phase 1: Stateless single-hop 2-DEX arbitrage strategy.
///         Called via call() from OmegaOrchestrator (NOT delegatecall).
///         Zero retained state — all assets flow through in a single transaction.
///
/// @dev    Calldata layout (ABI-encoded):
///           address pool_a        — source DEX adapter/pool
///           address pool_b        — destination DEX adapter/pool
///           address token_in      — token to borrow via flashloan
///           address token_out     — intermediate token
///           uint256 amount_in     — exact input amount
///           uint256 min_profit    — minimum net profit required (in token_in)
///
///         Flow: flashloan(token_in) -> swap A (token_in -> token_out)
///               -> swap B (token_out -> token_in) -> repay -> profit to Orchestrator
///
///         IMPORTANT — interface assumption: `pool_a` / `pool_b` MUST be contracts that
///         implement `swap(address,address,uint256,uint256,address)` (this is NOT the
///         real Uniswap V2 pair interface, which uses amount0Out/amount1Out + a separate
///         transfer-in). In practice this means pool_a/pool_b must be your own router/adapter
///         contracts, never a raw third-party pool address. This was true in the prior version
///         too; it is called out explicitly here so it isn't a silent assumption.
///
/// CHANGES vs prior version:
///   - safeApprove -> forceApprove (safeApprove does not exist in OpenZeppelin 5.x;
///     the prior file would not compile against the pinned OZ version).
///   - abi.decode on swap return data now checks length first, so a misbehaving adapter
///     returning no data reverts with a named error instead of a raw decode panic.
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
    error MalformedSwapReturnData(uint8 leg);
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

        // Leg 1: swap token_in -> token_out on pool_a
        uint256 intermediateOut = _swap(pool_a, token_in, token_out, amount_in, 1);

        // Leg 2: swap token_out -> token_in on pool_b
        uint256 finalOut = _swap(pool_b, token_out, token_in, intermediateOut, 2);

        // Profit check
        if (finalOut <= flashloanAmount) revert InsufficientProfit(0, min_profit);
        uint256 profit = finalOut - flashloanAmount;
        if (profit < min_profit) revert InsufficientProfit(profit, min_profit);

        netOutput = finalOut;

        // Send the flashloaned token (plus profit) back to the caller (the Orchestrator)
        // so it can repay the flashloan provider. This contract never retains custody past
        // the end of this call.
        IERC20(token_in).safeTransfer(msg.sender, finalOut);

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
        IERC20(tokenIn).forceApprove(pool, 0);
        IERC20(tokenIn).forceApprove(pool, amountIn);

        (bool success, bytes memory result) = pool.call(
            abi.encodeWithSignature(
                "swap(address,address,uint256,uint256,address)",
                tokenIn,
                tokenOut,
                amountIn,
                0,           // min amount out — enforced by our profit check, not here
                address(this)
            )
        );
        if (!success) revert SwapFailed(leg);
        if (result.length < 32) revert MalformedSwapReturnData(leg);
        amountOut = abi.decode(result, (uint256));
    }
}