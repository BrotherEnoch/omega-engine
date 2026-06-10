// contracts/src/strategies/LiquidationArb.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @title LiquidationArb — v12 Final
/// @notice Phase 3: Liquidation arbitrage across Aave v3, Compound v3, Morpho Blue.
///         Phase 3.1 adds Euler v2 after independent audit completion.
///         Called via call() from OmegaOrchestrator (NOT delegatecall).
///
/// @dev    CRITICAL invariant: flashloan provider ≠ target protocol.
///           - Aave liquidations: flashloan from Balancer or Uniswap (NOT Aave).
///           - Euler liquidations (Phase 3.1): flashloan from Balancer (NOT Euler).
///
///         Calldata layout (ABI-encoded):
///           Protocol protocol    — liquidation target protocol
///           address  collateral  — collateral token to receive
///           address  debt        — debt token to repay
///           address  user        — borrower to liquidate
///           uint256  debtToCover — amount of debt to repay
///           uint256  minProfit   — minimum net profit required
///
///         Flow: flashloan(debt) → liquidate(user, debtToCover)
///               → receive collateral → swap collateral → debt
///               → repay flashloan → profit to Orchestrator
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
    // Immutables
    // ─────────────────────────────────────────────────────────────────────────
    address public immutable orchestrator;

    // Protocol pool/contract addresses — set at construction
    address public immutable aave_pool;
    address public immutable compound_comet;
    address public immutable morpho_blue;
    address public immutable euler_v2;        // address(0) until Phase 3.1 audit passes

    // Swap router for collateral → debt conversion after liquidation
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
            uint256  minProfit
        ) = abi.decode(strategyCalldata, (Protocol, address, address, address, uint256, uint256));

        if (collateral == address(0) || debt == address(0) || user == address(0))
            revert ZeroAddress();

        // Gate Euler V2 behind Phase 3.1 audit activation
        if (protocol == Protocol.EulerV2 && !euler_activated)
            revert EulerNotYetActivated();

        // Execute liquidation — receives collateral
        uint256 collateralReceived;
        if      (protocol == Protocol.AaveV3)     { collateralReceived = _liquidateAave(collateral, debt, user, debtToCover, flashloanAmount); }
        else if (protocol == Protocol.CompoundV3) { collateralReceived = _liquidateCompound(collateral, debt, user, flashloanAmount); }
        else if (protocol == Protocol.MorphoBlue) { collateralReceived = _liquidateMorpho(collateral, debt, user, debtToCover, flashloanAmount); }
        else if (protocol == Protocol.EulerV2)    { collateralReceived = _liquidateEuler(collateral, debt, user, debtToCover, flashloanAmount); }
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
        uint256 debtToCover,
        uint256 /*flashloanAmount*/
    ) internal returns (uint256 collateralReceived) {
        IERC20(debt).safeApprove(aave_pool, 0);
        IERC20(debt).safeApprove(aave_pool, debtToCover);

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
        IERC20(debt).safeApprove(compound_comet, 0);
        IERC20(debt).safeApprove(compound_comet, flashloanAmount);

        uint256 balBefore = IERC20(collateral).balanceOf(address(this));

        // IComet.absorb(absorber, accounts[]) then buyCollateral
        // Step 1: absorb (makes position liquidatable)
        address[] memory accounts = new address[](1);
        accounts[0] = user;
        (bool absorbOk,) = compound_comet.call(
            abi.encodeWithSignature("absorb(address,address[])", address(this), accounts)
        );
        if (!absorbOk) revert LiquidationFailed(Protocol.CompoundV3);

        // Step 2: buy collateral from protocol
        (bool buyOk,) = compound_comet.call(
            abi.encodeWithSignature(
                "buyCollateral(address,uint256,uint256,address)",
                collateral, 0, flashloanAmount, address(this)
            )
        );
        if (!buyOk) revert LiquidationFailed(Protocol.CompoundV3);

        collateralReceived = IERC20(collateral).balanceOf(address(this)) - balBefore;
    }

    function _liquidateMorpho(
        address collateral,
        address debt,
        address user,
        uint256 debtToCover,
        uint256 /*flashloanAmount*/
    ) internal returns (uint256 collateralReceived) {
        IERC20(debt).safeApprove(morpho_blue, 0);
        IERC20(debt).safeApprove(morpho_blue, debtToCover);

        uint256 balBefore = IERC20(collateral).balanceOf(address(this));

        // IMorphoBlue.liquidate(MarketParams, borrower, seizedAssets, repaidShares, data)
        // Using repaidShares=0 path: specify seizedAssets computed off-chain
        (bool success,) = morpho_blue.call(
            abi.encodeWithSignature(
                "liquidate((address,address,address,address,uint256),address,uint256,uint256,bytes)",
                abi.encode(collateral, debt, address(0), address(0), uint256(0)), // MarketParams
                user,
                uint256(0),     // seizedAssets — 0 means use repaidShares path
                debtToCover,    // repaidShares (in debt token shares)
                bytes("")
            )
        );
        if (!success) revert LiquidationFailed(Protocol.MorphoBlue);

        collateralReceived = IERC20(collateral).balanceOf(address(this)) - balBefore;
    }

    function _liquidateEuler(
        address collateral,
        address debt,
        address user,
        uint256 debtToCover,
        uint256 /*flashloanAmount*/
    ) internal returns (uint256 collateralReceived) {
        // Phase 3.1 — Euler v2 liquidation
        // euler_activated gate enforced in execute() before this is called
        IERC20(debt).safeApprove(euler_v2, 0);
        IERC20(debt).safeApprove(euler_v2, debtToCover);

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
    // Collateral → debt swap
    // ─────────────────────────────────────────────────────────────────────────

    function _swapCollateralToDebt(
        address collateral,
        address debt,
        uint256 collateralAmount
    ) internal returns (uint256 debtOut) {
        IERC20(collateral).safeApprove(swap_router, 0);
        IERC20(collateral).safeApprove(swap_router, collateralAmount);

        (bool success, bytes memory result) = swap_router.call(
            abi.encodeWithSignature(
                "swap(address,address,uint256,uint256,address)",
                collateral, debt, collateralAmount, 0, address(this)
            )
        );
        if (!success) revert CollateralSwapFailed();
        debtOut = abi.decode(result, (uint256));
    }
}