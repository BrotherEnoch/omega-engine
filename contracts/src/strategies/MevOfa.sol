// contracts/src/strategies/MevOfa.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @title MevOfa — v12 Final
/// @notice Phase 4: MEV-OFA (Order Flow Agreement) backrunning strategy.
///         Backruns user transactions included in the same bundle.
///         OFA compliance enforced off-chain by the L4 Security layer.
///         Builder blacklist enforced at the relay/bundle-submission layer — NOT on-chain.
///         Called via call() from OmegaOrchestrator (NOT delegatecall).
///
/// @dev    Calldata layout (ABI-encoded):
///           bytes32  user_tx_hash     — hash of the user tx being backrun (for audit)
///           address  pool             — DEX pool where price impact occurred
///           address  token_in         — token to sell into impact (restores price)
///           address  token_out        — token received
///           uint256  amount_in        — amount to swap
///           uint256  min_profit       — minimum net profit required
///           uint256  max_slippage_bps — maximum allowed slippage (e.g. 50 = 0.5%)
///
///         Flow: flashloan(token_in) → backrun swap (captures price impact)
///               → swap back → repay flashloan → profit to Orchestrator
///
///         Adverse selection guard: if estimated_price_impact < threshold, execution reverts.
///         This prevents executing on stale or miscomputed price impact signals.
contract MevOfa {
    using SafeERC20 for IERC20;

    // ─────────────────────────────────────────────────────────────────────────
    // Immutables
    // ─────────────────────────────────────────────────────────────────────────
    address public immutable orchestrator;

    // Minimum price impact in bps to proceed (adverse selection filter)
    // Off-chain detector computes impact; this is the on-chain floor
    uint256 public immutable MIN_PRICE_IMPACT_BPS;

    // ─────────────────────────────────────────────────────────────────────────
    // Events
    // ─────────────────────────────────────────────────────────────────────────
    event BackrunExecuted(
        bytes32 indexed userTxHash,
        address indexed pool,
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 netProfit
    );
    event BackrunSkipped(
        bytes32 indexed userTxHash,
        string reason
    );

    // ─────────────────────────────────────────────────────────────────────────
    // Errors
    // ─────────────────────────────────────────────────────────────────────────
    error OnlyOrchestrator();
    error InsufficientProfit(uint256 actual, uint256 minimum);
    error PriceImpactTooLow(uint256 actual, uint256 minimum);
    error SlippageExceeded(uint256 actual, uint256 maximum);
    error BackrunFailed();
    error ZeroAddress();
    error InvalidCalldata();

    // ─────────────────────────────────────────────────────────────────────────
    // Constructor
    // ─────────────────────────────────────────────────────────────────────────
    constructor(address _orchestrator, uint256 _minPriceImpactBps) {
        if (_orchestrator == address(0)) revert ZeroAddress();
        orchestrator          = _orchestrator;
        MIN_PRICE_IMPACT_BPS  = _minPriceImpactBps;
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Modifier
    // ─────────────────────────────────────────────────────────────────────────
    modifier onlyOrchestrator() {
        if (msg.sender != orchestrator) revert OnlyOrchestrator();
        _;
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Core
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Execute a MEV backrun.
    /// @param  strategyCalldata ABI-encoded params (see layout above).
    /// @param  flashloanAmount  Amount of token_in received from flashloan provider.
    /// @return netOutput        token_in amount returned (flashloanAmount + profit).
    function execute(
        bytes calldata strategyCalldata,
        uint256 flashloanAmount
    ) external onlyOrchestrator returns (uint256 netOutput) {
        if (strategyCalldata.length == 0) revert InvalidCalldata();

        (
            bytes32 user_tx_hash,
            address pool,
            address token_in,
            address token_out,
            uint256 amount_in,
            uint256 min_profit,
            uint256 max_slippage_bps,
            uint256 estimated_price_impact_bps
        ) = abi.decode(
            strategyCalldata,
            (bytes32, address, address, address, uint256, uint256, uint256, uint256)
        );

        if (pool == address(0) || token_in == address(0) || token_out == address(0))
            revert ZeroAddress();

        // Adverse selection guard: reject if estimated price impact is too low
        if (estimated_price_impact_bps < MIN_PRICE_IMPACT_BPS) {
            emit BackrunSkipped(user_tx_hash, "price_impact_below_threshold");
            revert PriceImpactTooLow(estimated_price_impact_bps, MIN_PRICE_IMPACT_BPS);
        }

        // Record pre-swap balance to measure actual output
        uint256 tokenOutBefore = IERC20(token_out).balanceOf(address(this));

        // Leg 1: swap token_in → token_out (capture price impact from user tx)
        IERC20(token_in).safeApprove(pool, 0);
        IERC20(token_in).safeApprove(pool, amount_in);

        // FIX: decode return value was unused — removed assignment, result checked via balance delta
        (bool leg1Ok,) = pool.call(
            abi.encodeWithSignature(
                "swap(address,address,uint256,uint256,address)",
                token_in, token_out, amount_in, 0, address(this)
            )
        );
        if (!leg1Ok) revert BackrunFailed();

        // Slippage check on leg 1 — measured via balance delta (source of truth)
        uint256 tokenOutAfter = IERC20(token_out).balanceOf(address(this));
        uint256 actualOut     = tokenOutAfter - tokenOutBefore;
        uint256 minOut        = amount_in * (10_000 - max_slippage_bps) / 10_000;
        if (actualOut < minOut)
            revert SlippageExceeded(actualOut, minOut);

        // Leg 2: swap token_out → token_in (close the position)
        uint256 tokenInBefore = IERC20(token_in).balanceOf(address(this));

        IERC20(token_out).safeApprove(pool, 0);
        IERC20(token_out).safeApprove(pool, actualOut);

        (bool leg2Ok,) = pool.call(
            abi.encodeWithSignature(
                "swap(address,address,uint256,uint256,address)",
                token_out, token_in, actualOut, 0, address(this)
            )
        );
        if (!leg2Ok) revert BackrunFailed();

        uint256 finalTokenIn = IERC20(token_in).balanceOf(address(this)) - tokenInBefore;

        // Profit check
        if (finalTokenIn <= flashloanAmount)
            revert InsufficientProfit(0, min_profit);
        uint256 profit = finalTokenIn - flashloanAmount;
        if (profit < min_profit)
            revert InsufficientProfit(profit, min_profit);

        netOutput = finalTokenIn;

        emit BackrunExecuted(user_tx_hash, pool, token_in, token_out, amount_in, profit);
    }
}
