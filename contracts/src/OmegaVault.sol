// contracts/src/OmegaVault.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/security/ReentrancyGuard.sol";
import "@openzeppelin/contracts/access/AccessControl.sol";

/// @title OmegaVault — v12 Final (OmegaDAO 5% fee split)
/// @notice One-way profit bridge: Orchestrator → Vault (pending) → PIL (confirmed)
/// @dev    Certora invariants:
///           C6: profit released only after valid STARK proof AND depth >= 12
///           C9: profit_to_pil + profit_to_dao == netProfit AND dao_fee <= 10%
///         Per-transfer cap 50 ETH (in token equivalent). Daily cap 500 ETH.
///         ZK proof gate: IStarkVerifier.verify() must pass before any release.
///         OPIL minted on pil_share only — OPIL holders not diluted by DAO fee.
contract OmegaVault is ReentrancyGuard, AccessControl {
    using SafeERC20 for IERC20;

    // ─────────────────────────────────────────────────────────────────────────
    // Roles
    // ─────────────────────────────────────────────────────────────────────────
    bytes32 public constant ORCHESTRATOR_ROLE = keccak256("ORCHESTRATOR");
    bytes32 public constant DEPTH_UPDATER_ROLE = keccak256("DEPTH_UPDATER");
    bytes32 public constant GOVERNANCE_ROLE    = keccak256("GOVERNANCE");

    // ─────────────────────────────────────────────────────────────────────────
    // Immutables
    // ─────────────────────────────────────────────────────────────────────────
    address public immutable pil_treasury;
    address public immutable stark_verifier;
    IERC20  public immutable profit_token;

    // ─────────────────────────────────────────────────────────────────────────
    // Governance-controlled parameters
    // ─────────────────────────────────────────────────────────────────────────
    address public dao_fee_address;   // OmegaDAO multisig — 48h timelock to change
    uint256 public dao_fee_bps;       // default 500 = 5%; max 1000 = 10%

    // Pending governance change for dao_fee_address (48h timelock)
    address public pending_dao_fee_address;
    uint256 public dao_fee_address_unlock_time;

    // Pending governance change for dao_fee_bps (48h timelock via L3)
    uint256 public pending_dao_fee_bps;
    uint256 public dao_fee_bps_unlock_time;

    // ─────────────────────────────────────────────────────────────────────────
    // Constants
    // ─────────────────────────────────────────────────────────────────────────
    uint256 public constant MAX_DAO_FEE_BPS        = 1000;   // 10% hard cap (C9)
    uint256 public constant MIN_CONFIRMATION_DEPTH = 12;      // C6
    uint256 public constant TIMELOCK_DURATION       = 48 hours;

    // Transfer caps (in profit_token base units — caller sets at deploy via constructor)
    uint256 public immutable PER_TRANSFER_CAP;   // 50 ETH equivalent
    uint256 public immutable DAILY_CAP;           // 500 ETH equivalent

    // Daily cap tracking
    uint256 public daily_released;
    uint256 public daily_window_start;

    // ─────────────────────────────────────────────────────────────────────────
    // Per-blueprint state
    // ─────────────────────────────────────────────────────────────────────────
    mapping(bytes32 => uint256) public pending_profit;
    mapping(bytes32 => uint8)   public confirmation_depth;
    mapping(bytes32 => bool)    public proof_verified;
    mapping(bytes32 => bool)    public released;   // replay protection on release

    // ─────────────────────────────────────────────────────────────────────────
    // Events
    // ─────────────────────────────────────────────────────────────────────────
    event PendingProfitReceived(bytes32 indexed blueprintHash, uint256 amount);
    event ConfirmationDepthUpdated(bytes32 indexed blueprintHash, uint8 depth);
    event ProofVerified(bytes32 indexed blueprintHash);
    event ProfitSplit(
        bytes32 indexed blueprintHash,
        uint256 pilShare,
        uint256 daoFee,
        address daoAddress
    );
    event DaoFeeAddressChangeQueued(address indexed pending, uint256 unlockTime);
    event DaoFeeAddressUpdated(address indexed oldAddr, address indexed newAddr);
    event DaoFeeBpsChangeQueued(uint256 pending, uint256 unlockTime);
    event DaoFeeBpsUpdated(uint256 oldBps, uint256 newBps);

    // ─────────────────────────────────────────────────────────────────────────
    // Errors
    // ─────────────────────────────────────────────────────────────────────────
    error InsufficientDepth(uint8 actual, uint256 required);
    error ProofNotVerified(bytes32 blueprintHash);
    error InvalidProof();
    error NoPendingProfit(bytes32 blueprintHash);
    error AlreadyReleased(bytes32 blueprintHash);
    error DaoFeeExceedsMax(uint256 bps, uint256 max);
    error ExceedsPerTransferCap(uint256 amount, uint256 cap);
    error ExceedsDailyCap(uint256 amount, uint256 remaining);
    error TimelockNotExpired(uint256 unlockTime);
    error NoPendingChange();
    error ZeroAddress();

    // ─────────────────────────────────────────────────────────────────────────
    // Constructor
    // ─────────────────────────────────────────────────────────────────────────
    constructor(
        address _pil,
        address _daoFeeAddr,
        address _starkVerifier,
        address _token,
        address _admin,
        address _orchestrator,
        uint256 _perTransferCap,
        uint256 _dailyCap
    ) {
        if (_pil == address(0) || _daoFeeAddr == address(0) ||
            _starkVerifier == address(0) || _token == address(0) ||
            _admin == address(0)) revert ZeroAddress();

        pil_treasury    = _pil;
        dao_fee_address = _daoFeeAddr;
        stark_verifier  = _starkVerifier;
        profit_token    = IERC20(_token);
        dao_fee_bps     = 500; // 5% default
        PER_TRANSFER_CAP = _perTransferCap;
        DAILY_CAP        = _dailyCap;
        daily_window_start = block.timestamp;

        _grantRole(DEFAULT_ADMIN_ROLE,  _admin);
        _grantRole(GOVERNANCE_ROLE,     _admin);
        _grantRole(ORCHESTRATOR_ROLE,   _orchestrator);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Step 1: Orchestrator deposits pending profit
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Called by OmegaOrchestrator immediately after execution.
    ///         Profit sits in pending state until proof + depth are satisfied.
    function receivePendingProfit(
        bytes32 blueprintHash,
        uint256 netProfit
    ) external onlyRole(ORCHESTRATOR_ROLE) {
        pending_profit[blueprintHash] += netProfit;
        emit PendingProfitReceived(blueprintHash, netProfit);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Step 2: Off-chain relayer updates confirmation depth
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Monotonically increasing depth update — only advances, never regresses.
    function updateConfirmationDepth(
        bytes32 blueprintHash,
        uint8   depth
    ) external onlyRole(DEPTH_UPDATER_ROLE) {
        if (depth > confirmation_depth[blueprintHash]) {
            confirmation_depth[blueprintHash] = depth;
            emit ConfirmationDepthUpdated(blueprintHash, depth);
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Step 3: STARK proof submission (C6)
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Submit and verify STARK proof for a blueprint.
    ///         Must be called before releaseProfit.
    function submitProof(
        bytes32        blueprintHash,
        bytes calldata starkProof
    ) external {
        if (!IStarkVerifier(stark_verifier).verify(starkProof, blueprintHash))
            revert InvalidProof();
        proof_verified[blueprintHash] = true;
        emit ProofVerified(blueprintHash);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Step 4: Release profit (C6 + C9)
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Release confirmed profit: 95% → PIL, 5% → OmegaDAO.
    ///         Requires: proof verified AND confirmation_depth >= 12.
    /// @dev    Certora C6: gated by proof + depth.
    ///         Certora C9: pil_share + dao_fee == netProfit, dao_fee <= 10%.
    function releaseProfit(bytes32 blueprintHash) external nonReentrant {
        // ── C6 guards ────────────────────────────────────────────────────────
        if (!proof_verified[blueprintHash])
            revert ProofNotVerified(blueprintHash);
        if (confirmation_depth[blueprintHash] < MIN_CONFIRMATION_DEPTH)
            revert InsufficientDepth(confirmation_depth[blueprintHash], MIN_CONFIRMATION_DEPTH);

        // ── Replay protection ────────────────────────────────────────────────
        if (released[blueprintHash])
            revert AlreadyReleased(blueprintHash);

        uint256 net = pending_profit[blueprintHash];
        if (net == 0) revert NoPendingProfit(blueprintHash);

        // ── Transfer caps ─────────────────────────────────────────────────────
        if (net > PER_TRANSFER_CAP)
            revert ExceedsPerTransferCap(net, PER_TRANSFER_CAP);

        _refreshDailyWindow();
        if (daily_released + net > DAILY_CAP)
            revert ExceedsDailyCap(net, DAILY_CAP - daily_released);

        // ── Effects BEFORE external calls (CEI) ──────────────────────────────
        released[blueprintHash]         = true;
        pending_profit[blueprintHash]   = 0;
        daily_released                 += net;

        // ── C9: DAO fee split ─────────────────────────────────────────────────
        uint256 dao_fee   = (net * dao_fee_bps) / 10_000;
        uint256 pil_share = net - dao_fee;

        // Hard invariant check — never exceeds 10% regardless of storage value
        if (dao_fee > net / 10)
            revert DaoFeeExceedsMax(dao_fee_bps, MAX_DAO_FEE_BPS);

        // ── Transfers ─────────────────────────────────────────────────────────
        profit_token.safeTransfer(pil_treasury,    pil_share);
        profit_token.safeTransfer(dao_fee_address, dao_fee);

        emit ProfitSplit(blueprintHash, pil_share, dao_fee, dao_fee_address);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Daily cap helper
    // ─────────────────────────────────────────────────────────────────────────

    function _refreshDailyWindow() internal {
        if (block.timestamp >= daily_window_start + 1 days) {
            daily_released     = 0;
            daily_window_start = block.timestamp;
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Governance: dao_fee_address (48h timelock)
    // ─────────────────────────────────────────────────────────────────────────

    function queueDaoFeeAddressChange(
        address newAddr
    ) external onlyRole(GOVERNANCE_ROLE) {
        if (newAddr == address(0)) revert ZeroAddress();
        pending_dao_fee_address    = newAddr;
        dao_fee_address_unlock_time = block.timestamp + TIMELOCK_DURATION;
        emit DaoFeeAddressChangeQueued(newAddr, dao_fee_address_unlock_time);
    }

    function executeDaoFeeAddressChange() external onlyRole(GOVERNANCE_ROLE) {
        if (pending_dao_fee_address == address(0)) revert NoPendingChange();
        if (block.timestamp < dao_fee_address_unlock_time)
            revert TimelockNotExpired(dao_fee_address_unlock_time);
        address old        = dao_fee_address;
        dao_fee_address    = pending_dao_fee_address;
        pending_dao_fee_address = address(0);
        dao_fee_address_unlock_time = 0;
        emit DaoFeeAddressUpdated(old, dao_fee_address);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Governance: dao_fee_bps (48h timelock, max 10%)
    // ─────────────────────────────────────────────────────────────────────────

    function queueDaoFeeBpsChange(
        uint256 newBps
    ) external onlyRole(GOVERNANCE_ROLE) {
        if (newBps > MAX_DAO_FEE_BPS)
            revert DaoFeeExceedsMax(newBps, MAX_DAO_FEE_BPS);
        pending_dao_fee_bps    = newBps;
        dao_fee_bps_unlock_time = block.timestamp + TIMELOCK_DURATION;
        emit DaoFeeBpsChangeQueued(newBps, dao_fee_bps_unlock_time);
    }

    function executeDaoFeeBpsChange() external onlyRole(GOVERNANCE_ROLE) {
        if (dao_fee_bps_unlock_time == 0) revert NoPendingChange();
        if (block.timestamp < dao_fee_bps_unlock_time)
            revert TimelockNotExpired(dao_fee_bps_unlock_time);
        uint256 old     = dao_fee_bps;
        dao_fee_bps     = pending_dao_fee_bps;
        pending_dao_fee_bps    = 0;
        dao_fee_bps_unlock_time = 0;
        emit DaoFeeBpsUpdated(old, dao_fee_bps);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // View helpers
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Returns whether a blueprint is releasable right now.
    function isReleasable(bytes32 blueprintHash) external view returns (bool) {
        return proof_verified[blueprintHash]
            && confirmation_depth[blueprintHash] >= MIN_CONFIRMATION_DEPTH
            && !released[blueprintHash]
            && pending_profit[blueprintHash] > 0;
    }

    /// @notice Remaining daily release capacity.
    function dailyCapRemaining() external view returns (uint256) {
        if (block.timestamp >= daily_window_start + 1 days) return DAILY_CAP;
        return DAILY_CAP > daily_released ? DAILY_CAP - daily_released : 0;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// IStarkVerifier interface
// ─────────────────────────────────────────────────────────────────────────────
interface IStarkVerifier {
    function verify(bytes calldata proof, bytes32 blueprintHash) external view returns (bool);
}
