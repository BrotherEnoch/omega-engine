// contracts/src/strategies/MevOfa.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @title MevOfa — v13 Final
/// @notice Phase 4: MEV-OFA (Order Flow Agreement) backrunning strategy.
///         Backruns user transactions included in the same bundle.
///         OFA compliance enforced off-chain by the L4 Security layer.
///         Builder blacklist enforced at the relay/bundle-submission layer — NOT on-chain.
///         Called via call() from OmegaOrchestrator (NOT delegatecall).
///
/// @dev    Calldata layout (ABI-encoded):
///           bytes32  user_tx_hash               — hash of the user tx being backrun (for audit)
///           address  pool                       — DEX pool where price impact occurred
///           address  token_in                   — token to sell into impact (restores price)
///           address  token_out                  — token received
///           uint256  amount_in                  — amount to swap
///           uint256  min_profit                 — minimum net profit required
///           uint256  max_slippage_bps            — maximum allowed slippage (e.g. 50 = 0.5%)
///           uint256  estimated_price_impact_bps  — off-chain-computed price impact estimate
///
///         Flow: flashloan(token_in) -> backrun swap (captures price impact)
///               -> swap back -> repay flashloan -> profit to Orchestrator
///
/// @dev    IMPORTANT TRUST-BOUNDARY NOTE (not a bug fix, a design fact you should know):
///         `estimated_price_impact_bps` is a value supplied by the caller (your orchestrator),
///         not something this contract independently verifies against live pool reserves.
///         The check below is a floor on a *claim*, not a guarantee about *reality* — it protects
///         you only to the extent you trust whatever computed that number off-chain. If you want
///         an on-chain guarantee instead of a trust-based one, this contract would need to read
///         pool reserves/price directly and compute impact itself, which is a real design decision
///         (which oracle/pool-reading method to standardize on) that has to be made deliberately,
///         not defaulted silently inside a "bug fix." Flagging it rather than quietly changing
///         the security model out from under you.
contract MevOfa {
    using SafeERC20 for IERC20;

    // ─────────────────────────────────────────────────────────────────────────
    // Immutables
    // ─────────────────────────────────────────────────────────────────────────
    address public immutable orchestrator;

    // Minimum price impact in bps to proceed (adverse selection filter)
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
    error BackrunFailed(uint8 leg);
    error MalformedSwapReturnData(uint8 leg);
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

        // Adverse selection guard: reject if claimed price impact is below threshold.
        // See trust-boundary note at the top of this file for what this check does and doesn't guarantee.
        if (estimated_price_impact_bps < MIN_PRICE_IMPACT_BPS) {
            emit BackrunSkipped(user_tx_hash, "price_impact_below_threshold");
            revert PriceImpactTooLow(estimated_price_impact_bps, MIN_PRICE_IMPACT_BPS);
        }

        // Leg 1: swap token_in -> token_out (capture price impact from user tx)
        uint256 tokenOutBefore = IERC20(token_out).balanceOf(address(this));

        IERC20(token_in).forceApprove(pool, 0);
        IERC20(token_in).forceApprove(pool, amount_in);

        (bool leg1Ok, bytes memory leg1Result) = pool.call(
            abi.encodeWithSignature(
                "swap(address,address,uint256,uint256,address)",
                token_in, token_out, amount_in, 0, address(this)
            )
        );
        if (!leg1Ok) revert BackrunFailed(1);
        // Return value isn't used for accounting (balance delta is the source of truth below),
        // but we still confirm the call actually returned well-formed data rather than silently
        // succeeding on an empty/short response.
        if (leg1Result.length < 32) revert MalformedSwapReturnData(1);

        // Slippage check on leg 1 — measured via balance delta (source of truth)
        uint256 tokenOutAfter = IERC20(token_out).balanceOf(address(this));
        uint256 actualOut     = tokenOutAfter - tokenOutBefore;
        uint256 minOut        = amount_in * (10_000 - max_slippage_bps) / 10_000;
        if (actualOut < minOut)
            revert SlippageExceeded(actualOut, minOut);

        // Leg 2: swap token_out -> token_in (close the position)
        uint256 tokenInBefore = IERC20(token_in).balanceOf(address(this));

        IERC20(token_out).forceApprove(pool, 0);
        IERC20(token_out).forceApprove(pool, actualOut);

        (bool leg2Ok, bytes memory leg2Result) = pool.call(
            abi.encodeWithSignature(
                "swap(address,address,uint256,uint256,address)",
                token_out, token_in, actualOut, 0, address(this)
            )
        );
        if (!leg2Ok) revert BackrunFailed(2);
        if (leg2Result.length < 32) revert MalformedSwapReturnData(2);

        uint256 finalTokenIn = IERC20(token_in).balanceOf(address(this)) - tokenInBefore;

        // Profit check
        if (finalTokenIn <= flashloanAmount)
            revert InsufficientProfit(0, min_profit);
        uint256 profit = finalTokenIn - flashloanAmount;
        if (profit < min_profit)
            revert InsufficientProfit(profit, min_profit);

        netOutput = finalTokenIn;

        // Send the flashloaned token (plus profit) back to the caller (the Orchestrator)
        // so it can repay the flashloan provider.
        IERC20(token_in).safeTransfer(msg.sender, finalTokenIn);

        emit BackrunExecuted(user_tx_hash, pool, token_in, token_out, amount_in, profit);
    }
}