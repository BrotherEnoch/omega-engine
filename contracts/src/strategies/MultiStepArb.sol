// contracts/src/strategies/MultiStepArb.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @title MultiStepArb — v12 Final
/// @notice Phase 2: Multi-hop cross-protocol arbitrage (up to 8 hops).
///         Bellman-Ford optimal route computed off-chain; executed on-chain atomically.
///         Called via call() from OmegaOrchestrator (NOT delegatecall).
///
/// @dev    Calldata layout (ABI-encoded):
///           Hop[]  route          — ordered array of up to 8 hops
///           uint256 min_profit    — minimum net profit required (in route[0].token_in)
///
///         Hop struct:
///           address pool          — DEX pool address
///           address token_in      — input token for this hop
///           address token_out     — output token for this hop
///           uint256 amount_in     — input amount (0 = use full balance from previous hop)
///           uint8   dex_type      — 0 = UniV2, 1 = UniV3, 2 = Curve, 3 = Balancer
///
///         Flow: flashloan(token_in) → hop[0] → hop[1] → … → hop[N]
///               → repay → profit to Orchestrator
contract MultiStepArb {
    using SafeERC20 for IERC20;

    // ─────────────────────────────────────────────────────────────────────────
    // Constants
    // ─────────────────────────────────────────────────────────────────────────
    uint256 public constant MAX_HOPS = 8;

    // DEX type identifiers
    uint8 public constant DEX_UNIV2     = 0;
    uint8 public constant DEX_UNIV3     = 1;
    uint8 public constant DEX_CURVE     = 2;
    uint8 public constant DEX_BALANCER  = 3;

    // ─────────────────────────────────────────────────────────────────────────
    // Types
    // ─────────────────────────────────────────────────────────────────────────
    struct Hop {
        address pool;
        address token_in;
        address token_out;
        uint256 amount_in;  // 0 = use full output of previous hop
        uint8   dex_type;
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Immutables
    // ─────────────────────────────────────────────────────────────────────────
    address public immutable orchestrator;

    // ─────────────────────────────────────────────────────────────────────────
    // Events
    // ─────────────────────────────────────────────────────────────────────────
    event MultiStepArbExecuted(
        uint256 hops,
        address tokenStart,
        address tokenEnd,
        uint256 amountIn,
        uint256 netProfit
    );

    // ─────────────────────────────────────────────────────────────────────────
    // Errors
    // ─────────────────────────────────────────────────────────────────────────
    error OnlyOrchestrator();
    error TooManyHops(uint256 hops, uint256 max);
    error ZeroHops();
    error InsufficientProfit(uint256 actual, uint256 minimum);
    error SwapFailed(uint256 hopIndex, address pool);
    error ZeroAddress();
    error InvalidCalldata();
    error TokenMismatch(uint256 hopIndex);   // hop[i].token_out != hop[i+1].token_in

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

    /// @notice Execute a multi-hop arbitrage route.
    /// @param  strategyCalldata ABI-encoded (Hop[], min_profit).
    /// @param  flashloanAmount  Amount of route[0].token_in received from flashloan.
    /// @return netOutput        Amount of token_in returned (flashloanAmount + profit).
    function execute(
        bytes calldata strategyCalldata,
        uint256 flashloanAmount
    ) external onlyOrchestrator returns (uint256 netOutput) {
        if (strategyCalldata.length == 0) revert InvalidCalldata();

        (Hop[] memory route, uint256 min_profit) = abi.decode(
            strategyCalldata,
            (Hop[], uint256)
        );

        if (route.length == 0)          revert ZeroHops();
        if (route.length > MAX_HOPS)    revert TooManyHops(route.length, MAX_HOPS);

        // Validate token chain continuity
        for (uint256 i = 0; i + 1 < route.length; i++) {
            if (route[i].token_out != route[i + 1].token_in)
                revert TokenMismatch(i + 1);
        }

        // Execute hops sequentially
        uint256 currentAmount = flashloanAmount;
        for (uint256 i = 0; i < route.length; i++) {
            Hop memory hop = route[i];
            if (hop.pool == address(0) || hop.token_in == address(0) || hop.token_out == address(0))
                revert ZeroAddress();

            // If amount_in == 0: use full output from previous hop
            uint256 swapIn = hop.amount_in == 0 ? currentAmount : hop.amount_in;
            currentAmount  = _dispatchSwap(hop, swapIn, i);
        }

        // Final token must be route[0].token_in
        uint256 finalOut = currentAmount;

        // Profit check
        if (finalOut <= flashloanAmount)
            revert InsufficientProfit(0, min_profit);
        uint256 profit = finalOut - flashloanAmount;
        if (profit < min_profit)
            revert InsufficientProfit(profit, min_profit);

        netOutput = finalOut;

        emit MultiStepArbExecuted(
            route.length,
            route[0].token_in,
            route[route.length - 1].token_out,
            flashloanAmount,
            profit
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Swap dispatcher — routes by DEX type
    // ─────────────────────────────────────────────────────────────────────────

    function _dispatchSwap(
        Hop memory hop,
        uint256 amountIn,
        uint256 hopIndex
    ) internal returns (uint256 amountOut) {
        IERC20(hop.token_in).safeApprove(hop.pool, 0);
        IERC20(hop.token_in).safeApprove(hop.pool, amountIn);

        bool success;
        bytes memory result;

        if (hop.dex_type == DEX_UNIV2) {
            (success, result) = hop.pool.call(
                abi.encodeWithSignature(
                    "swap(address,address,uint256,uint256,address)",
                    hop.token_in, hop.token_out, amountIn, 0, address(this)
                )
            );
        } else if (hop.dex_type == DEX_UNIV3) {
            // UniV3: exactInputSingle
            (success, result) = hop.pool.call(
                abi.encodeWithSignature(
                    "exactInputSingle((address,address,uint24,address,uint256,uint256,uint160))",
                    abi.encode(
                        hop.token_in,
                        hop.token_out,
                        uint24(3000),     // fee tier — optimally passed per-hop; default 0.3%
                        address(this),
                        amountIn,
                        0,
                        uint160(0)        // sqrtPriceLimitX96 — no limit
                    )
                )
            );
        } else if (hop.dex_type == DEX_CURVE) {
            // Curve: exchange(i, j, dx, min_dy)
            (success, result) = hop.pool.call(
                abi.encodeWithSignature(
                    "exchange(int128,int128,uint256,uint256)",
                    int128(0), int128(1), amountIn, 0
                )
            );
        } else if (hop.dex_type == DEX_BALANCER) {
            // Balancer V2: batchSwap (simplified single swap)
            (success, result) = hop.pool.call(
                abi.encodeWithSignature(
                    "swap((bytes32,uint8,address,address,uint256,bytes),(address,bool,address,bool),uint256,uint256)",
                    abi.encode(
                        bytes32(0), uint8(0), hop.token_in, hop.token_out, amountIn, bytes("")
                    ),
                    abi.encode(address(this), false, address(this), false),
                    0,
                    block.timestamp
                )
            );
        } else {
            // Fallback: generic swap signature
            (success, result) = hop.pool.call(
                abi.encodeWithSignature(
                    "swap(address,address,uint256,uint256,address)",
                    hop.token_in, hop.token_out, amountIn, 0, address(this)
                )
            );
        }

        if (!success) revert SwapFailed(hopIndex, hop.pool);
        amountOut = abi.decode(result, (uint256));
    }
}
