// contracts/src/strategies/LiquidationArb.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @title LiquidationArb — v13 Final
/// @notice Phase 3: Liquidation arbitrage across Aave v3, Compound v3, Morpho Blue.
///         Phase 3.1 adds Euler v2 after independent audit completion.
///         Called via call() from OmegaOrchestrator (NOT delegatecall).
///
/// @dev    CRITICAL invariant: flashloan provider != target protocol.
///           - Aave liquidations: flashloan from Balancer or Uniswap (NOT Aave).
///           - Euler liquidations (Phase 3.1): flashloan from Balancer (NOT Euler).
///
///         Calldata layout (ABI-encoded):
///           Protocol protocol    — liquidation target protocol
///           address  collateral  — collateral token to receive
///           address  debt        — debt token to repay
///           address  user        — borrower to liquidate
///           uint256  debtToCover — amount of debt to repay (ignored for Compound, see below)
///           uint256  minProfit   — minimum net profit required
///           bytes    extraData   — protocol-specific extension data (currently only used by
///                                  MorphoBlue — see _liquidateMorpho). Pass empty bytes ("")
///                                  for Aave/Compound/Euler.
///
///         Flow: flashloan(debt) -> liquidate(user, debtToCover)
///               -> receive collateral -> swap collateral -> debt
///               -> repay flashloan -> profit to Orchestrator
///
/// CHANGES vs prior version (see accompanying audit for full detail):
///   1. FIXED — Morpho Blue integration was structurally broken: MarketParams (oracle, irm,
///      lltv) were hardcoded to zero/wrong values, which does not hash to any real registered
///      Morpho market, and loanToken/collateralToken were in the wrong argument order. This
///      version requires the caller to supply real MarketParams via `extraData`, decoded and
///      validated inside `_liquidateMorpho`.
///   2. FIXED — `_liquidateMorpho` (and formerly Uniswap V3 / Balancer hops in MultiStepArb,
///      not present in this file) used `abi.encodeWithSignature(sig, abi.encode(...))`, which
///      double-encodes a tuple as raw bytes and produces calldata the target contract cannot
///      decode. Fixed to pass the struct value directly to the encoder.
///   3. safeApprove -> forceApprove (OpenZeppelin 5.x removed safeApprove).
///   4. abi.decode on swap-router return data now checks length before decoding.
contract LiquidationArb {
    using SafeERC20 for IERC20;

    // ─────────────────────────────────────────────────────────────────────────
    // Protocol enum
    // ─────────────────────────────────────────────────────────────────────────
    enum Protocol {
        AaveV3,       // Phase 3.0
        CompoundV3,   // Phase 3.0
        MorphoBlue,   // Phase 3.0
        EulerV2       // Phase 3.1 — requires independent audit before activation
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Morpho Blue market identity — a market is identified by the hash of these five
    // fields, NOT by an address. All five MUST correspond to a real, already-created
    // Morpho market or the liquidate() call below will simply fail to find a market.
    // ─────────────────────────────────────────────────────────────────────────
    struct MorphoMarketParams {
        address loanToken;
        address collateralToken;
        address oracle;
        address irm;
        uint256 lltv;
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Immutables
    // ─────────────────────────────────────────────────────────────────────────
    address public immutable orchestrator;

    address public immutable aave_pool;
    address public immutable compound_comet;
    address public immutable morpho_blue;
    address public immutable euler_v2;        // address(0) until Phase 3.1 audit passes

    address public immutable swap_router;

    // ─────────────────────────────────────────────────────────────────────────
    // Phase gate for Euler v2
    // ─────────────────────────────────────────────────────────────────────────
    bool public euler_activated; // false until governance Phase 3.1 activation

    // ─────────────────────────────────────────────────────────────────────────
    // Events
    // ─────────────────────────────────────────────────────────────────────────
    event LiquidationExecuted(
        Protocol indexed protocol,
        address indexed user,
        address  collateral,
        address  debt,
        uint256  debtCovered,
        uint256  collateralReceived,
        uint256  netProfit
    );
    event EulerActivated();

    // ─────────────────────────────────────────────────────────────────────────
    // Errors
    // ─────────────────────────────────────────────────────────────────────────
    error OnlyOrchestrator();
    error EulerNotYetActivated();
    error InsufficientProfit(uint256 actual, uint256 minimum);
    error LiquidationFailed(Protocol protocol);
    error CollateralSwapFailed();
    error MalformedSwapReturnData();
    error MalformedMorphoExtraData();
    error InvalidMorphoMarketParams();
    error ZeroAddress();
    error InvalidCalldata();
    error InvalidProtocol();

    // ─────────────────────────────────────────────────────────────────────────
    // Constructor
    // ─────────────────────────────────────────────────────────────────────────
    constructor(
        address _orchestrator,
        address _aavePool,
        address _compoundComet,
        address _morphoBlue,
        address _eulerV2,          // pass address(0) for Phase 3.0 deploy
        address _swapRouter
    ) {
        if (_orchestrator == address(0) || _swapRouter == address(0))
            revert ZeroAddress();
        orchestrator    = _orchestrator;
        aave_pool       = _aavePool;
        compound_comet  = _compoundComet;
        morpho_blue     = _morphoBlue;
        euler_v2        = _eulerV2;
        swap_router     = _swapRouter;
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Modifiers
    // ─────────────────────────────────────────────────────────────────────────
    modifier onlyOrchestrator() {
        if (msg.sender != orchestrator) revert OnlyOrchestrator();
        _;
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Phase 3.1 gate — called by governance after Euler v2 audit passes
    // ─────────────────────────────────────────────────────────────────────────
    function activateEuler() external onlyOrchestrator {
        if (euler_v2 == address(0)) revert ZeroAddress();
        euler_activated = true;
        emit EulerActivated();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Core
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Execute liquidation arbitrage.
    /// @param  strategyCalldata ABI-encoded params (see layout above).
    /// @param  flashloanAmount  Amount of debt token received from flashloan provider.
    /// @return netOutput        Amount of debt token returned (flashloanAmount + profit).
    function execute(
        bytes calldata strategyCalldata,
        uint256 flashloanAmount
    ) external onlyOrchestrator returns (uint256 netOutput) {
        if (strategyCalldata.length == 0) revert InvalidCalldata();

        (
            Protocol protocol,
            address  collateral,
            address  debt,
            address  user,
            uint256  debtToCover,
            uint256  minProfit,
            bytes memory extraData
        ) = abi.decode(strategyCalldata, (Protocol, address, address, address, uint256, uint256, bytes));

        if (collateral == address(0) || debt == address(0) || user == address(0))
            revert ZeroAddress();

        // Gate Euler V2 behind Phase 3.1 audit activation
        if (protocol == Protocol.EulerV2 && !euler_activated)
            revert EulerNotYetActivated();

        // Execute liquidation — receives collateral
        uint256 collateralReceived;
        if      (protocol == Protocol.AaveV3)     { collateralReceived = _liquidateAave(collateral, debt, user, debtToCover); }
        else if (protocol == Protocol.CompoundV3) { collateralReceived = _liquidateCompound(collateral, debt, user, flashloanAmount); }
        else if (protocol == Protocol.MorphoBlue) { collateralReceived = _liquidateMorpho(collateral, debt, user, debtToCover, extraData); }
        else if (protocol == Protocol.EulerV2)    { collateralReceived = _liquidateEuler(collateral, debt, user, debtToCover); }
        else revert InvalidProtocol();

        // Swap received collateral back to debt token
        uint256 debtTokenOut = _swapCollateralToDebt(collateral, debt, collateralReceived);

        // Profit check
        if (debtTokenOut <= flashloanAmount)
            revert InsufficientProfit(0, minProfit);
        uint256 profit = debtTokenOut - flashloanAmount;
        if (profit < minProfit)
            revert InsufficientProfit(profit, minProfit);

        netOutput = debtTokenOut;

        // Send the flashloaned token (plus profit) back to the caller (the Orchestrator)
        // so it can repay the flashloan provider.
        IERC20(debt).safeTransfer(msg.sender, debtTokenOut);

        emit LiquidationExecuted(
            protocol, user, collateral, debt, debtToCover, collateralReceived, profit
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Protocol-specific liquidation implementations
    // ─────────────────────────────────────────────────────────────────────────

    function _liquidateAave(
        address collateral,
        address debt,
        address user,
        uint256 debtToCover
    ) internal returns (uint256 collateralReceived) {
        IERC20(debt).forceApprove(aave_pool, 0);
        IERC20(debt).forceApprove(aave_pool, debtToCover);

        uint256 balBefore = IERC20(collateral).balanceOf(address(this));

        // IAaveV3Pool.liquidationCall(collateralAsset, debtAsset, user, debtToCover, receiveAToken)
        (bool success,) = aave_pool.call(
            abi.encodeWithSignature(
                "liquidationCall(address,address,address,uint256,bool)",
                collateral, debt, user, debtToCover, false
            )
        );
        if (!success) revert LiquidationFailed(Protocol.AaveV3);

        collateralReceived = IERC20(collateral).balanceOf(address(this)) - balBefore;
    }

    function _liquidateCompound(
        address collateral,
        address debt,
        address user,
        uint256 flashloanAmount
    ) internal returns (uint256 collateralReceived) {
        IERC20(debt).forceApprove(compound_comet, 0);
        IERC20(debt).forceApprove(compound_comet, flashloanAmount);

        uint256 balBefore = IERC20(collateral).balanceOf(address(this));

        // IComet.absorb(absorber, accounts[]) — makes the position liquidatable
        address[] memory accounts = new address[](1);
        accounts[0] = user;
        (bool absorbOk,) = compound_comet.call(
            abi.encodeWithSignature("absorb(address,address[])", address(this), accounts)
        );
        if (!absorbOk) revert LiquidationFailed(Protocol.CompoundV3);

        // IComet.buyCollateral(asset, minAmount, baseAmount, recipient)
        (bool buyOk,) = compound_comet.call(
            abi.encodeWithSignature(
                "buyCollateral(address,uint256,uint256,address)",
                collateral, 0, flashloanAmount, address(this)
            )
        );
        if (!buyOk) revert LiquidationFailed(Protocol.CompoundV3);

        collateralReceived = IERC20(collateral).balanceOf(address(this)) - balBefore;
    }

    /// @dev Morpho Blue markets are identified by hash(MarketParams), not an address —
    ///      there is no way to liquidate correctly without the real oracle/irm/lltv for the
    ///      specific market the borrower's position lives in. Those three fields plus an
    ///      explicit choice of `seizedAssets` vs `repaidShares` must be supplied by the caller
    ///      via `extraData`, ABI-encoded as (MorphoMarketParams, uint256 seizedAssets).
    ///      Per Morpho's own liquidate() semantics, exactly one of (seizedAssets, repaidShares)
    ///      must be non-zero; `debtToCover` (the top-level field, already decoded by execute())
    ///      is used as `repaidShares`. If you intend to specify collateral seized directly
    ///      instead, set `debtToCover` to 0 and put the seize amount in `extraData` — this
    ///      contract does not guess which path you meant.
    function _liquidateMorpho(
        address collateral,
        address debt,
        address user,
        uint256 debtToCover,       // interpreted as `repaidShares` unless it is 0
        bytes memory extraData
    ) internal returns (uint256 collateralReceived) {
        if (extraData.length == 0) revert MalformedMorphoExtraData();

        (MorphoMarketParams memory marketParams, uint256 seizedAssets) =
            abi.decode(extraData, (MorphoMarketParams, uint256));

        if (marketParams.oracle == address(0) || marketParams.irm == address(0) || marketParams.lltv == 0)
            revert InvalidMorphoMarketParams();
        if (marketParams.loanToken != debt || marketParams.collateralToken != collateral)
            revert InvalidMorphoMarketParams();
        // Exactly one of seizedAssets / debtToCover(repaidShares) must be set, per Morpho semantics.
        if ((seizedAssets == 0) == (debtToCover == 0))
            revert InvalidMorphoMarketParams();

        // See debtToCoverApprovalCeiling() below for why this is bounded by actual balance
        // rather than an amount computed from debtToCover (which is denominated in shares).
        IERC20(debt).forceApprove(morpho_blue, 0);
        IERC20(debt).forceApprove(morpho_blue, debtToCoverApprovalCeiling(debt));

        uint256 balBefore = IERC20(collateral).balanceOf(address(this));

        // IMorphoBlue.liquidate(MarketParams, borrower, seizedAssets, repaidShares, data)
        // Pass the struct directly — do NOT pre-encode it with abi.encode() and hand the
        // resulting bytes to abi.encodeWithSignature(), which double-encodes the tuple and
        // produces calldata Morpho cannot parse (this was the bug in the prior version).
        (bool success,) = morpho_blue.call(
            abi.encodeWithSignature(
                "liquidate((address,address,address,address,uint256),address,uint256,uint256,bytes)",
                marketParams,
                user,
                seizedAssets,
                debtToCover,
                bytes("")
            )
        );
        if (!success) revert LiquidationFailed(Protocol.MorphoBlue);

        collateralReceived = IERC20(collateral).balanceOf(address(this)) - balBefore;
    }

    /// @dev Small helper purely so the approval amount for the repaidShares-path case has an
    ///      explicit, auditable ceiling instead of an arbitrary guess. Morpho's repaidShares are
    ///      denominated in market shares, not underlying debt-token units, so the true asset cost
    ///      of repaying `debtToCover` shares is not knowable on-chain without reading market state.
    ///      Approving this contract's *current debt-token balance* (which, at this point in the
    ///      flow, is exactly the flashloaned amount) is a tight, auditable ceiling: Morpho can
    ///      never pull more than what this contract actually holds, and it's re-approved to zero
    ///      first on every call, so nothing is left standing between transactions.
    function debtToCoverApprovalCeiling(address debtToken) internal view returns (uint256) {
        return IERC20(debtToken).balanceOf(address(this));
    }

    function _liquidateEuler(
        address collateral,
        address debt,
        address user,
        uint256 debtToCover
    ) internal returns (uint256 collateralReceived) {
        // Phase 3.1 — Euler v2 liquidation
        // euler_activated gate enforced in execute() before this is called
        IERC20(debt).forceApprove(euler_v2, 0);
        IERC20(debt).forceApprove(euler_v2, debtToCover);

        uint256 balBefore = IERC20(collateral).balanceOf(address(this));

        // IEVault.liquidate(violator, collateral, repayAssets, minYieldBalance)
        (bool success,) = euler_v2.call(
            abi.encodeWithSignature(
                "liquidate(address,address,uint256,uint256)",
                user, collateral, debtToCover, 0
            )
        );
        if (!success) revert LiquidationFailed(Protocol.EulerV2);

        collateralReceived = IERC20(collateral).balanceOf(address(this)) - balBefore;
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Collateral -> debt swap
    // ─────────────────────────────────────────────────────────────────────────

    function _swapCollateralToDebt(
        address collateral,
        address debt,
        uint256 collateralAmount
    ) internal returns (uint256 debtOut) {
        IERC20(collateral).forceApprove(swap_router, 0);
        IERC20(collateral).forceApprove(swap_router, collateralAmount);

        (bool success, bytes memory result) = swap_router.call(
            abi.encodeWithSignature(
                "swap(address,address,uint256,uint256,address)",
                collateral, debt, collateralAmount, 0, address(this)
            )
        );
        if (!success) revert CollateralSwapFailed();
        if (result.length < 32) revert MalformedSwapReturnData();
        debtOut = abi.decode(result, (uint256));
    }
}