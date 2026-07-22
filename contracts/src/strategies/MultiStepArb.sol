// contracts/src/strategies/MultiStepArb.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @title MultiStepArb — v13 Final
/// @notice Phase 2: Multi-hop cross-protocol arbitrage (up to 8 hops).
///         Bellman-Ford optimal route computed off-chain; executed on-chain atomically.
///         Called via call() from OmegaOrchestrator (NOT delegatecall).
///
/// @dev    Calldata layout (ABI-encoded):
///           Hop[]  route          — ordered array of up to 8 hops
///           uint256 min_profit    — minimum net profit required (in route[0].token_in)
///
///         Hop struct (see below) now carries explicit per-DEX routing fields instead of
///         relying on hardcoded assumptions (Curve coin indices, UniV3 fee tier), because
///         those hardcoded assumptions were a correctness bug in the prior version.
///
///         Flow: flashloan(token_in) -> hop[0] -> hop[1] -> ... -> hop[N]
///               -> repay -> profit to Orchestrator
///
/// CHANGES vs prior version (see accompanying audit for full detail):
///   1. FIXED — UniV3 and Balancer branches used
///      `abi.encodeWithSignature(sig, abi.encode(...))`, which double-encodes the tuple
///      argument as raw bytes and produces calldata the target router/vault cannot decode.
///      Fixed to pass struct values directly to the encoder.
///   2. FIXED — Curve branch hardcoded coin indices (i=0, j=1) regardless of which tokens
///      were actually being swapped. Any pool where the intended tokens weren't at indices
///      0/1 would swap the wrong assets or revert. `Hop` now carries explicit `curve_i` /
///      `curve_j` fields that the off-chain route builder must populate for Curve hops.
///   3. FIXED — nothing previously checked that the last hop's output token matched
///      route[0].token_in (the token the flashloan must be repaid in). Added an explicit
///      check; without it a routing bug upstream could silently compare two different
///      tokens' balances as if they were fungible.
///   4. ADDED — `Hop.univ3_fee` replaces the hardcoded 3000 (0.3%) fee tier, since not every
///      pool pair trades at that tier.
///   5. ADDED — `Hop.balancer_pool_id`, since Balancer V2 identifies a pool by its bytes32
///      poolId within the Vault, not by the Vault's own address alone; the prior version had
///      no way to specify which pool to actually trade against.
///   6. safeApprove -> forceApprove (OpenZeppelin 5.x removed safeApprove).
///   7. abi.decode on swap return data now checks length before decoding.
contract MultiStepArb {
    using SafeERC20 for IERC20;

    // ─────────────────────────────────────────────────────────────────────────
    // Constants
    // ─────────────────────────────────────────────────────────────────────────
    uint256 public constant MAX_HOPS = 8;

    uint8 public constant DEX_UNIV2     = 0;
    uint8 public constant DEX_UNIV3     = 1;
    uint8 public constant DEX_CURVE     = 2;
    uint8 public constant DEX_BALANCER  = 3;

    // ─────────────────────────────────────────────────────────────────────────
    // Types
    // ─────────────────────────────────────────────────────────────────────────

    /// @dev Fields not relevant to a given `dex_type` are ignored by that branch, but the
    ///      off-chain route builder must still populate the ones that ARE relevant:
    ///        - DEX_UNIV3:    univ3_fee must be the real fee tier of the target pool.
    ///        - DEX_CURVE:    curve_i / curve_j must be the real coin indices for
    ///                        token_in / token_out in that pool.
    ///        - DEX_BALANCER: balancer_pool_id must be the real poolId; `pool` must be the
    ///                        Balancer Vault address (not the pool itself).
    struct Hop {
        address pool;             // DEX pool/router address (Balancer: the Vault address)
        address token_in;         // input token for this hop
        address token_out;        // output token for this hop
        uint256 amount_in;        // 0 = use full balance from previous hop
        uint8   dex_type;         // 0=UniV2, 1=UniV3, 2=Curve, 3=Balancer
        uint24  univ3_fee;        // UniV3 only: fee tier in hundredths of a bip (e.g. 3000 = 0.3%)
        int128  curve_i;          // Curve only: coin index of token_in
        int128  curve_j;          // Curve only: coin index of token_out
        bytes32 balancer_pool_id; // Balancer only: pool ID within the Vault
    }

    /// @dev Mirrors Uniswap V3 SwapRouter's ExactInputSingleParams (the "classic" ISwapRouter,
    ///      which includes `deadline`). If your deployed router is SwapRouter02 (no `deadline`
    ///      field), this struct and the encoded signature string below both need to change to
    ///      match — that's a real external-dependency decision (which router you're pointed at),
    ///      not something guessable from this file alone. Confirm against your router's actual
    ///      ABI before deploying.
    struct UniV3ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24  fee;
        address recipient;
        uint256 deadline;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    /// @dev Mirrors Balancer V2 IVault.SingleSwap.
    struct BalancerSingleSwap {
        bytes32 poolId;
        uint8   kind;     // 0 = GIVEN_IN
        address assetIn;
        address assetOut;
        uint256 amount;
        bytes   userData;
    }

    /// @dev Mirrors Balancer V2 IVault.FundManagement.
    struct BalancerFundManagement {
        address sender;
        bool    fromInternalBalance;
        address payable recipient;
        bool    toInternalBalance;
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
    error MalformedSwapReturnData(uint256 hopIndex);
    error ZeroAddress();
    error InvalidCalldata();
    error TokenMismatch(uint256 hopIndex);        // hop[i].token_out != hop[i+1].token_in
    error RouteDoesNotClose();                    // last hop's token_out != route[0].token_in

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

        // Validate token chain continuity between consecutive hops
        for (uint256 i = 0; i + 1 < route.length; i++) {
            if (route[i].token_out != route[i + 1].token_in)
                revert TokenMismatch(i + 1);
        }

        // The route must close: what comes out at the end must be the same token that was
        // flashloaned in, since that's what gets compared against flashloanAmount below and
        // what actually has to repay the flashloan. Without this check, a bug in the off-chain
        // route builder could produce a route that "profits" in the wrong token.
        if (route[route.length - 1].token_out != route[0].token_in)
            revert RouteDoesNotClose();

        // Execute hops sequentially
        uint256 currentAmount = flashloanAmount;
        for (uint256 i = 0; i < route.length; i++) {
            Hop memory hop = route[i];
            if (hop.pool == address(0) || hop.token_in == address(0) || hop.token_out == address(0))
                revert ZeroAddress();

            uint256 swapIn = hop.amount_in == 0 ? currentAmount : hop.amount_in;
            currentAmount  = _dispatchSwap(hop, swapIn, i);
        }

        uint256 finalOut = currentAmount;

        // Profit check
        if (finalOut <= flashloanAmount)
            revert InsufficientProfit(0, min_profit);
        uint256 profit = finalOut - flashloanAmount;
        if (profit < min_profit)
            revert InsufficientProfit(profit, min_profit);

        netOutput = finalOut;

        // Send the flashloaned token (plus profit) back to the caller (the Orchestrator)
        // so it can repay the flashloan provider.
        IERC20(route[0].token_in).safeTransfer(msg.sender, finalOut);

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
        IERC20(hop.token_in).forceApprove(hop.pool, 0);
        IERC20(hop.token_in).forceApprove(hop.pool, amountIn);

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
            UniV3ExactInputSingleParams memory params = UniV3ExactInputSingleParams({
                tokenIn:           hop.token_in,
                tokenOut:          hop.token_out,
                fee:               hop.univ3_fee,
                recipient:         address(this),
                deadline:          block.timestamp,
                amountIn:          amountIn,
                amountOutMinimum:  0,
                sqrtPriceLimitX96: 0
            });
            (success, result) = hop.pool.call(
                abi.encodeWithSignature(
                    "exactInputSingle((address,address,uint24,address,uint256,uint256,uint256,uint160))",
                    params
                )
            );
        } else if (hop.dex_type == DEX_CURVE) {
            // Curve: exchange(i, j, dx, min_dy) — indices come from the Hop itself, not a
            // hardcoded assumption. The off-chain route builder is responsible for supplying
            // the correct coin indices for token_in/token_out in this specific pool.
            (success, result) = hop.pool.call(
                abi.encodeWithSignature(
                    "exchange(int128,int128,uint256,uint256)",
                    hop.curve_i, hop.curve_j, amountIn, uint256(0)
                )
            );
        } else if (hop.dex_type == DEX_BALANCER) {
            BalancerSingleSwap memory singleSwap = BalancerSingleSwap({
                poolId:    hop.balancer_pool_id,
                kind:      0,
                assetIn:   hop.token_in,
                assetOut:  hop.token_out,
                amount:    amountIn,
                userData:  bytes("")
            });
            BalancerFundManagement memory funds = BalancerFundManagement({
                sender:              address(this),
                fromInternalBalance: false,
                recipient:           payable(address(this)),
                toInternalBalance:   false
            });
            (success, result) = hop.pool.call(
                abi.encodeWithSignature(
                    "swap((bytes32,uint8,address,address,uint256,bytes),(address,bool,address,bool),uint256,uint256)",
                    singleSwap, funds, uint256(0), block.timestamp
                )
            );
        } else {
            // Fallback: generic adapter swap signature (same interface used by DEX_UNIV2)
            (success, result) = hop.pool.call(
                abi.encodeWithSignature(
                    "swap(address,address,uint256,uint256,address)",
                    hop.token_in, hop.token_out, amountIn, 0, address(this)
                )
            );
        }

        if (!success) revert SwapFailed(hopIndex, hop.pool);
        if (result.length < 32) revert MalformedSwapReturnData(hopIndex);
        amountOut = abi.decode(result, (uint256));
    }
}