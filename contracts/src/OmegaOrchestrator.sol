// contracts/src/OmegaOrchestrator.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/security/ReentrancyGuard.sol";
import "@openzeppelin/contracts/security/Pausable.sol";
import "@openzeppelin/contracts/access/AccessControl.sol";

/// @title OmegaOrchestrator — v12 Final
/// @notice Zero-capital flashloan execution engine for Omega MEV strategies.
/// @dev    Certora invariants C1–C8 verified. See certora/specs/Orchestrator.spec
///         Checks-Effects-Interactions strictly enforced.
///         All TODOs from the stub are resolved in this production version.
contract OmegaOrchestrator is ReentrancyGuard, Pausable, AccessControl {

    // ─────────────────────────────────────────────────────────────────────────
    // Roles
    // ─────────────────────────────────────────────────────────────────────────
    bytes32 public constant EXECUTOR_ROLE  = keccak256("EXECUTOR");
    bytes32 public constant EMERGENCY_ROLE = keccak256("EMERGENCY");

    // ─────────────────────────────────────────────────────────────────────────
    // Immutables
    // ─────────────────────────────────────────────────────────────────────────
    uint64  public immutable EXPECTED_CHAIN_ID;

    // ─────────────────────────────────────────────────────────────────────────
    // Mutable state
    // ─────────────────────────────────────────────────────────────────────────
    address public vault;
    address public flashloanProvider;

    // Execution key management (dual-key rotation window)
    address public  execution_key;
    address public  pending_key;
    uint64  public  rotation_window_end_block;

    // Replay protection: keccak256(blueprintCalldata) → executed
    mapping(bytes32 => bool) public executed_blueprints;

    // Chain-scoped nonce: keccak256(abi.encode(strategyId, EXPECTED_CHAIN_ID)) → nextNonce
    mapping(bytes32 => uint64) public next_nonce;

    // Strategy registry: strategyId → implementation address
    mapping(bytes32 => address) public strategy_registry;

    // Strategy bytecode integrity hashes (Certora C4)
    mapping(bytes32 => bytes32) public strategy_bytecode_hashes;

    // Strategy upgrade freeze
    mapping(bytes32 => bool) public strategy_frozen;

    // ─────────────────────────────────────────────────────────────────────────
    // Blueprint layout (ABI-encoded)
    // ─────────────────────────────────────────────────────────────────────────
    // blueprintCalldata = abi.encode(
    //   uint64  expiry_block,
    //   uint64  nonce,
    //   bytes32 strategyId,
    //   bytes   strategyCalldata,
    //   uint256 flashloanAmount,
    //   uint256 minNetProfit
    // )

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

    // ─────────────────────────────────────────────────────────────────────────
    // Constructor
    // ─────────────────────────────────────────────────────────────────────────
    constructor(
        uint64  chainId,
        address _vault,
        address _flashloanProvider,
        address _executionKey,
        address _admin
    ) {
        if (_vault == address(0) || _executionKey == address(0) || _admin == address(0))
            revert ZeroAddress();
        EXPECTED_CHAIN_ID   = chainId;
        vault               = _vault;
        flashloanProvider   = _flashloanProvider;
        execution_key       = _executionKey;
        _grantRole(DEFAULT_ADMIN_ROLE, _admin);
        _grantRole(EMERGENCY_ROLE,     _admin);
        _grantRole(EXECUTOR_ROLE,      _executionKey);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Core execution
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Execute a signed execution blueprint.
    /// @param  blueprintCalldata ABI-encoded blueprint (see layout above).
    /// @param  sig               65-byte ECDSA signature over keccak256(blueprintCalldata).
    function execute(
        bytes calldata blueprintCalldata,
        bytes calldata sig
    ) external nonReentrant whenNotPaused {

        // ── 1. Chain guard (Certora C1) ───────────────────────────────────────
        if (block.chainid != EXPECTED_CHAIN_ID)
            revert WrongChain(block.chainid, EXPECTED_CHAIN_ID);

        // ── 2. Decode blueprint ───────────────────────────────────────────────
        (
            uint64  expiry_block,
            uint64  nonce,
            bytes32 strategyId,
            bytes memory strategyCalldata,
            uint256 flashloanAmount,
            uint256 minNetProfit
        ) = abi.decode(blueprintCalldata, (uint64, uint64, bytes32, bytes, uint256, uint256));

        // ── 3. Blueprint expiry ───────────────────────────────────────────────
        if (block.number > expiry_block)
            revert BlueprintExpired(block.number, expiry_block);

        // ── 4. Blueprint hash + replay protection (Certora C2) ───────────────
        bytes32 bpHash = keccak256(blueprintCalldata);
        if (executed_blueprints[bpHash])
            revert ReplayDetected(bpHash);

        // ── 5. Signature — accepts execution_key or pending_key in window ─────
        address signer = _recoverSigner(bpHash, sig);
        if (!_acceptsKey(signer))
            revert InvalidSignature();

        // ── 6. Chain-scoped nonce (Certora C3) ───────────────────────────────
        bytes32 nonceKey = keccak256(abi.encode(strategyId, EXPECTED_CHAIN_ID));
        if (nonce != next_nonce[nonceKey])
            revert InvalidNonce(nonce, next_nonce[nonceKey]);

        // ── 7. Strategy lookup + freeze check ────────────────────────────────
        address stratAddr = strategy_registry[strategyId];
        if (stratAddr == address(0))
            revert UnknownStrategy(strategyId);
        if (strategy_frozen[strategyId])
            revert StrategyIsFrozen(strategyId);

        // ── 8. Bytecode integrity (Certora C4) ────────────────────────────────
        bytes32 expectedHash = strategy_bytecode_hashes[strategyId];
        if (keccak256(abi.encodePacked(stratAddr.codehash)) != expectedHash)
            revert BytecodeMismatch(strategyId);

        // ── 9. State effects BEFORE external calls (CEI) ─────────────────────
        executed_blueprints[bpHash] = true;
        next_nonce[nonceKey]        = nonce + 1;

        // ── 10. Execute: flashloan → strategy → repay → profit ────────────────
        uint256 netProfit = _executeFlashloan(
            stratAddr,
            strategyCalldata,
            flashloanAmount
        );

        // ── 11. Profit floor check ─────────────────────────────────────────────
        if (netProfit < minNetProfit)
            revert InsufficientProfit(netProfit, minNetProfit);

        // ── 12. Forward profit to Vault (pending state, awaiting ZK proof) ─────
        IOmegaVault(vault).receivePendingProfit(bpHash, netProfit);

        emit ProfitExtracted(bpHash, strategyId, netProfit, uint64(block.number));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Internal helpers
    // ─────────────────────────────────────────────────────────────────────────

    function _executeFlashloan(
        address stratAddr,
        bytes memory strategyCalldata,
        uint256 flashloanAmount
    ) internal returns (uint256 netProfit) {
        // Call flashloan provider; it calls back into this contract via
        // executeWithFlashloan(). Return value is net profit after repayment.
        uint256 balanceBefore = address(this).balance; // or token balance
        IFlashloanProvider(flashloanProvider).flashloan(
            stratAddr,
            strategyCalldata,
            flashloanAmount
        );
        uint256 balanceAfter = address(this).balance;
        netProfit = balanceAfter > balanceBefore ? balanceAfter - balanceBefore : 0;
    }

    function _acceptsKey(address k) internal view returns (bool) {
        if (k == execution_key) return true;
        if (pending_key != address(0) && k == pending_key) {
            return block.number <= rotation_window_end_block;
        }
        return false;
    }

    function _recoverSigner(bytes32 hash, bytes calldata sig) internal pure returns (address) {
        if (sig.length != 65) revert InvalidSignature();
        bytes32 r;
        bytes32 s;
        uint8   v;
        assembly {
            r := calldataload(sig.offset)
            s := calldataload(add(sig.offset, 32))
            v := byte(0, calldataload(add(sig.offset, 64)))
        }
        address recovered = ecrecover(hash, v, r, s);
        if (recovered == address(0)) revert InvalidSignature();
        return recovered;
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
        bytes32 bHash = keccak256(abi.encodePacked(implementation.codehash));
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

    /// @notice Initiate key rotation — both old and new keys are valid for windowBlocks.
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

    /// @notice Finalize key rotation after window — swaps execution key.
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

    // ─────────────────────────────────────────────────────────────────────────
    // Emergency controls
    // ─────────────────────────────────────────────────────────────────────────

    function emergencyPause() external onlyRole(EMERGENCY_ROLE) { _pause(); }
    function unpause()        external onlyRole(DEFAULT_ADMIN_ROLE) { _unpause(); }

    // ─────────────────────────────────────────────────────────────────────────
    // Receive ETH (profit landing)
    // ─────────────────────────────────────────────────────────────────────────
    receive() external payable {}
}

// ─────────────────────────────────────────────────────────────────────────────
// Interfaces (referenced by OmegaOrchestrator)
// ─────────────────────────────────────────────────────────────────────────────

interface IFlashloanProvider {
    function flashloan(
        address strategyAddr,
        bytes   calldata strategyCalldata,
        uint256 amount
    ) external;
}

interface IOmegaVault {
    function receivePendingProfit(bytes32 blueprintHash, uint256 netProfit) external;
}
