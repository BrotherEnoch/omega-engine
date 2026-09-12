// contracts/test/OmegaSystemBoundary.t.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";

import {OmegaOrchestrator} from "../src/OmegaOrchestrator.sol";
import {OmegaVault}         from "../src/OmegaVault.sol";
import {CanaryArb}          from "../src/strategies/CanaryArb.sol";
import {MultiStepArb}       from "../src/strategies/MultiStepArb.sol";
import {LiquidationArb}     from "../src/strategies/LiquidationArb.sol";
import {MevOfa}             from "../src/strategies/MevOfa.sol";

// Reuse the mocks already defined and battle-tested in OmegaSystem.t.sol rather than
// maintaining a second copy that could silently drift from the real ABIs.
import {MockERC20, MockStarkVerifier, MockBalancerVault} from "./OmegaSystem.t.sol";

/// @title OmegaSystemBoundary.t.sol
/// @notice Boundary-case and trust-boundary coverage that OmegaSystem.t.sol doesn't
///         currently exercise:
///           - PER_TRANSFER_CAP / DAILY_CAP at the EXACT boundary (not just "one over")
///           - receivePendingProfit's one-shot-per-blueprintHash guarantee (ProfitAlreadyPending)
///           - rescueERC20's totalPendingProfit-aware surplus cap, at the exact boundary
///           - the BaseFeeTooHigh guard at the EXACT boundary (basefee == maxBaseFee, not just over)
///           - the key-rotation dual-key window at the EXACT boundary block
///           - MultiStepArb's MAX_HOPS boundary (8 hops succeeding, not just 9 reverting)
///           - LiquidationArb._liquidateCompound using flashloanAmount instead of the
///             caller-supplied debtToCover (a real discrepancy vs the Aave/Euler branches)
///           - MevOfa's price-impact gate accepting a caller-SUPPLIED claim with no relation
///             to what the swaps in the same call actually did (the trust-boundary note in
///             MevOfa.sol's own header, made concrete)
///           - DOCUMENTS (does not assert-as-bug) that finalizeKeyRotation() does not consult
///             OmegaVault.pendingProofCount() before completing, despite that getter's own doc
///             comment saying it exists for exactly that purpose. See that test's own comment
///             for why this is flagged as a documentation/behavior mismatch rather than a
///             proven bug -- releaseProfit() is permissionless and doesn't depend on the
///             Orchestrator's execution_key, so rotating keys does not itself endanger
///             already-deposited pending profit.

// ─────────────────────────────────────────────────────────────────────────────
// OmegaVault boundary tests
// ─────────────────────────────────────────────────────────────────────────────

contract OmegaVaultBoundaryTest is Test {
    OmegaVault        public vault;
    MockERC20         public token;
    MockStarkVerifier public verifier;

    address admin        = makeAddr("admin");
    address pil          = makeAddr("pil");
    address daoFeeAddr   = makeAddr("dao");
    address orchestrator = makeAddr("orchestrator");
    address depthUpdater = makeAddr("depthUpdater");

    uint256 constant PER_CAP   = 50 ether;
    uint256 constant DAILY_CAP = 500 ether;

    // 11 * 45 ether = 495 ether, leaving exactly 5 ether of daily headroom -- deliberately
    // NOT a multiple that lines up with PER_CAP, so the daily-cap boundary can be tested in
    // isolation from the per-transfer cap (see test_DailyCapOneWeiOverReverts for why that
    // matters: if remaining headroom == PER_CAP exactly, pushing 1 wei over trips BOTH caps
    // at once and the test would prove nothing about the daily cap specifically).
    uint256 constant CHUNK = 45 ether;

    function setUp() public {
        token    = new MockERC20();
        verifier = new MockStarkVerifier();

        vm.prank(admin);
        vault = new OmegaVault(
            pil,
            daoFeeAddr,
            address(verifier),
            address(token),
            admin,
            orchestrator,
            PER_CAP,
            DAILY_CAP
        );

        vm.startPrank(admin);
        vault.grantRole(vault.DEPTH_UPDATER_ROLE(), depthUpdater);
        vm.stopPrank();

        token.mint(orchestrator, 1_000_000 ether);
        vm.prank(orchestrator);
        token.approve(address(vault), type(uint256).max);

        // Idle balance sitting in the Vault ahead of any deposit -- used below to exercise
        // rescueERC20's surplus-above-totalPendingProfit boundary.
        token.mint(address(vault), 1000 ether);
    }

    function _boundHash(bytes32 bpHash, uint256 netProfit) internal view returns (bytes32) {
        return vault.computePublicInputsHash(bpHash, netProfit);
    }

    /// @dev Deposits + proves + confirms depth for `bpHash`, WITHOUT releasing -- release is
    ///      left to the caller so each test can assert on it directly.
    function _depositAndConfirm(bytes32 bpHash, uint256 amount) internal {
        vm.prank(orchestrator);
        vault.receivePendingProfit(bpHash, amount);
        vault.submitProof(bpHash, _boundHash(bpHash, amount), bytes("proof"));
        vm.prank(depthUpdater);
        vault.updateConfirmationDepth(bpHash, 12);
    }

    // ── PER_TRANSFER_CAP boundary ──────────────────────────────────────────────

    function test_PerTransferCapExactBoundarySucceeds() public {
        bytes32 bpHash = keccak256("per-cap-exact");
        _depositAndConfirm(bpHash, PER_CAP); // exactly at the cap -- must succeed, not revert

        vault.releaseProfit(bpHash);
        assertTrue(vault.released(bpHash), "release at exactly PER_TRANSFER_CAP must succeed");
    }

    // (test_PerTransferCapEnforced in OmegaSystem.t.sol already covers PER_CAP + 1 reverting)

    // ── DAILY_CAP boundary ─────────────────────────────────────────────────────

    function test_DailyCapExactBoundarySucceeds() public {
        // 11 releases of 45 ether = 495 ether, then a final 5 ether release lands exactly
        // on DAILY_CAP (500 ether) -- must succeed.
        for (uint256 i = 0; i < 11; i++) {
            bytes32 bpHash = keccak256(abi.encode("daily-fill", i));
            _depositAndConfirm(bpHash, CHUNK);
            vault.releaseProfit(bpHash);
        }
        assertEq(vault.daily_released(), 495 ether);

        bytes32 finalHash = keccak256("daily-exact-final");
        _depositAndConfirm(finalHash, 5 ether);
        vault.releaseProfit(finalHash); // 495 + 5 == DAILY_CAP exactly -- must succeed

        assertEq(vault.daily_released(), DAILY_CAP, "daily_released must land exactly on DAILY_CAP");
    }

    function test_DailyCapOneWeiOverReverts() public {
        for (uint256 i = 0; i < 11; i++) {
            bytes32 bpHash = keccak256(abi.encode("daily-overflow-fill", i));
            _depositAndConfirm(bpHash, CHUNK);
            vault.releaseProfit(bpHash);
        }
        assertEq(vault.daily_released(), 495 ether);
        // Remaining headroom is exactly 5 ether. Request 5 ether + 1 wei -- still well under
        // PER_TRANSFER_CAP (50 ether), so this isolates the daily-cap check specifically.
        uint256 overAmount = 5 ether + 1;
        bytes32 bpHash = keccak256("daily-overflow-final");
        _depositAndConfirm(bpHash, overAmount);

        vm.expectRevert(
            abi.encodeWithSelector(OmegaVault.ExceedsDailyCap.selector, overAmount, 5 ether)
        );
        vault.releaseProfit(bpHash);
    }

    // ── One-shot deposit guarantee (fix #1 in OmegaVault's own changelog) ──────

    function test_ReceivePendingProfitRevertsOnSecondDepositForSameHash() public {
        bytes32 bpHash = keccak256("double-deposit");

        vm.prank(orchestrator);
        vault.receivePendingProfit(bpHash, 1 ether);

        vm.prank(orchestrator);
        vm.expectRevert(
            abi.encodeWithSelector(OmegaVault.ProfitAlreadyPending.selector, bpHash)
        );
        vault.receivePendingProfit(bpHash, 1 ether);
    }

    // ── rescueERC20 surplus boundary (v14.1) ────────────────────────────────────

    function test_RescueERC20ExactSurplusSucceeds() public {
        bytes32 bpHash = keccak256("rescue-exact");
        vm.prank(orchestrator);
        vault.receivePendingProfit(bpHash, 10 ether); // totalPendingProfit = 10 ether

        // Vault balance = 1000 ether idle (from setUp) + 10 ether just deposited = 1010 ether.
        // Rescuable surplus = balance - totalPendingProfit = 1000 ether exactly.
        uint256 surplus = vault.rescuableProfitTokenSurplus();
        assertEq(surplus, 1000 ether);

        vm.prank(admin);
        vault.rescueERC20(address(token), admin, surplus); // exact boundary -- must succeed
        assertEq(token.balanceOf(admin), surplus);
    }

    function test_RescueERC20OneWeiOverSurplusReverts() public {
        bytes32 bpHash = keccak256("rescue-over");
        vm.prank(orchestrator);
        vault.receivePendingProfit(bpHash, 10 ether);

        uint256 surplus = vault.rescuableProfitTokenSurplus();

        vm.prank(admin);
        vm.expectRevert(
            abi.encodeWithSelector(
                OmegaVault.InsufficientRescuableBalance.selector, surplus + 1, surplus
            )
        );
        vault.rescueERC20(address(token), admin, surplus + 1);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OmegaOrchestrator boundary tests
// ─────────────────────────────────────────────────────────────────────────────

contract OmegaOrchestratorBoundaryTest is Test {
    OmegaOrchestrator public orch;
    MockBalancerVault public flashloan;
    OmegaVault        public vault;
    MockERC20         public token;
    MockStarkVerifier public verifier;
    CanaryArb         public canary;

    address admin      = makeAddr("admin");
    address pil        = makeAddr("pil");
    address daoFeeAddr = makeAddr("dao");

    uint256 execPrivKey = 0xA11CE;
    address execKey;

    function setUp() public {
        execKey   = vm.addr(execPrivKey);
        token     = new MockERC20();
        verifier  = new MockStarkVerifier();
        flashloan = new MockBalancerVault();

        vm.prank(admin);
        vault = new OmegaVault(
            pil, daoFeeAddr, address(verifier), address(token),
            admin, address(0),
            50 ether, 500 ether
        );

        vm.prank(admin);
        orch = new OmegaOrchestrator(
            uint64(block.chainid),
            address(vault),
            address(flashloan),
            address(0),
            execKey,
            admin
        );

        bytes32 orchestratorRole = vault.ORCHESTRATOR_ROLE();
        vm.prank(admin);
        vault.grantRole(orchestratorRole, address(orch));

        canary = new CanaryArb(address(orch));
        bytes32 canaryId = keccak256("canary");
        vm.prank(admin);
        orch.registerStrategy(canaryId, address(canary));
    }

    function _buildBlueprintWithBaseFee(
        bytes32 stratId,
        uint64 nonce,
        uint256 maxBaseFee
    ) internal view returns (bytes memory) {
        return abi.encode(
            uint64(block.number + 100),
            nonce,
            stratId,
            OmegaOrchestrator.FlashloanProviderType.Balancer,
            address(token),
            address(0),
            abi.encode(address(token)),
            uint256(0),
            uint256(0),
            maxBaseFee
        );
    }

    function _sign(bytes memory bp, uint256 key) internal view returns (bytes memory) {
        bytes32 hash = keccak256(abi.encode(address(orch), uint64(block.chainid), bp));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, hash);
        return abi.encodePacked(r, s, v);
    }

    // ── BaseFeeTooHigh boundary ─────────────────────────────────────────────────

    function test_BaseFeeExactBoundarySucceeds() public {
        // block.basefee == maxBaseFee is the documented boundary that must still succeed --
        // execute() only reverts when basefee is STRICTLY greater than maxBaseFee.
        bytes32 canaryId = keccak256("canary");
        bytes memory bp  = _buildBlueprintWithBaseFee(canaryId, 0, 1);
        bytes memory sig = _sign(bp, execPrivKey);

        vm.fee(1); // block.basefee = 1 == maxBaseFee
        orch.execute(bp, sig); // must NOT revert
    }

    // (test_BaseFeeGuardReverts in OmegaSystem.t.sol already covers basefee > maxBaseFee)

    // ── Key rotation window boundary ────────────────────────────────────────────

    function test_KeyRotationWindowExactBoundarySucceeds() public {
        uint256 newKeyPriv = 0xB0B;
        address newKey     = vm.addr(newKeyPriv);
        uint64  windowBlocks = 10;

        vm.prank(admin);
        orch.initiateKeyRotation(newKey, windowBlocks);
        uint64 windowEnd = orch.rotation_window_end_block();

        vm.roll(windowEnd); // exactly at the boundary block -- pending key must still work

        bytes32 canaryId = keccak256("canary");
        bytes memory bp  = _buildBlueprintWithBaseFee(canaryId, 0, type(uint256).max);
        bytes memory sig = _sign(bp, newKeyPriv);

        orch.execute(bp, sig); // must NOT revert -- _acceptsKey uses block.number <= end
    }

    function test_KeyRotationWindowOneBlockPastExpiryReverts() public {
        uint256 newKeyPriv = 0xB0B2;
        address newKey     = vm.addr(newKeyPriv);
        uint64  windowBlocks = 10;

        vm.prank(admin);
        orch.initiateKeyRotation(newKey, windowBlocks);
        uint64 windowEnd = orch.rotation_window_end_block();

        vm.roll(windowEnd + 1); // one block past the boundary

        bytes32 canaryId = keccak256("canary");
        bytes memory bp  = _buildBlueprintWithBaseFee(canaryId, 0, type(uint256).max);
        bytes memory sig = _sign(bp, newKeyPriv);

        vm.expectRevert(OmegaOrchestrator.InvalidSignature.selector);
        orch.execute(bp, sig);
    }

    // ── Documents: finalizeKeyRotation() does not check vault.pendingProofCount() ──

    /// @notice OmegaVault.pendingProofCount()'s own doc comment states it is "Exposed so
    ///         OTHER contracts (e.g. Orchestrator, before finalizing a key rotation) can
    ///         confirm nothing is stuck mid-flight in this Vault." OmegaOrchestrator does not
    ///         import ReconciliationRotationGate and finalizeKeyRotation() does not call
    ///         vault.pendingProofCount() (or anything else on the Vault) before completing.
    ///         This test documents that CURRENT behavior -- rotation completes regardless --
    ///         rather than asserting it as a failure, because releaseProfit() is permissionless
    ///         and independent of execution_key, so an already-deposited pending profit is not
    ///         actually endangered by the Orchestrator's key changing underneath it. Flagging
    ///         this as a comment/behavior mismatch worth a deliberate decision either way, not
    ///         as a proven exploit.
    function test_FinalizeKeyRotationSucceedsRegardlessOfVaultPendingProofs() public {
        bytes32 bpHash = keccak256("rotation-gap-demo");

        token.mint(address(orch), 5 ether);
        vm.startPrank(address(orch));
        token.approve(address(vault), 5 ether);
        vault.receivePendingProfit(bpHash, 5 ether);
        vm.stopPrank();

        assertEq(vault.pendingProofCount(), 1, "vault should report one profit pending release");

        address newKey = makeAddr("rotationGapNewKey");
        vm.prank(admin);
        orch.initiateKeyRotation(newKey, 100);

        vm.prank(admin);
        orch.finalizeKeyRotation(); // succeeds today, with no reference to vault state at all

        assertEq(orch.execution_key(), newKey, "rotation completed despite pending vault proof");
    }

    // ── Pause / unpause role gating ─────────────────────────────────────────────

    function test_OnlyEmergencyRoleCanPause() public {
        vm.prank(makeAddr("attacker"));
        vm.expectRevert();
        orch.emergencyPause();
    }

    function test_OnlyAdminCanUnpause() public {
        vm.prank(admin);
        orch.emergencyPause();

        vm.prank(makeAddr("attacker"));
        vm.expectRevert();
        orch.unpause();

        vm.prank(admin);
        orch.unpause(); // admin holds DEFAULT_ADMIN_ROLE -- must succeed
        assertFalse(orch.paused());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared generic-swap mock (address,address,uint256,uint256,address) -> uint256
//
// Implements the same swap(...) shape used by SimpleArb, MultiStepArb (DEX_UNIV2 / fallback),
// LiquidationArb's collateral->debt router, and MevOfa's backrun legs. Pulls `amountIn` of
// `tokenIn` from the caller (which must have approved this pool -- every one of those
// contracts does, via forceApprove, before making the call) and mints `amountIn + 1` of
// `tokenOut` to `recipient`: a deterministic 1-wei profit per call. Minting rather than
// trading against a pre-funded reserve is the same test-double simplification
// OmegaSystem.t.sol's own MockBalancerVault already uses -- the thing under test in each
// consuming contract is ITS OWN logic (hop count, token chain, profit accounting, or -- for
// the tests below -- which raw amount gets passed into a downstream call), not real DEX
// pricing.
// ─────────────────────────────────────────────────────────────────────────────

contract MockSwapPool {
    function swap(
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 /* minOut */,
        address recipient
    ) external returns (uint256 amountOut) {
        MockERC20(tokenIn).transferFrom(msg.sender, address(this), amountIn);
        amountOut = amountIn + 1;
        MockERC20(tokenOut).mint(recipient, amountOut);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// MultiStepArb boundary tests
// ─────────────────────────────────────────────────────────────────────────────

contract MultiStepArbBoundaryTest is Test {
    MultiStepArb  msa;
    MockERC20     token;
    MockSwapPool  pool;
    address       orch = makeAddr("orch");

    function setUp() public {
        msa   = new MultiStepArb(orch);
        token = new MockERC20();
        pool  = new MockSwapPool();
    }

    /// @notice Exactly MAX_HOPS (8) hops must succeed -- OmegaSystem.t.sol's
    ///         test_RejectsTooManyHops already covers 9 hops reverting, but nothing
    ///         previously confirmed the boundary itself (8) actually goes through.
    ///         All 8 hops trade the same token through the same mock pool (token_in ==
    ///         token_out on every hop) purely so the token-chain-continuity check and the
    ///         route-closes check are trivially satisfied without needing 8 distinct tokens --
    ///         the thing being tested is the hop COUNT boundary, not routing correctness across
    ///         different assets (that's what test_RejectsTokenChainMismatch already covers).
    function test_MaxHopsExactBoundarySucceeds() public {
        uint256 flashloanAmount = 1000;
        token.mint(address(msa), flashloanAmount);

        MultiStepArb.Hop[] memory route = new MultiStepArb.Hop[](8);
        for (uint256 i = 0; i < 8; i++) {
            route[i] = MultiStepArb.Hop({
                pool:              address(pool),
                token_in:          address(token),
                token_out:         address(token),
                amount_in:         0, // use running balance from the previous hop
                dex_type:          0, // DEX_UNIV2 -- generic swap(...) signature
                univ3_fee:         0,
                curve_i:           int128(0),
                curve_j:           int128(0),
                balancer_pool_id:  bytes32(0)
            });
        }

        bytes memory callData = abi.encode(route, uint256(0)); // min_profit = 0

        vm.prank(orch);
        uint256 netOutput = msa.execute(callData, flashloanAmount);

        // Each hop adds exactly 1 wei of profit (see MockSwapPool) -- 8 hops -> +8 total.
        assertEq(netOutput, flashloanAmount + 8, "8-hop route must succeed and compound profit");
        assertEq(token.balanceOf(orch), netOutput, "final output must be sent back to orchestrator");
    }

    // (test_RejectsTooManyHops / test_RejectsZeroHops / test_RejectsTokenChainMismatch /
    //  test_OnlyOrchestratorCanExecute in OmegaSystem.t.sol already cover the other paths.)
}

// ─────────────────────────────────────────────────────────────────────────────
// LiquidationArb: Compound leg debtToCover discrepancy
// ─────────────────────────────────────────────────────────────────────────────

/// @dev Minimal Compound v3 Comet double. `absorb` just records that it was called (real
///      Comet makes the position liquidatable; we don't need that mechanic to prove the point
///      here). `buyCollateral` is the one that matters: it records exactly which `baseAmount`
///      LiquidationArb passed in, and mints that many collateral tokens to the recipient --
///      standing in for "however much collateral Compound would actually hand over for that
///      base-token spend."
contract MockCompoundComet {
    uint256 public lastBuyCollateralBaseAmount;
    address public lastBuyCollateralAsset;
    bool    public absorbCalled;

    function absorb(address /* absorber */, address[] calldata /* accounts */) external {
        absorbCalled = true;
    }

    function buyCollateral(
        address asset,
        uint256 /* minAmount */,
        uint256 baseAmount,
        address recipient
    ) external {
        lastBuyCollateralAsset       = asset;
        lastBuyCollateralBaseAmount  = baseAmount;
        MockERC20(asset).mint(recipient, baseAmount);
    }
}

contract LiquidationArbCompoundBoundaryTest is Test {
    LiquidationArb     liqArb;
    MockCompoundComet  comet;
    MockSwapPool       router;
    MockERC20          debtToken;
    MockERC20          collateralToken;

    address orch = makeAddr("orch");
    address user = makeAddr("borrower");

    function setUp() public {
        comet  = new MockCompoundComet();
        router = new MockSwapPool();

        debtToken       = new MockERC20();
        collateralToken = new MockERC20();

        liqArb = new LiquidationArb(
            orch,
            makeAddr("aavePool"),    // unused on this test's code path
            address(comet),
            makeAddr("morphoBlue"),  // unused on this test's code path
            address(0),              // eulerV2 not deployed for this test
            address(router)
        );
    }

    /// @notice Documents a real discrepancy: LiquidationArb._liquidateCompound's
    ///         `buyCollateral` call uses `flashloanAmount`, NOT the caller-supplied
    ///         `debtToCover` -- every other protocol branch in this same contract
    ///         (_liquidateAave, _liquidateEuler) uses `debtToCover` for its equivalent
    ///         parameter. This test sets `debtToCover` to a value deliberately different
    ///         from `flashloanAmount` and shows the mock Comet receives `flashloanAmount`,
    ///         never `debtToCover` -- so a caller who sets `debtToCover` expecting it to
    ///         bound or control the size of the Compound leg silently gets `flashloanAmount`
    ///         used instead. Not asserted here as an obvious bug (Compound's absorb/
    ///         buyCollateral flow may genuinely need to key off the flashloaned amount rather
    ///         than a caller-chosen debt figure), but the inconsistency with the Aave/Euler
    ///         branches is real and worth a deliberate decision rather than silent drift.
    function test_CompoundLiquidationUsesFlashloanAmountNotDebtToCover() public {
        uint256 flashloanAmount = 500 ether;
        uint256 debtToCover     = 777 ether; // deliberately different from flashloanAmount

        debtToken.mint(address(liqArb), flashloanAmount);

        bytes memory callData = abi.encode(
            LiquidationArb.Protocol.CompoundV3,
            address(collateralToken),
            address(debtToken),
            user,
            debtToCover,
            uint256(0), // minProfit
            bytes("")   // extraData -- unused for Compound, per LiquidationArb's own docstring
        );

        vm.prank(orch);
        liqArb.execute(callData, flashloanAmount);

        assertTrue(comet.absorbCalled(), "absorb must have been called");
        assertEq(comet.lastBuyCollateralAsset(), address(collateralToken));
        assertEq(
            comet.lastBuyCollateralBaseAmount(),
            flashloanAmount,
            "Compound leg used flashloanAmount as baseAmount"
        );
        assertTrue(
            comet.lastBuyCollateralBaseAmount() != debtToCover,
            "Compound leg did NOT use the caller-supplied debtToCover, unlike the Aave/Euler branches"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// MevOfa: self-reported price-impact trust boundary
// ─────────────────────────────────────────────────────────────────────────────

contract MevOfaBoundaryTest is Test {
    MevOfa       mevOfa;
    MockSwapPool pool;
    MockERC20    tokenIn;
    MockERC20    tokenOut;

    address orch = makeAddr("orch");

    uint256 constant MIN_IMPACT_BPS = 50;

    function setUp() public {
        mevOfa   = new MevOfa(orch, MIN_IMPACT_BPS);
        pool     = new MockSwapPool();
        tokenIn  = new MockERC20();
        tokenOut = new MockERC20();
    }

    function _buildCalldata(
        uint256 estimatedImpactBps,
        uint256 amountIn
    ) internal view returns (bytes memory) {
        return abi.encode(
            bytes32("tx"),
            address(pool),
            address(tokenIn),
            address(tokenOut),
            amountIn,
            uint256(0), // min_profit
            uint256(0), // max_slippage_bps
            estimatedImpactBps
        );
    }

    /// @notice Complements MevOfaTest.test_RejectsBelowMinPriceImpact (which covers 49 < 50
    ///         reverting): a claim of EXACTLY MIN_PRICE_IMPACT_BPS must succeed, not revert --
    ///         the check is `< MIN_PRICE_IMPACT_BPS`, not `<=`.
    function test_PriceImpactExactThresholdSucceeds() public {
        uint256 amountIn = 100 ether;
        tokenIn.mint(address(mevOfa), amountIn);

        bytes memory callData = _buildCalldata(MIN_IMPACT_BPS, amountIn);

        vm.prank(orch);
        mevOfa.execute(callData, amountIn); // must NOT revert
    }

    /// @notice Makes concrete the trust-boundary note in MevOfa.sol's own header:
    ///         `estimated_price_impact_bps` is checked as a caller-SUPPLIED number against
    ///         MIN_PRICE_IMPACT_BPS -- the contract never reads pool reserves or derives an
    ///         impact figure from what the swaps in this same call actually did. Both calls
    ///         below hit the IDENTICAL mock pool behavior (MockSwapPool's fixed, deterministic
    ///         +1-wei-per-call profit) and therefore produce IDENTICAL realized output, yet one
    ///         claims a price impact of exactly the minimum (50 bps) and the other claims a
    ///         wildly disproportionate 1,000,000 bps (10,000%). Both succeed, with identical
    ///         results, because the check only ever compares the claim to the threshold -- it
    ///         cannot, and does not, distinguish a truthful claim from a fabricated one.
    function test_PriceImpactCheckAcceptsAnyClaimRegardlessOfRealizedOutcome() public {
        uint256 amountIn = 100 ether;

        tokenIn.mint(address(mevOfa), amountIn);
        bytes memory lowClaimCalldata = _buildCalldata(MIN_IMPACT_BPS, amountIn);
        vm.prank(orch);
        uint256 outLowClaim = mevOfa.execute(lowClaimCalldata, amountIn);

        // Fresh funding for the second call -- same pool, same amounts, only the claimed
        // price impact changes.
        tokenIn.mint(address(mevOfa), amountIn);
        bytes memory hugeClaimCalldata = _buildCalldata(1_000_000, amountIn);
        vm.prank(orch);
        uint256 outHugeClaim = mevOfa.execute(hugeClaimCalldata, amountIn);

        assertEq(
            outLowClaim,
            outHugeClaim,
            "realized output is identical regardless of the claimed price impact"
        );
    }
}