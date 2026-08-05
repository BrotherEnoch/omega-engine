// contracts/test/OmegaSystem.t.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "forge-std/console.sol";

import {OmegaOrchestrator} from "../src/OmegaOrchestrator.sol";
import {OmegaVault}         from "../src/OmegaVault.sol";
import {OpilToken}          from "../src/OpilToken.sol";
import {CanaryArb}          from "../src/strategies/CanaryArb.sol";
import {SimpleArb}          from "../src/strategies/SimpleArb.sol";
import {LiquidationArb}     from "../src/strategies/LiquidationArb.sol";
import {MultiStepArb}       from "../src/strategies/MultiStepArb.sol";
import {MevOfa}             from "../src/strategies/MevOfa.sol";

// ─────────────────────────────────────────────────────────────────────────────
// Mock Contracts
//
// NOTE ON THIS REVISION: every mock/helper below was rewritten against the
// CURRENT OmegaOrchestrator.sol / OmegaVault.sol ABIs in this delivery, not
// against whatever earlier revision the original test file predated. This is
// not an OpenZeppelin-version concern -- it's the test file having drifted
// out of sync with three real changes to the production contracts:
//   1. submitProof(bytes32, bytes32, bytes) -- publicInputsHash binding (C4 fix).
//   2. OmegaOrchestrator's constructor takes 6 args (added _aavePool) and its
//      blueprint layout is a 10-field tuple (added providerType, flashloanToken,
//      providerContract, maxBaseFee) -- not the 6-field layout this file
//      previously encoded.
//   3. receivePendingProfit now performs a real safeTransferFrom instead of a
//      bare mapping increment -- the caller (Orchestrator, or in these tests,
//      the `orchestrator` test address standing in for it) must actually hold
//      balance and have granted allowance.
// ─────────────────────────────────────────────────────────────────────────────

contract MockERC20 {
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    uint256 public totalSupply;

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
        totalSupply    += amount;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        balanceOf[msg.sender] -= amount;
        balanceOf[to]         += amount;
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        allowance[from][msg.sender] -= amount;
        balanceOf[from]             -= amount;
        balanceOf[to]               += amount;
        return true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }
}

/// @dev Matches the REAL IStarkVerifier.verify(bytes,bytes32,bytes32) signature -- the
///      prior 2-arg version here (bytes,bytes32) did not match OmegaVault.sol's actual
///      interface at all; any real call through it would have hit no matching selector
///      and reverted, regardless of `shouldPass`.
contract MockStarkVerifier {
    bool public shouldPass = true;
    function setShouldPass(bool v) external { shouldPass = v; }
    function verify(bytes calldata, bytes32, bytes32) external view returns (bool) {
        return shouldPass;
    }
}

/// @dev Minimal interface for the Balancer-style callback OmegaOrchestrator implements
///      (receiveFlashLoan). Declared here, not imported, since OmegaOrchestrator.sol
///      itself doesn't expose a named interface for its own callback -- it's just a
///      public function matching Balancer's real ABI.
interface IFlashLoanRecipient {
    function receiveFlashLoan(
        address[] calldata tokens,
        uint256[] calldata amounts,
        uint256[] calldata feeAmounts,
        bytes calldata userData
    ) external;
}

/// @dev Replaces the prior MockFlashloanProvider, which implemented a
///      `flashloan(address,bytes,uint256)` function that matches NONE of the three real
///      provider interfaces OmegaOrchestrator._executeFlashloan actually dispatches to.
///      This mock implements Balancer V2's real `flashLoan` entry point and calls back
///      into the recipient exactly as the real Vault would (mint-to-simulate-lending,
///      since this mock has no pre-funded reserve of its own to draw from -- MockERC20's
///      mint is unguarded, so this is a legitimate simplification for a test double, not
///      a shortcut that changes what's being tested: the thing under test is
///      OmegaOrchestrator's OWN accounting and callback-authentication logic, not
///      Balancer's).
contract MockBalancerVault {
    function flashLoan(
        address recipient,
        address[] calldata tokens,
        uint256[] calldata amounts,
        bytes calldata userData
    ) external {
        uint256[] memory feeAmounts = new uint256[](amounts.length);
        for (uint256 i = 0; i < tokens.length; i++) {
            MockERC20(tokens[i]).mint(recipient, amounts[i]);
        }
        IFlashLoanRecipient(recipient).receiveFlashLoan(tokens, amounts, feeAmounts, userData);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OmegaVault Tests
// ─────────────────────────────────────────────────────────────────────────────

contract OmegaVaultTest is Test {
    OmegaVault          public vault;
    MockERC20           public token;
    MockStarkVerifier   public verifier;

    address admin        = makeAddr("admin");
    address pil          = makeAddr("pil");
    address daoFeeAddr   = makeAddr("dao");
    address orchestrator = makeAddr("orchestrator");
    address depthUpdater = makeAddr("depthUpdater");

    uint256 constant PER_CAP   = 50 ether;
    uint256 constant DAILY_CAP = 500 ether;

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

        // FIX: receivePendingProfit now performs a real safeTransferFrom(msg.sender, ...)
        // instead of the prior bare mapping increment -- `orchestrator` (the caller of
        // every receivePendingProfit call below) must actually hold balance AND have
        // approved the Vault, or every one of those calls reverts on insufficient
        // allowance regardless of any other setup. Minted/approved once here, generously,
        // rather than per-test, since every test in this contract that deposits pending
        // profit needs it.
        token.mint(orchestrator, 1_000_000 ether);
        vm.prank(orchestrator);
        token.approve(address(vault), type(uint256).max);

        // Retained from the original setup: extra idle balance sitting in the Vault ahead
        // of any deposit. Harmless under the current accounting (requireVaultReconciliation
        // only checks balance >= expected, never balance == expected, and totalPendingProfit
        // tracking is independent of raw balance) -- left in place rather than removed, to
        // keep this diff minimal where the original intent doesn't conflict with anything.
        token.mint(address(vault), 1000 ether);
    }

    /// @dev Computes the real publicInputsHash for a given (bpHash, netProfit) pair the
    ///      same way OmegaVault.computePublicInputsHash does internally -- calling the
    ///      Vault's own public function rather than re-deriving the encoding by hand, so
    ///      this test can never silently drift from the real binding logic.
    function _boundHash(bytes32 bpHash, uint256 netProfit) internal view returns (bytes32) {
        return vault.computePublicInputsHash(bpHash, netProfit);
    }

    function test_C6_ReleaseRevertsWithoutProof() public {
        bytes32 bpHash = keccak256("bp1");

        vm.prank(orchestrator);
        vault.receivePendingProfit(bpHash, 1 ether);

        vm.prank(depthUpdater);
        vault.updateConfirmationDepth(bpHash, 12);

        vm.expectRevert(abi.encodeWithSelector(OmegaVault.ProofNotVerified.selector, bpHash));
        vault.releaseProfit(bpHash);
    }

    function test_C6_ReleaseRevertsWithInsufficientDepth() public {
        bytes32 bpHash = keccak256("bp2");

        vm.prank(orchestrator);
        vault.receivePendingProfit(bpHash, 1 ether);

        vault.submitProof(bpHash, _boundHash(bpHash, 1 ether), bytes("proof"));

        vm.prank(depthUpdater);
        vault.updateConfirmationDepth(bpHash, 11);

        vm.expectRevert(
            abi.encodeWithSelector(OmegaVault.InsufficientDepth.selector, uint8(11), uint256(12))
        );
        vault.releaseProfit(bpHash);
    }

    function test_C9_DaoFeeSplitCorrect() public {
        bytes32 bpHash = keccak256("bp3");
        uint256 net    = 10 ether;

        vm.prank(orchestrator);
        vault.receivePendingProfit(bpHash, net);
        vault.submitProof(bpHash, _boundHash(bpHash, net), bytes("proof"));

        vm.prank(depthUpdater);
        vault.updateConfirmationDepth(bpHash, 12);

        uint256 pilBefore = token.balanceOf(pil);
        uint256 daoBefore = token.balanceOf(daoFeeAddr);

        vault.releaseProfit(bpHash);

        uint256 pilShare = token.balanceOf(pil)        - pilBefore;
        uint256 daoShare = token.balanceOf(daoFeeAddr) - daoBefore;

        assertEq(pilShare + daoShare, net,             "C9: split must equal netProfit");
        assertEq(daoShare, net * 500 / 10_000,         "C9: dao fee must be 5%");
        assertEq(pilShare, net - daoShare,             "C9: pil share must be remainder");
        assertTrue(daoShare <= net / 10,               "C9: dao fee must not exceed 10%");
    }

    function test_C9_DaoFeeCannotExceed10Pct() public {
        vm.prank(admin);
        vm.expectRevert(
            abi.encodeWithSelector(OmegaVault.DaoFeeExceedsMax.selector, 1001, 1000)
        );
        vault.queueDaoFeeBpsChange(1001);
    }

    function test_NoDoubleRelease() public {
        bytes32 bpHash = keccak256("bp4");

        vm.prank(orchestrator);
        vault.receivePendingProfit(bpHash, 1 ether);
        vault.submitProof(bpHash, _boundHash(bpHash, 1 ether), bytes("proof"));

        vm.prank(depthUpdater);
        vault.updateConfirmationDepth(bpHash, 12);

        vault.releaseProfit(bpHash);

        vm.expectRevert(abi.encodeWithSelector(OmegaVault.AlreadyReleased.selector, bpHash));
        vault.releaseProfit(bpHash);
    }

    function test_PerTransferCapEnforced() public {
        bytes32 bpHash   = keccak256("bp5");
        uint256 tooLarge = PER_CAP + 1;

        vm.prank(orchestrator);
        vault.receivePendingProfit(bpHash, tooLarge);
        vault.submitProof(bpHash, _boundHash(bpHash, tooLarge), bytes("proof"));

        vm.prank(depthUpdater);
        vault.updateConfirmationDepth(bpHash, 12);

        vm.expectRevert(
            abi.encodeWithSelector(OmegaVault.ExceedsPerTransferCap.selector, tooLarge, PER_CAP)
        );
        vault.releaseProfit(bpHash);
    }

    function test_DaoFeeAddressTimelockEnforced() public {
        address newDao = makeAddr("newDao");

        vm.prank(admin);
        vault.queueDaoFeeAddressChange(newDao);

        vm.prank(admin);
        vm.expectRevert();
        vault.executeDaoFeeAddressChange();

        vm.warp(block.timestamp + 49 hours);
        vm.prank(admin);
        vault.executeDaoFeeAddressChange();

        assertEq(vault.dao_fee_address(), newDao);
    }

    function test_DepthNeverDecreases() public {
        bytes32 bpHash = keccak256("bp6");

        vm.prank(depthUpdater);
        vault.updateConfirmationDepth(bpHash, 10);
        assertEq(vault.confirmation_depth(bpHash), 10);

        vm.prank(depthUpdater);
        vault.updateConfirmationDepth(bpHash, 5);
        assertEq(vault.confirmation_depth(bpHash), 10, "depth must not decrease");
    }

    function test_DailyCapResets() public {
        for (uint256 i = 0; i < 10; i++) {
            bytes32 bpHash = keccak256(abi.encode("daily", i));
            vm.prank(orchestrator);
            vault.receivePendingProfit(bpHash, 49 ether);
            vault.submitProof(bpHash, _boundHash(bpHash, 49 ether), bytes("proof"));
            vm.prank(depthUpdater);
            vault.updateConfirmationDepth(bpHash, 12);
            try vault.releaseProfit(bpHash) {} catch {}
        }
        vm.warp(block.timestamp + 1 days + 1);
        assertEq(vault.dailyCapRemaining(), DAILY_CAP);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OpilToken Tests (unchanged -- OpilToken's public interface was not affected by
// any of the fixes described in this file's header)
// ─────────────────────────────────────────────────────────────────────────────

contract OpilTokenTest is Test {
    OpilToken public opil;
    address admin = makeAddr("admin");
    address pil   = makeAddr("pil");
    address alice = makeAddr("alice");
    address bob   = makeAddr("bob");

    function setUp() public {
        vm.prank(admin);
        opil = new OpilToken(pil, admin);
    }

    function test_C7_VotesZeroWithinLockWindow() public {
        vm.prank(pil);
        opil.mint(alice, 100e18);
        assertEq(opil.getVotes(alice), 0, "C7: votes must be 0 within 7 days");
    }

    function test_C7_VotesAvailableAfterLock() public {
        vm.prank(pil);
        opil.mint(alice, 100e18);

        vm.prank(alice);
        opil.delegate(alice);

        vm.warp(block.timestamp + 7 days + 1);
        assertTrue(opil.getVotes(alice) > 0, "C7: votes must be available after lock");
    }

    function test_C7_TransferResetsLock() public {
        vm.prank(pil);
        opil.mint(alice, 100e18);

        vm.prank(alice);
        opil.delegate(alice);

        vm.warp(block.timestamp + 8 days);
        assertTrue(opil.getVotes(alice) > 0);

        vm.prank(alice);
        bool ok = opil.transfer(bob, 50e18);
        assertTrue(ok);

        vm.prank(bob);
        opil.delegate(bob);

        assertEq(opil.getVotes(bob), 0, "C7: transfer must reset vote lock for recipient");
    }

    function test_OnlyPilCanMint() public {
        vm.prank(alice);
        vm.expectRevert();
        opil.mint(alice, 100e18);
    }

    function test_C8_SupplyIntegrity() public {
        vm.prank(pil);
        opil.mint(alice, 100e18);
        vm.prank(pil);
        opil.mint(bob, 200e18);

        assertEq(opil.totalSupply(), 300e18, "C8: totalSupply must equal sum of balances");
        assertEq(opil.balanceOf(alice) + opil.balanceOf(bob), opil.totalSupply());
    }

    function test_C8_BurnReducesSupply() public {
        vm.prank(pil);
        opil.mint(alice, 100e18);
        vm.prank(pil);
        opil.burn(alice, 40e18);

        assertEq(opil.totalSupply(), 60e18);
        assertEq(opil.balanceOf(alice), 60e18);
    }

    function test_VoteUnlockHelpers() public {
        vm.prank(pil);
        opil.mint(alice, 100e18);

        assertTrue(opil.voteUnlockIn(alice) > 0, "Should have time remaining");

        vm.warp(block.timestamp + 8 days);
        assertEq(opil.voteUnlockIn(alice), 0, "Lock should be expired");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OmegaOrchestrator Tests
// ─────────────────────────────────────────────────────────────────────────────

contract OmegaOrchestratorTest is Test {
    OmegaOrchestrator     public orch;
    MockBalancerVault     public flashloan;
    OmegaVault            public vault;
    MockERC20             public token;
    MockStarkVerifier     public verifier;
    CanaryArb             public canary;

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

        // Deploy vault as admin so admin holds DEFAULT_ADMIN_ROLE.
        // Orchestrator address is not yet known, so pass address(0) as placeholder.
        vm.prank(admin);
        vault = new OmegaVault(
            pil, daoFeeAddr, address(verifier), address(token),
            admin, address(0),
            50 ether, 500 ether
        );

        // FIX: real constructor takes 6 args (chainId, vault, flashloanProvider,
        // aavePool, executionKey, admin) -- previously called with 5, missing
        // _aavePool. Passed as address(0): none of these tests exercise the AaveV3
        // provider branch, so it's simply unused, not a placeholder standing in for
        // a real value that matters here.
        vm.prank(admin);
        orch = new OmegaOrchestrator(
            uint64(block.chainid),
            address(vault),
            address(flashloan),
            address(0),
            execKey,
            admin
        );

        // Cache the role selector BEFORE entering the prank context.
        // vault.ORCHESTRATOR_ROLE() is a staticcall; calling it inside vm.prank
        // consumes the prank before grantRole executes, causing the revert.
        bytes32 orchestratorRole = vault.ORCHESTRATOR_ROLE();
        vm.prank(admin);
        vault.grantRole(orchestratorRole, address(orch));

        // Deploy and register canary strategy.
        canary = new CanaryArb(address(orch));
        bytes32 canaryId = keccak256("canary");
        vm.prank(admin);
        orch.registerStrategy(canaryId, address(canary));
    }

    function test_C1_WrongChainReverts() public {
        vm.chainId(999);
        bytes memory bp  = _buildBlueprint(keccak256("canary"), 0);
        bytes memory sig = _sign(bp);
        vm.expectRevert();
        orch.execute(bp, sig);
    }

    function test_C2_ReplayReverts() public {
        bytes32 canaryId = keccak256("canary");
        bytes memory bp  = _buildBlueprint(canaryId, 0);
        bytes memory sig = _sign(bp);

        orch.execute(bp, sig);

        bytes32 bpHash = keccak256(abi.encode(address(orch), uint64(block.chainid), bp));
        vm.expectRevert(
            abi.encodeWithSelector(OmegaOrchestrator.ReplayDetected.selector, bpHash)
        );
        orch.execute(bp, sig);
    }

    function test_C3_WrongNonceReverts() public {
        bytes32 canaryId = keccak256("canary");
        // FIX: full 10-field layout (was a stale 6-field encoding). Field values other
        // than `nonce` itself are irrelevant to what this test checks -- execute()
        // reverts at the nonce check (step 6), before the flashloan-token consistency
        // check (step 9) or the flashloan itself are ever reached -- but the ABI decode
        // in step 2 still has to succeed against the REAL 10-field tuple shape, or this
        // test would fail with a raw decode error instead of the InvalidNonce it's
        // actually testing for.
        bytes memory bp = abi.encode(
            uint64(block.number + 100),                          // expiry_block
            uint64(5),                                           // nonce (wrong -- expected 0)
            canaryId,                                            // strategyId
            OmegaOrchestrator.FlashloanProviderType.Balancer,     // providerType
            address(token),                                      // flashloanToken
            address(0),                                          // providerContract
            abi.encode(address(token)),                          // strategyCalldata
            uint256(0),                                          // flashloanAmount
            uint256(0),                                          // minNetProfit
            type(uint256).max                                    // maxBaseFee (opt out)
        );
        bytes memory sig = _sign(bp);
        vm.expectRevert(
            abi.encodeWithSelector(OmegaOrchestrator.InvalidNonce.selector, uint64(5), uint64(0))
        );
        orch.execute(bp, sig);
    }

    function test_InvalidSigReverts() public {
        bytes32 canaryId = keccak256("canary");
        bytes memory bp     = _buildBlueprint(canaryId, 0);
        bytes memory badSig = _signWithKey(bp, 0xBADBAD);
        vm.expectRevert(OmegaOrchestrator.InvalidSignature.selector);
        orch.execute(bp, badSig);
    }

    function test_UnknownStrategyReverts() public {
        bytes32 unknownId = keccak256("unknown");
        bytes memory bp   = _buildBlueprint(unknownId, 0);
        bytes memory sig  = _sign(bp);
        vm.expectRevert(
            abi.encodeWithSelector(OmegaOrchestrator.UnknownStrategy.selector, unknownId)
        );
        orch.execute(bp, sig);
    }

    function test_C5_FrozenStrategyReverts() public {
        bytes32 canaryId = keccak256("canary");
        vm.prank(admin);
        orch.freezeStrategy(canaryId);

        bytes memory bp  = _buildBlueprint(canaryId, 0);
        bytes memory sig = _sign(bp);
        vm.expectRevert(
            abi.encodeWithSelector(OmegaOrchestrator.StrategyIsFrozen.selector, canaryId)
        );
        orch.execute(bp, sig);
    }

    function test_EmergencyPauseBlocks() public {
        vm.prank(admin);
        orch.emergencyPause();

        bytes32 canaryId = keccak256("canary");
        bytes memory bp  = _buildBlueprint(canaryId, 0);
        bytes memory sig = _sign(bp);

        vm.expectRevert("Pausable: paused");
        orch.execute(bp, sig);
    }

    function test_KeyRotationWindow() public {
        address newKey = makeAddr("newKey");
        vm.prank(admin);
        orch.initiateKeyRotation(newKey, 100);

        bytes32 canaryId = keccak256("canary");
        bytes memory bp  = _buildBlueprint(canaryId, 0);
        bytes memory sig = _sign(bp);
        orch.execute(bp, sig);
    }

    function test_BaseFeeGuardReverts() public {
        // New coverage: the maxBaseFee field this test file previously had no way to
        // exercise at all, since it predated the field's existence.
        bytes32 canaryId = keccak256("canary");
        bytes memory bp = abi.encode(
            uint64(block.number + 100),
            uint64(0),
            canaryId,
            OmegaOrchestrator.FlashloanProviderType.Balancer,
            address(token),
            address(0),
            abi.encode(address(token)),
            uint256(0),
            uint256(0),
            uint256(0) // maxBaseFee = 0 -- any nonzero block.basefee must revert
        );
        bytes memory sig = _sign(bp);
        vm.fee(1); // ensure block.basefee > 0
        vm.expectRevert(
            abi.encodeWithSelector(OmegaOrchestrator.BaseFeeTooHigh.selector, uint256(1), uint256(0))
        );
        orch.execute(bp, sig);
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// @dev FIX: encodes the full current 10-field blueprint layout (expiry_block, nonce,
    ///      strategyId, providerType, flashloanToken, providerContract, strategyCalldata,
    ///      flashloanAmount, minNetProfit, maxBaseFee) -- the prior version of this helper
    ///      encoded only 6 fields, matching a layout OmegaOrchestrator.execute() no longer
    ///      decodes against. flashloanToken is fixed to address(token) (the Vault's own
    ///      profit_token) since execute() reverts with TokenMismatchWithVault otherwise;
    ///      strategyCalldata is abi.encode(address(token)) since CanaryArb.execute requires
    ///      it to know which token to transfer back.
    function _buildBlueprint(bytes32 stratId, uint64 nonce) internal view returns (bytes memory) {
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
            type(uint256).max
        );
    }

    function _sign(bytes memory bp) internal view returns (bytes memory) {
        return _signWithKey(bp, execPrivKey);
    }

    function _signWithKey(bytes memory bp, uint256 key) internal view returns (bytes memory) {
        // FIX: the domain-separated hash execute() actually verifies against is
        // keccak256(abi.encode(address(this), EXPECTED_CHAIN_ID, blueprintCalldata)),
        // not keccak256(blueprintCalldata) alone (see OmegaOrchestrator.sol's own
        // blueprint-layout comment). Signing the wrong hash would make every
        // legitimately-signed test blueprint fail signature recovery.
        bytes32 hash = keccak256(abi.encode(address(orch), uint64(block.chainid), bp));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, hash);
        return abi.encodePacked(r, s, v);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CanaryArb Tests (unchanged -- exercises CanaryArb directly, no Orchestrator/Vault
// interaction, unaffected by anything described in this file's header)
// ─────────────────────────────────────────────────────────────────────────────

contract CanaryArbTest is Test {
    CanaryArb canary;
    MockERC20 token;
    address orch = makeAddr("orch");

    function setUp() public {
        canary = new CanaryArb(orch);
        token  = new MockERC20();
        // FIX: CanaryArb.execute() requires non-empty strategyCalldata that decodes to a
        // token address (see CanaryArb.sol's own docstring — the prior version ignored
        // strategyCalldata entirely; the current one needs to know which token to
        // transfer back). It also actually calls safeTransfer(msg.sender, flashloanAmount)
        // on that token, so the canary contract must hold enough balance to cover every
        // execute() call below, not just decode a valid address. Minted generously once
        // here rather than per-test.
        token.mint(address(canary), 1_000_000 ether);
    }

    function test_ReturnsExactFlashloanAmount() public {
        vm.prank(orch);
        uint256 out = canary.execute(abi.encode(address(token)), 1 ether);
        assertEq(out, 1 ether, "Canary must return exact flashloan amount");
    }

    function test_EmitsCanaryPing() public {
        vm.prank(orch);
        vm.expectEmit(true, false, false, true);
        emit CanaryArb.CanaryPing(uint64(block.number), 0.0001 ether, 1, true);
        canary.execute(abi.encode(address(token)), 0.0001 ether);
    }

    function test_IncrementsPingCount() public {
        vm.startPrank(orch);
        canary.execute(abi.encode(address(token)), 1 ether);
        canary.execute(abi.encode(address(token)), 1 ether);
        canary.execute(abi.encode(address(token)), 1 ether);
        vm.stopPrank();
        assertEq(canary.ping_count(), 3);
    }

    function test_OnlyOrchestratorCanExecute() public {
        vm.prank(makeAddr("attacker"));
        vm.expectRevert(CanaryArb.OnlyOrchestrator.selector);
        canary.execute(abi.encode(address(token)), 1 ether);
    }

    function test_SecondsSinceLastPingInitial() public view {
        assertEq(canary.secondsSinceLastPing(), type(uint256).max);
    }

    function test_SecondsSinceLastPingAfterPing() public {
        vm.prank(orch);
        canary.execute(abi.encode(address(token)), 1 ether);
        vm.warp(block.timestamp + 30);
        assertEq(canary.secondsSinceLastPing(), 30);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// LiquidationArb Tests (unchanged)
// ─────────────────────────────────────────────────────────────────────────────

contract LiquidationArbTest is Test {
    LiquidationArb liqArb;
    address orch       = makeAddr("orch");
    address mockAave   = makeAddr("aave");
    address mockComp   = makeAddr("comp");
    address mockMorpho = makeAddr("morpho");
    address mockRouter = makeAddr("router");
    address mockToken  = makeAddr("token");

    function setUp() public {
        liqArb = new LiquidationArb(
            orch,
            mockAave,
            mockComp,
            mockMorpho,
            address(0),
            mockRouter
        );
    }

    function test_EulerRejectsWhenNotActivated() public {
        address user = makeAddr("user");
        // FIX: LiquidationArb.execute() decodes a 7-field tuple
        // (Protocol, address, address, address, uint256, uint256, bytes) — the trailing
        // `extraData` field (empty for every protocol except MorphoBlue, per
        // LiquidationArb.sol's own docstring) was missing here. Encoding only 6 fields
        // against a decode that expects a trailing dynamic `bytes` produces a raw ABI
        // decode failure, not the EulerNotYetActivated revert this test is actually
        // trying to exercise — which is exactly the "reverted, but without the expected
        // data" failure this produced.
        bytes memory callData = abi.encode(
            LiquidationArb.Protocol.EulerV2,
            mockToken, mockToken, user,
            uint256(1 ether),
            uint256(0),
            bytes("")
        );
        vm.prank(orch);
        vm.expectRevert(LiquidationArb.EulerNotYetActivated.selector);
        liqArb.execute(callData, 1 ether);
    }

    function test_OnlyOrchestratorCanExecute() public {
        vm.prank(makeAddr("attacker"));
        vm.expectRevert(LiquidationArb.OnlyOrchestrator.selector);
        liqArb.execute(bytes("data"), 1 ether);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// MultiStepArb Tests (unchanged)
// ─────────────────────────────────────────────────────────────────────────────

contract MultiStepArbTest is Test {
    MultiStepArb msa;
    address orch = makeAddr("orch");

    function setUp() public {
        msa = new MultiStepArb(orch);
    }

    function test_RejectsZeroHops() public {
        MultiStepArb.Hop[] memory route = new MultiStepArb.Hop[](0);
        bytes memory callData = abi.encode(route, uint256(0));
        vm.prank(orch);
        vm.expectRevert(MultiStepArb.ZeroHops.selector);
        msa.execute(callData, 1 ether);
    }

    function test_RejectsTooManyHops() public {
        MultiStepArb.Hop[] memory route = new MultiStepArb.Hop[](9);
        for (uint i = 0; i < 9; i++) {
            route[i] = MultiStepArb.Hop(
                makeAddr("pool"), makeAddr("t"), makeAddr("t2"),
                0, 0, 0, int128(0), int128(0), bytes32(0)
            );
        }
        bytes memory callData = abi.encode(route, uint256(0));
        vm.prank(orch);
        vm.expectRevert(abi.encodeWithSelector(MultiStepArb.TooManyHops.selector, 9, 8));
        msa.execute(callData, 1 ether);
    }

    function test_RejectsTokenChainMismatch() public {
        address t1 = makeAddr("t1");
        address t2 = makeAddr("t2");
        address t3 = makeAddr("t3");

        MultiStepArb.Hop[] memory route = new MultiStepArb.Hop[](2);
        route[0] = MultiStepArb.Hop(
            makeAddr("pool1"), t1, t2, 0, 0, 0, int128(0), int128(0), bytes32(0)
        );
        route[1] = MultiStepArb.Hop(
            makeAddr("pool2"), t3, t1, 0, 0, 0, int128(0), int128(0), bytes32(0)
        );

        bytes memory callData = abi.encode(route, uint256(0));
        vm.prank(orch);
        vm.expectRevert(abi.encodeWithSelector(MultiStepArb.TokenMismatch.selector, uint256(1)));
        msa.execute(callData, 1 ether);
    }

    function test_OnlyOrchestratorCanExecute() public {
        vm.prank(makeAddr("rogue"));
        vm.expectRevert(MultiStepArb.OnlyOrchestrator.selector);
        msa.execute(bytes("x"), 1 ether);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SimpleArb Tests (NEW -- SimpleArb was imported but had no test coverage at all
// in either the original file or the earlier rewrite of this one. Basic coverage
// added here, matching the depth given to the other simple strategies
// (CanaryArb/MevOfa) rather than the full swap-mock harness LiquidationArb or
// MultiStepArb would need for end-to-end coverage.
// ─────────────────────────────────────────────────────────────────────────────

contract SimpleArbTest is Test {
    SimpleArb simpleArb;
    address orch = makeAddr("orch");

    function setUp() public {
        simpleArb = new SimpleArb(orch);
    }

    function test_OnlyOrchestratorCanExecute() public {
        vm.prank(makeAddr("attacker"));
        vm.expectRevert(SimpleArb.OnlyOrchestrator.selector);
        simpleArb.execute(bytes("x"), 1 ether);
    }

    function test_RevertsOnShortCalldata() public {
        // SimpleArb requires strategyCalldata.length >= 6 * 32 (six ABI-encoded
        // words: pool_a, pool_b, token_in, token_out, amount_in, min_profit).
        vm.prank(orch);
        vm.expectRevert(SimpleArb.InvalidCalldata.selector);
        simpleArb.execute(abi.encode(address(0)), 1 ether); // only 1 word, not 6
    }

    function test_RevertsOnZeroPoolAddress() public {
        bytes memory callData = abi.encode(
            address(0),          // pool_a -- zero, must revert
            makeAddr("pool_b"),
            makeAddr("token_in"),
            makeAddr("token_out"),
            uint256(1 ether),
            uint256(0)
        );
        vm.prank(orch);
        vm.expectRevert(SimpleArb.ZeroAddress.selector);
        simpleArb.execute(callData, 1 ether);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// MevOfa Tests (unchanged)
// ─────────────────────────────────────────────────────────────────────────────

contract MevOfaTest is Test {
    MevOfa  mevOfa;
    address orch = makeAddr("orch");

    function setUp() public {
        mevOfa = new MevOfa(orch, 50);
    }

    function test_RejectsBelowMinPriceImpact() public {
        bytes memory callData = abi.encode(
            bytes32(0),
            makeAddr("pool"),
            makeAddr("tokenIn"),
            makeAddr("tokenOut"),
            uint256(1 ether),
            uint256(0),
            uint256(50),
            uint256(49)
        );
        vm.prank(orch);
        vm.expectRevert(
            abi.encodeWithSelector(MevOfa.PriceImpactTooLow.selector, uint256(49), uint256(50))
        );
        mevOfa.execute(callData, 1 ether);
    }

    function test_OnlyOrchestratorCanExecute() public {
        vm.prank(makeAddr("rogue"));
        vm.expectRevert(MevOfa.OnlyOrchestrator.selector);
        mevOfa.execute(bytes("x"), 1 ether);
    }
}