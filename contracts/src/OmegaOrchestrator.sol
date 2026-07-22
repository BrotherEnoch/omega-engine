// contracts/src/OmegaOrchestrator.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import "@openzeppelin/contracts/utils/Pausable.sol";
import "@openzeppelin/contracts/access/AccessControl.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";

/// @title OmegaOrchestrator — v13 Final
/// @notice Zero-capital flashloan execution engine for Omega MEV strategies.
/// @dev    Checks-Effects-Interactions strictly enforced.
///
/// CHANGES vs prior version — these are not incremental tweaks, the prior file had no
/// working execution path at all:
///   1. FIXED (critical, blocking) — the prior file's doc comment said the flashloan provider
///      "calls back into this contract via executeWithFlashloan()", but no such function, or
///      any callback function at all, existed anywhere in the contract. `_executeFlashloan`
///      just measured `address(this).balance` (ETH) before/after calling
///      `IFlashloanProvider.flashloan(...)` and called the difference "profit" — with no
///      actual repayment logic, no token transfer to the strategy contract, and a
///      flashloan-provider interface that didn't even specify which ERC20 to borrow. As
///      written, calling execute() could not have resulted in a real flashloan-and-arb cycle
///      under any real flashloan provider's actual calling convention. This version
///      implements a real callback against Balancer V2's Vault interface (flashLoan /
///      receiveFlashLoan) — see the design note below on why Balancer specifically, and what
///      that choice does and doesn't cover.
///   2. FIXED (critical, blocking) — related to #1: the blueprint layout had no field
///      specifying which ERC20 token to flashloan. A flashloan provider fundamentally needs
///      to know that. Added `address flashloanToken` to the blueprint.
///   3. FIXED (real vulnerability) — the signed hash was `keccak256(blueprintCalldata)` alone,
///      with no reference to this specific contract's address. If the same execution_key is
///      ever used across two deployments on the same chain (a staging/canary deployment and
///      a prod one, or an old paused version and a new one — this system explicitly has a
///      canary strategy, so multiple deployments sharing a signer is a realistic scenario), a
///      validly-signed blueprint for one deployment could be replayed on the other. Fixed by
///      folding `address(this)` and the chain ID into the signed/replay-tracked hash.
///   4. FIXED — signature recovery was hand-rolled inline assembly calling raw `ecrecover`,
///      with no protection against ECDSA signature malleability. Replaced with OpenZeppelin's
///      `ECDSA.recover`, which rejects malleable (high-s) signatures and malformed input.
///      (Malleability wasn't separately exploitable for replay here, since replay protection
///      keys off the blueprint hash rather than the signature — but there's no reason to
///      accept a weaker guarantee than the standard library gives for free.)
///   5. REMOVED — `EXECUTOR_ROLE` was declared and granted to the execution key in the
///      constructor, but never checked anywhere (`execute()` is gated by the ECDSA signature
///      check against `execution_key`/`pending_key`, not by `onlyRole`). An unused
///      access-control role sitting in a contract like this is itself a footgun: a future
///      maintainer could reasonably assume it's enforced when it isn't. Removed rather than
///      left dangling.
///   6. SIMPLIFIED — `strategy_bytecode_hashes` was storing
///      `keccak256(abi.encodePacked(implementation.codehash))`, i.e. re-hashing a hash for no
///      benefit (codehash is already a hash, and there's no multi-argument packing-collision
///      risk with a single fixed-size value). Now stores `implementation.codehash` directly.
///   7. ADDED — this version now supports three flashloan providers (Balancer V2, Aave v3,
///      Uniswap V3), not just Balancer. This was added because the off-chain provider-selection
///      crate (omega-flashloan) is built with real fallback logic across all three — its own
///      test suite exercises the Aave fallback path — while this contract previously only
///      implemented Balancer's callback shape. A blueprint that caused the off-chain selector
///      to pick Aave or Uniswap V3 would have built calldata for a callback function that
///      didn't exist here, and reverted. See the blueprint layout below (now carries
///      `providerType` and `providerContract`) and the three callback functions
///      (`receiveFlashLoan`, `executeOperation`, `uniswapV3FlashCallback`).
///
/// DESIGN NOTE — provider handling, per provider type:
///   - Balancer V2: one admin-configured Vault address (`flashloanProvider`), shared across
///     all blueprints. Push-repayment: this contract transfers principal+fee back to the
///     Vault before the callback returns; Balancer checks its own balance increased.
///   - Aave v3: one admin-configured Pool address (`aavePool`), shared across all blueprints.
///     Pull-repayment: Aave transfers `amount+premium` FROM this contract via `transferFrom`
///     after `executeOperation` returns `true`, so this contract must approve the Pool for
///     that amount before returning, not push a transfer itself.
///   - Uniswap V3: there is no single canonical pool address — a different pool exists per
///     token pair and fee tier, so the blueprint itself must supply `providerContract` (the
///     specific pool to flash against). This contract determines on-chain whether
///     `flashloanToken` is that pool's `token0` or `token1` by calling the pool directly,
///     rather than trusting an off-chain-supplied flag — which also closes the exact
///     token0/token1 ambiguity present in the off-chain Rust encoder for this same provider.
///   Regardless of provider, this Orchestrator still only ever forwards profit to the Vault in
///   one token per deployment, matching the Vault's own immutable `profit_token` — checked
///   explicitly in execute(). Supporting multiple simultaneous profit tokens would still be a
///   separate, real architecture decision (multiple Vault deployments or a token-routing
///   layer), not something folded in here.
contract OmegaOrchestrator is ReentrancyGuard, Pausable, AccessControl {
    using SafeERC20 for IERC20;
    using ECDSA for bytes32;

    // ─────────────────────────────────────────────────────────────────────────
    // Flashloan provider type
    // ─────────────────────────────────────────────────────────────────────────
    enum FlashloanProviderType { Balancer, AaveV3, UniswapV3 }

    // ─────────────────────────────────────────────────────────────────────────
    // Roles
    // ─────────────────────────────────────────────────────────────────────────
    bytes32 public constant EMERGENCY_ROLE = keccak256("EMERGENCY");

    // ─────────────────────────────────────────────────────────────────────────
    // Immutables
    // ─────────────────────────────────────────────────────────────────────────
    uint64  public immutable EXPECTED_CHAIN_ID;

    // ─────────────────────────────────────────────────────────────────────────
    // Mutable state
    // ─────────────────────────────────────────────────────────────────────────
    address public vault;
    address public flashloanProvider;   // Balancer V2 Vault (or flashLoan/receiveFlashLoan-compatible)
    address public aavePool;            // Aave v3 Pool (flashLoanSimple/executeOperation)

    address public  execution_key;
    address public  pending_key;
    uint64  public  rotation_window_end_block;

    // Replay protection: domain-separated blueprint hash -> executed
    mapping(bytes32 => bool) public executed_blueprints;

    // Chain-scoped nonce: keccak256(abi.encode(strategyId, EXPECTED_CHAIN_ID)) -> nextNonce
    mapping(bytes32 => uint64) public next_nonce;

    mapping(bytes32 => address) public strategy_registry;
    mapping(bytes32 => bytes32) public strategy_bytecode_hashes;
    mapping(bytes32 => bool)    public strategy_frozen;

    // Transient (single-call) flashloan callback state — always false/zero outside of an
    // in-flight execute() call. See receiveFlashLoan() for how this is used.
    bool    private _flashloanInProgress;
    uint256 private _pendingNetProfit;
    // Uniswap V3 has no single canonical pool address, so — unlike flashloanProvider/aavePool,
    // which are fixed and admin-set — this tracks which specific pool THIS call is allowed to
    // call back from. Always address(0) outside of an in-flight Uniswap V3 flashloan.
    address private _activeUniswapV3Pool;

    // ─────────────────────────────────────────────────────────────────────────
    // Blueprint layout (ABI-encoded)
    // ─────────────────────────────────────────────────────────────────────────
    // blueprintCalldata = abi.encode(
    //   uint64  expiry_block,
    //   uint64  nonce,
    //   bytes32 strategyId,
    //   uint8   providerType,     // 0=Balancer, 1=AaveV3, 2=UniswapV3 (see FlashloanProviderType)
    //   address flashloanToken,
    //   address providerContract, // ONLY meaningful for UniswapV3 (the specific pool to use);
    //                             // ignored for Balancer/Aave, which use the fixed admin-set
    //                             // flashloanProvider/aavePool addresses instead. Pass
    //                             // address(0) for those two.
    //   bytes   strategyCalldata,
    //   uint256 flashloanAmount,
    //   uint256 minNetProfit
    // )
    //
    // The value actually signed and used for replay protection is:
    //   keccak256(abi.encode(address(this), EXPECTED_CHAIN_ID, blueprintCalldata))
    // NOT keccak256(blueprintCalldata) alone — see change #3 above.

    // ─────────────────────────────────────────────────────────────────────────
    // Events
    // ─────────────────────────────────────────────────────────────────────────
    event ProfitExtracted(
        bytes32 indexed blueprintHash,
        bytes32 indexed strategyId,
        uint256 netProfit,
        uint64  blockNumber
    );
    event StrategyRegistered(bytes32 indexed strategyId, address implementation, bytes32 bytecodeHash);
    event StrategyFrozen(bytes32 indexed strategyId);
    event KeyRotationInitiated(address indexed newKey, uint64 windowEndBlock);
    event KeyRotationCompleted(address indexed oldKey, address indexed newKey);
    event VaultUpdated(address indexed oldVault, address indexed newVault);
    event FlashloanProviderUpdated(address indexed oldProvider, address indexed newProvider);
    event AavePoolUpdated(address indexed oldPool, address indexed newPool);

    // ─────────────────────────────────────────────────────────────────────────
    // Errors
    // ─────────────────────────────────────────────────────────────────────────
    error WrongChain(uint256 actual, uint64 expected);
    error BlueprintExpired(uint256 current, uint64 expiry);
    error ReplayDetected(bytes32 blueprintHash);
    error InvalidSignature();
    error InvalidNonce(uint64 provided, uint64 expected);
    error UnknownStrategy(bytes32 strategyId);
    error StrategyIsFrozen(bytes32 strategyId);
    error BytecodeMismatch(bytes32 strategyId);
    error InsufficientProfit(uint256 actual, uint256 minimum);
    error ZeroAddress();
    error RotationWindowActive();
    error NoPendingRotation();
    error TokenMismatchWithVault(address flashloanToken, address vaultProfitToken);
    error OnlyFlashloanProvider();
    error UnexpectedFlashloanCallback();
    error InvalidFlashloanCallback();
    error InvalidProviderType();
    error TokenNotInPool(address pool, address flashloanToken);

    // ─────────────────────────────────────────────────────────────────────────
    // Constructor
    // ─────────────────────────────────────────────────────────────────────────
    constructor(
        uint64  chainId,
        address _vault,
        address _flashloanProvider,
        address _aavePool,
        address _executionKey,
        address _admin
    ) {
        if (_vault == address(0) || _executionKey == address(0) || _admin == address(0))
            revert ZeroAddress();
        EXPECTED_CHAIN_ID   = chainId;
        vault               = _vault;
        flashloanProvider   = _flashloanProvider;
        aavePool            = _aavePool;   // address(0) is fine — AaveV3 provider type simply
                                            // unusable until setAavePool() is called
        execution_key       = _executionKey;
        _grantRole(DEFAULT_ADMIN_ROLE, _admin);
        _grantRole(EMERGENCY_ROLE,     _admin);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Core execution
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Execute a signed execution blueprint.
    /// @param  blueprintCalldata ABI-encoded blueprint (see layout above).
    /// @param  sig               65-byte ECDSA signature over the domain-separated blueprint hash.
    function execute(
        bytes calldata blueprintCalldata,
        bytes calldata sig
    ) external nonReentrant whenNotPaused {

        // ── 1. Chain guard ────────────────────────────────────────────────────
        if (block.chainid != EXPECTED_CHAIN_ID)
            revert WrongChain(block.chainid, EXPECTED_CHAIN_ID);

        // ── 2. Decode blueprint ───────────────────────────────────────────────
        (
            uint64  expiry_block,
            uint64  nonce,
            bytes32 strategyId,
            FlashloanProviderType providerType,
            address flashloanToken,
            address providerContract,
            bytes memory strategyCalldata,
            uint256 flashloanAmount,
            uint256 minNetProfit
        ) = abi.decode(
            blueprintCalldata,
            (uint64, uint64, bytes32, FlashloanProviderType, address, address, bytes, uint256, uint256)
        );

        if (flashloanToken == address(0)) revert ZeroAddress();

        // ── 3. Blueprint expiry ───────────────────────────────────────────────
        if (block.number > expiry_block)
            revert BlueprintExpired(block.number, expiry_block);

        // ── 4. Domain-separated blueprint hash + replay protection ───────────
        bytes32 bpHash = keccak256(abi.encode(address(this), EXPECTED_CHAIN_ID, blueprintCalldata));
        if (executed_blueprints[bpHash])
            revert ReplayDetected(bpHash);

        // ── 5. Signature — accepts execution_key or pending_key in window ─────
        address signer = bpHash.recover(sig);
        if (!_acceptsKey(signer))
            revert InvalidSignature();

        // ── 6. Chain-scoped nonce ─────────────────────────────────────────────
        bytes32 nonceKey = keccak256(abi.encode(strategyId, EXPECTED_CHAIN_ID));
        if (nonce != next_nonce[nonceKey])
            revert InvalidNonce(nonce, next_nonce[nonceKey]);

        // ── 7. Strategy lookup + freeze check ────────────────────────────────
        address stratAddr = strategy_registry[strategyId];
        if (stratAddr == address(0))
            revert UnknownStrategy(strategyId);
        if (strategy_frozen[strategyId])
            revert StrategyIsFrozen(strategyId);

        // ── 8. Bytecode integrity ─────────────────────────────────────────────
        // NOTE: this only protects non-proxy strategies (all current strategy contracts are
        // plain, non-upgradeable contracts). If a strategy is ever deployed behind a proxy,
        // codehash checks the proxy's bytecode, not its implementation, and this check stops
        // being meaningful — that would need a different integrity mechanism entirely.
        bytes32 expectedHash = strategy_bytecode_hashes[strategyId];
        if (stratAddr.codehash != expectedHash)
            revert BytecodeMismatch(strategyId);

        // ── 9. Vault token consistency check ─────────────────────────────────
        address vaultProfitToken = address(IOmegaVault(vault).profit_token());
        if (flashloanToken != vaultProfitToken)
            revert TokenMismatchWithVault(flashloanToken, vaultProfitToken);

        // ── 10. State effects BEFORE external calls (CEI) ────────────────────
        executed_blueprints[bpHash] = true;
        next_nonce[nonceKey]        = nonce + 1;

        // ── 11. Execute: flashloan -> strategy -> repay -> profit ────────────
        uint256 netProfit = _executeFlashloan(
            providerType, stratAddr, strategyCalldata, flashloanToken, providerContract, flashloanAmount
        );

        // ── 12. Profit floor check ────────────────────────────────────────────
        if (netProfit < minNetProfit)
            revert InsufficientProfit(netProfit, minNetProfit);

        // ── 13. Forward profit to Vault (pending state, awaiting ZK proof) ────
        if (netProfit > 0) {
            IERC20(flashloanToken).forceApprove(vault, netProfit);
            IOmegaVault(vault).receivePendingProfit(bpHash, netProfit);
        }

        emit ProfitExtracted(bpHash, strategyId, netProfit, uint64(block.number));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Flashloan orchestration — dispatches to Balancer V2, Aave v3, or Uniswap V3
    // ─────────────────────────────────────────────────────────────────────────

    function _executeFlashloan(
        FlashloanProviderType providerType,
        address stratAddr,
        bytes memory strategyCalldata,
        address flashloanToken,
        address providerContract,
        uint256 flashloanAmount
    ) internal returns (uint256 netProfit) {
        // Everything every callback needs, since none of these providers hand back anything
        // except whatever opaque payload we gave them.
        bytes memory userData = abi.encode(stratAddr, strategyCalldata, flashloanToken, flashloanAmount);

        _flashloanInProgress = true;
        _pendingNetProfit    = 0;

        if (providerType == FlashloanProviderType.Balancer) {
            address[] memory tokens = new address[](1);
            tokens[0] = flashloanToken;
            uint256[] memory amounts = new uint256[](1);
            amounts[0] = flashloanAmount;
            IBalancerVault(flashloanProvider).flashLoan(address(this), tokens, amounts, userData);

        } else if (providerType == FlashloanProviderType.AaveV3) {
            IAavePool(aavePool).flashLoanSimple(address(this), flashloanToken, flashloanAmount, userData, 0);

        } else if (providerType == FlashloanProviderType.UniswapV3) {
            if (providerContract == address(0)) revert ZeroAddress();

            address token0 = IUniswapV3Pool(providerContract).token0();
            address token1 = IUniswapV3Pool(providerContract).token1();

            uint256 amount0;
            uint256 amount1;
            if (flashloanToken == token0) {
                amount0 = flashloanAmount;
            } else if (flashloanToken == token1) {
                amount1 = flashloanAmount;
            } else {
                // Determined on-chain by actually calling the pool, rather than trusting an
                // off-chain-supplied token0/token1 flag — this is exactly the ambiguity the
                // off-chain Rust encoder has to just assume away (it defaults to "always
                // token0" since it does no on-chain reads by design). Here, we can and do
                // check it for real.
                revert TokenNotInPool(providerContract, flashloanToken);
            }

            // Uniswap V3 doesn't hand its own address back in the callback, so we have to
            // remember which specific pool we're mid-flashloan with ourselves, to know who's
            // allowed to call uniswapV3FlashCallback() next.
            _activeUniswapV3Pool = providerContract;
            IUniswapV3Pool(providerContract).flash(address(this), amount0, amount1, userData);
            _activeUniswapV3Pool = address(0);

        } else {
            revert InvalidProviderType();
        }

        // If the expected callback below didn't run (e.g. a misbehaving provider that
        // doesn't call back at all), _flashloanInProgress would still read true here and
        // every subsequent legitimate call would be permanently locked out — so make sure
        // it's always cleared on this path regardless of what happened inside the nested call.
        _flashloanInProgress = false;

        netProfit         = _pendingNetProfit;
        _pendingNetProfit = 0;
    }

    /// @notice Balancer V2 Vault flashloan callback. Must only ever be invoked as a nested
    ///         call from within `execute()`'s own `flashLoan` call in the same transaction —
    ///         `_flashloanInProgress` enforces that, since simply checking `msg.sender ==
    ///         flashloanProvider` is not enough: anyone can call the real Balancer Vault's
    ///         `flashLoan` naming this contract as recipient with attacker-chosen `userData`,
    ///         and Balancer will faithfully call this function with a legitimate
    ///         `msg.sender`. The flag closes that hole.
    function receiveFlashLoan(
        address[] calldata tokens,
        uint256[] calldata amounts,
        uint256[] calldata feeAmounts,
        bytes calldata userData
    ) external {
        if (msg.sender != flashloanProvider) revert OnlyFlashloanProvider();
        if (!_flashloanInProgress) revert UnexpectedFlashloanCallback();
        if (tokens.length != 1 || amounts.length != 1 || feeAmounts.length != 1)
            revert InvalidFlashloanCallback();

        (
            address stratAddr,
            bytes memory strategyCalldata,
            address flashloanToken,
            uint256 flashloanAmount
        ) = abi.decode(userData, (address, bytes, address, uint256));

        if (tokens[0] != flashloanToken || amounts[0] != flashloanAmount)
            revert InvalidFlashloanCallback();

        // Strategies are invoked via call() (not delegatecall — see each strategy contract's
        // own header) and account for tokens via their OWN balance, so the borrowed tokens
        // have to actually live at the strategy's address before calling it, not just sit
        // here in the Orchestrator.
        IERC20(flashloanToken).safeTransfer(stratAddr, flashloanAmount);

        // Every strategy contract's execute() is required to end by transferring `netOutput`
        // of the flashloaned token back to `msg.sender` (this contract) — see the strategy
        // contracts' own headers for that side of the contract.
        uint256 netOutput = IStrategy(stratAddr).execute(strategyCalldata, flashloanAmount);

        uint256 repayAmount = amounts[0] + feeAmounts[0];
        // Balancer expects a PUSH: it checks its own balance increased by at least
        // amount+fee by the time this function returns. No approval needed.
        IERC20(flashloanToken).safeTransfer(flashloanProvider, repayAmount);

        _pendingNetProfit = netOutput > repayAmount ? netOutput - repayAmount : 0;
    }

    /// @notice Aave v3 Pool flashloan callback (IPool.flashLoanSimple). Same
    ///         `_flashloanInProgress` reasoning as receiveFlashLoan() above — anyone can call
    ///         the real Aave Pool naming this contract as receiver with attacker-chosen
    ///         `params`, and Aave will faithfully call this function with a legitimate
    ///         `msg.sender`.
    /// @dev    Unlike Balancer, Aave v3 PULLS repayment: after this function returns `true`,
    ///         the Pool calls `transferFrom(address(this), pool, amount+premium)` — so this
    ///         function must leave a sufficient approval in place, not push a transfer itself.
    function executeOperation(
        address asset,
        uint256 amount,
        uint256 premium,
        address initiator,
        bytes calldata params
    ) external returns (bool) {
        if (msg.sender != aavePool) revert OnlyFlashloanProvider();
        if (!_flashloanInProgress) revert UnexpectedFlashloanCallback();
        // Aave passes back whoever called flashLoanSimple() as `initiator` — must be us,
        // since execute() always calls flashLoanSimple with itself as both caller and receiver.
        if (initiator != address(this)) revert InvalidFlashloanCallback();

        (
            address stratAddr,
            bytes memory strategyCalldata,
            address flashloanToken,
            uint256 flashloanAmount
        ) = abi.decode(params, (address, bytes, address, uint256));

        if (asset != flashloanToken || amount != flashloanAmount)
            revert InvalidFlashloanCallback();

        IERC20(flashloanToken).safeTransfer(stratAddr, flashloanAmount);
        uint256 netOutput = IStrategy(stratAddr).execute(strategyCalldata, flashloanAmount);

        uint256 repayAmount = amount + premium;
        // PULL repayment, per the @dev note above — approve, don't transfer.
        IERC20(flashloanToken).forceApprove(aavePool, repayAmount);

        _pendingNetProfit = netOutput > repayAmount ? netOutput - repayAmount : 0;
        return true;
    }

    /// @notice Uniswap V3 pool flashloan callback (IUniswapV3Pool.flash). `msg.sender` is
    ///         checked against `_activeUniswapV3Pool` rather than a single fixed address,
    ///         since (unlike Balancer/Aave) there is no one canonical Uniswap V3 pool — see
    ///         _executeFlashloan()'s UniswapV3 branch for how that's set.
    /// @dev    Like Balancer, Uniswap V3 expects a PUSH: the pool checks its own post-callback
    ///         balance increased by at least amount+fee for whichever token(s) were borrowed.
    function uniswapV3FlashCallback(
        uint256 fee0,
        uint256 fee1,
        bytes calldata data
    ) external {
        if (msg.sender != _activeUniswapV3Pool) revert OnlyFlashloanProvider();
        if (!_flashloanInProgress) revert UnexpectedFlashloanCallback();

        address pool = msg.sender;

        (
            address stratAddr,
            bytes memory strategyCalldata,
            address flashloanToken,
            uint256 flashloanAmount
        ) = abi.decode(data, (address, bytes, address, uint256));

        address token0 = IUniswapV3Pool(pool).token0();
        address token1 = IUniswapV3Pool(pool).token1();

        uint256 fee;
        if (flashloanToken == token0) {
            fee = fee0;
        } else if (flashloanToken == token1) {
            fee = fee1;
        } else {
            revert InvalidFlashloanCallback();
        }

        IERC20(flashloanToken).safeTransfer(stratAddr, flashloanAmount);
        uint256 netOutput = IStrategy(stratAddr).execute(strategyCalldata, flashloanAmount);

        uint256 repayAmount = flashloanAmount + fee;
        IERC20(flashloanToken).safeTransfer(pool, repayAmount);

        _pendingNetProfit = netOutput > repayAmount ? netOutput - repayAmount : 0;
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Internal helpers
    // ─────────────────────────────────────────────────────────────────────────

    function _acceptsKey(address k) internal view returns (bool) {
        if (k == execution_key) return true;
        if (pending_key != address(0) && k == pending_key) {
            return block.number <= rotation_window_end_block;
        }
        return false;
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Strategy registry management
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Register or update a strategy implementation.
    /// @dev    Only callable by DEFAULT_ADMIN. Frozen strategies cannot be updated.
    function registerStrategy(
        bytes32 strategyId,
        address implementation
    ) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (implementation == address(0)) revert ZeroAddress();
        if (strategy_frozen[strategyId]) revert StrategyIsFrozen(strategyId);
        bytes32 bHash = implementation.codehash;
        strategy_registry[strategyId]        = implementation;
        strategy_bytecode_hashes[strategyId] = bHash;
        emit StrategyRegistered(strategyId, implementation, bHash);
    }

    /// @notice Permanently freeze a strategy — cannot be re-activated.
    function freezeStrategy(bytes32 strategyId) external onlyRole(DEFAULT_ADMIN_ROLE) {
        strategy_frozen[strategyId] = true;
        emit StrategyFrozen(strategyId);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Key rotation (dual-key overlap window)
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Initiate key rotation — both old and new keys are valid until windowBlocks
    ///         from now, UNLESS finalizeKeyRotation is called earlier (finalizing early is
    ///         allowed and immediately revokes the old key — that's an intentional admin
    ///         escape hatch, not an oversight).
    function initiateKeyRotation(
        address newKey,
        uint64  windowBlocks
    ) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (newKey == address(0)) revert ZeroAddress();
        if (pending_key != address(0)) revert RotationWindowActive();
        pending_key                = newKey;
        rotation_window_end_block  = uint64(block.number) + windowBlocks;
        emit KeyRotationInitiated(newKey, rotation_window_end_block);
    }

    /// @notice Finalize key rotation — swaps execution key. Callable at any time after
    ///         initiation (see note above); does not require waiting for the window to end.
    function finalizeKeyRotation() external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (pending_key == address(0)) revert NoPendingRotation();
        address old = execution_key;
        execution_key = pending_key;
        pending_key   = address(0);
        rotation_window_end_block = 0;
        emit KeyRotationCompleted(old, execution_key);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Admin setters
    // ─────────────────────────────────────────────────────────────────────────

    function setVault(address _vault) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (_vault == address(0)) revert ZeroAddress();
        emit VaultUpdated(vault, _vault);
        vault = _vault;
    }

    function setFlashloanProvider(address _provider) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (_provider == address(0)) revert ZeroAddress();
        emit FlashloanProviderUpdated(flashloanProvider, _provider);
        flashloanProvider = _provider;
    }

    function setAavePool(address _pool) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (_pool == address(0)) revert ZeroAddress();
        emit AavePoolUpdated(aavePool, _pool);
        aavePool = _pool;
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Emergency controls
    // ─────────────────────────────────────────────────────────────────────────

    function emergencyPause() external onlyRole(EMERGENCY_ROLE) { _pause(); }
    function unpause()        external onlyRole(DEFAULT_ADMIN_ROLE) { _unpause(); }
}

// ─────────────────────────────────────────────────────────────────────────────
// Interfaces (referenced by OmegaOrchestrator)
// ─────────────────────────────────────────────────────────────────────────────

/// @dev Balancer V2 Vault's flashloan entry point.
interface IBalancerVault {
    function flashLoan(
        address recipient,
        address[] calldata tokens,
        uint256[] calldata amounts,
        bytes calldata userData
    ) external;
}

/// @dev Aave v3 Pool's single-asset flashloan entry point.
interface IAavePool {
    function flashLoanSimple(
        address receiverAddress,
        address asset,
        uint256 amount,
        bytes calldata params,
        uint16 referralCode
    ) external;
}

/// @dev Uniswap V3 pool's flashloan entry point. Note this is the POOL itself, not a shared
///      router/vault — a different address per token pair + fee tier.
interface IUniswapV3Pool {
    function token0() external view returns (address);
    function token1() external view returns (address);
    function flash(address recipient, uint256 amount0, uint256 amount1, bytes calldata data) external;
}

interface IStrategy {
    function execute(
        bytes calldata strategyCalldata,
        uint256 flashloanAmount
    ) external returns (uint256 netOutput);
}

interface IOmegaVault {
    function receivePendingProfit(bytes32 blueprintHash, uint256 netProfit) external;
    function profit_token() external view returns (IERC20);
}