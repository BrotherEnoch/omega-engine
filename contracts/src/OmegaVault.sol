// contracts/src/OmegaVault.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/security/ReentrancyGuard.sol";
import {AccessControl} from "@openzeppelin/contracts/access/AccessControl.sol";
import {ReconciliationRotationGate as Gate} from "./ReconciliationRotationGate.sol";

/// @title OmegaVault — v14 (Gate-integrated, ZK-bound)
/// @notice One-way profit bridge: Orchestrator -> Vault (pending) -> PIL (confirmed)
/// @dev    Certora invariants:
///           C6: profit released only after valid STARK proof AND depth >= 12
///           C9: profit_to_pil + profit_to_dao == netProfit AND dao_fee <= 10%
///         Per-transfer cap 50 ETH (in token equivalent). Daily cap 500 ETH.
///         ZK proof gate: IStarkVerifier.verify() must pass against the BOUND public inputs
///         before any release (see change #2 below — this is the actual C4 fix).
///         OPIL minted on pil_share only — OPIL holders not diluted by DAO fee.
///         `totalPendingProfit` (v14.1) tracks the sum of `pending_profit` across all
///         blueprintHashes at all times — `rescueERC20` relies on this to never authorize
///         moving funds encumbered by C9's pending-release obligation.
///
/// CHANGES vs the prior "v13 Final" version (which did NOT use the Gate library at all):
///   1. FIXED (accounting hazard) — `receivePendingProfit` used `pending_profit[hash] +=
///      netProfit`, i.e. it silently accepted a second (or Nth) deposit against the same
///      blueprintHash and just accumulated it. Combined with change #2 below, a proof
///      verified against the FIRST deposit's amount would then have authorized release of
///      the cumulative total after a later top-up — a real fund-safety bug, not a style
///      nit. `receivePendingProfit` is now strictly one-shot per blueprintHash: a second
///      call for the same hash reverts with `ProfitAlreadyPending`. In practice this matches
///      how it's actually invoked anyway — Orchestrator's own replay protection means
///      `execute()` calls this at most once per blueprintHash.
///   2. FIXED (C4 — this is the real ZK-binding gap) — `submitProof` previously only bound
///      a STARK proof to `blueprintHash`, with no binding to the amount/recipient/token it
///      was meant to attest to. Now, at deposit time, this contract computes
///      `publicInputsHash = keccak256(abi.encode(address(this), blueprintHash, netProfit,
///      address(profit_token)))` itself (not supplied by the caller) and binds it via the
///      Gate. `submitProof` now takes `publicInputsHash` and must match the bound value
///      before the STARK verifier is even called; the verifier itself is now also called
///      with `publicInputsHash`, so the proof is cryptographically bound to the exact
///      amount/token/contract it's meant to authorize, not just an opaque blueprint id.
///   3. ADDED — `releaseProfit` now runs `Gate.requireVaultReconciliation` immediately before
///      paying out, confirming this contract's actual token balance still covers the amount
///      it's about to distribute (protects against balance drifting out from under the
///      accounting between deposit and release — the "C2 vault accounting" class of bug).
///   4. ADDED — pending-proof tracking via the Gate's transient/persistent counters, plus a
///      public `pendingProofCount()` getter so OTHER contracts (specifically the
///      Orchestrator, on key rotation) can confirm this Vault has no profit stuck mid-flight
///      before finalizing something that changes who's allowed to drive it.
///   5. FIXED — the Gate's `requireVaultReconciliation`/`requirePostExecutionReconciliation`
///      take `IERC20` as their first argument, so calling them as `profit_token.require...`
///      requires `using Gate for IERC20;` in addition to `using Gate for Gate.GateState;`.
///      The prior integration attempt only declared the latter and would not have compiled.
///   6. Everything else (STARK gate, confirmation-depth gate, per-transfer/daily caps, 48h
///      timelocked DAO-fee governance, CEI ordering) is unchanged from the prior version.
///   7. ADDED (v14.1 — emergency fund rescue) — `rescueERC20`/`rescueETH`,
///      DEFAULT_ADMIN_ROLE-gated and `nonReentrant`. Closes the "no-rescue" gap: previously
///      neither this contract nor the Orchestrator had any recovery path for dust, mistaken
///      transfers, or airdropped tokens — they were permanently stranded. For `profit_token`
///      specifically, rescue is capped by a new `totalPendingProfit` running counter so it
///      can only draw on the surplus above what's currently owed to PIL/DAO for blueprints
///      awaiting release — it cannot touch encumbered funds and does not weaken C9. Any other
///      ERC20, and ETH, is rescuable in full (no legitimate claim on either exists here).
///      See `rescueERC20`'s own doc comment for the full reasoning.
contract OmegaVault is ReentrancyGuard, AccessControl {
    using SafeERC20 for IERC20;
    using Gate for Gate.GateState;
    using Gate for IERC20;

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

    address public pending_dao_fee_address;
    uint256 public dao_fee_address_unlock_time;

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

    uint256 public daily_released;
    uint256 public daily_window_start;

    // ─────────────────────────────────────────────────────────────────────────
    // Reconciliation gate state
    // ─────────────────────────────────────────────────────────────────────────
    Gate.GateState private gateState;

    // ─────────────────────────────────────────────────────────────────────────
    // Per-blueprint state
    // ─────────────────────────────────────────────────────────────────────────
    mapping(bytes32 => uint256) public pending_profit;
    mapping(bytes32 => uint8)   public confirmation_depth;
    mapping(bytes32 => bool)    public proof_verified;
    mapping(bytes32 => bool)    public released;   // replay protection on release

    /// @notice Running sum of `pending_profit` across every blueprintHash currently
    ///         awaiting release — i.e. the amount of `profit_token` legitimately owed to
    ///         PIL/DAO right now. Incremented in `receivePendingProfit`, decremented in
    ///         `releaseProfit`. Exists solely to give `rescueERC20` an O(1) way to compute
    ///         the *rescuable surplus* of `profit_token` without walking every
    ///         blueprintHash — see that function's doc comment.
    uint256 public totalPendingProfit;

    // ─────────────────────────────────────────────────────────────────────────
    // Events
    // ─────────────────────────────────────────────────────────────────────────
    event PendingProfitReceived(bytes32 indexed blueprintHash, uint256 amount, bytes32 publicInputsHash);
    event ConfirmationDepthUpdated(bytes32 indexed blueprintHash, uint8 depth);
    event ProofVerified(bytes32 indexed blueprintHash, bytes32 publicInputsHash);
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
    event FundsRescued(address indexed token, address indexed to, uint256 amount);
    event EthRescued(address indexed to, uint256 amount);

    // ─────────────────────────────────────────────────────────────────────────
    // Errors
    // ─────────────────────────────────────────────────────────────────────────
    error InsufficientDepth(uint8 actual, uint256 required);
    error ProofNotVerified(bytes32 blueprintHash);
    error InvalidProof();
    error NoPendingProfit(bytes32 blueprintHash);
    error AlreadyReleased(bytes32 blueprintHash);
    error ProfitAlreadyPending(bytes32 blueprintHash);
    error DaoFeeExceedsMax(uint256 bps, uint256 max);
    error ExceedsPerTransferCap(uint256 amount, uint256 cap);
    error ExceedsDailyCap(uint256 amount, uint256 remaining);
    error TimelockNotExpired(uint256 unlockTime);
    error NoPendingChange();
    error ZeroAddress();
    error ZeroAmount();
    error InsufficientRescuableBalance(uint256 requested, uint256 rescuable);
    error EthTransferFailed();
    error TotalPendingProfitUnderflow(uint256 current, uint256 attemptedDecrement);

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
        // NOTE: DEPTH_UPDATER_ROLE is intentionally not granted here — there is no default
        // depth-updating relayer. After deployment you must call
        // grantRole(DEPTH_UPDATER_ROLE, <your relayer address>) yourself. Skipping this step
        // means updateConfirmationDepth can never be called by anyone, and no profit can
        // ever clear the C6 gate.
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Step 1: Orchestrator deposits pending profit (one-shot per blueprintHash)
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Called by OmegaOrchestrator immediately after execution. Pulls `netProfit`
    ///         of `profit_token` from the caller (the Orchestrator must have approved this
    ///         contract for at least `netProfit` beforehand), records it as pending, and
    ///         binds the ZK public-inputs hash that any later `submitProof` must match.
    /// @dev    One-shot: reverts if this blueprintHash already has pending profit recorded.
    function receivePendingProfit(
        bytes32 blueprintHash,
        uint256 netProfit
    ) external onlyRole(ORCHESTRATOR_ROLE) nonReentrant {
        if (netProfit == 0) revert ZeroAmount();
        if (pending_profit[blueprintHash] != 0) revert ProfitAlreadyPending(blueprintHash);

        // Pull real tokens in the same call that updates the accounting — the two can't
        // drift apart the way a bare mapping increment (with no transfer) previously could.
        profit_token.safeTransferFrom(msg.sender, address(this), netProfit);

        pending_profit[blueprintHash] = netProfit;
        totalPendingProfit           += netProfit;

        // Bind the ZK public inputs to THIS exact amount/token/contract, computed on-chain
        // (not caller-supplied) — closes the C4 gap where a proof was only bound to an
        // opaque blueprint id and not to what it actually authorized moving. Computed via
        // the same `computePublicInputsHash` helper exposed below, so the binding logic has
        // exactly one implementation rather than two copies that could drift apart.
        bytes32 publicInputsHash = computePublicInputsHash(blueprintHash, netProfit);
        gateState.bindProofInputs(blueprintHash, publicInputsHash);
        gateState.trackVaultProofOpen();

        emit PendingProfitReceived(blueprintHash, netProfit, publicInputsHash);
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
    // Step 3: STARK proof submission (C6 + C4)
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Submit and verify a STARK proof for a blueprint. Must be called before
    ///         releaseProfit. Permissionless by design — the proof is self-verifying via
    ///         IStarkVerifier; an invalid proof, or one whose publicInputsHash doesn't match
    ///         what was bound at deposit time, simply reverts here.
    function submitProof(
        bytes32        blueprintHash,
        bytes32        publicInputsHash,
        bytes calldata starkProof
    ) external {
        // Reverts if not bound, or if bound to a different hash than the one supplied —
        // this is the on-chain half of the C4 fix (the caller can't just supply any hash).
        gateState.requireProofInputMatch(blueprintHash, publicInputsHash);

        if (!IStarkVerifier(stark_verifier).verify(starkProof, blueprintHash, publicInputsHash))
            revert InvalidProof();

        proof_verified[blueprintHash] = true;
        emit ProofVerified(blueprintHash, publicInputsHash);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Step 4: Release profit (C6 + C9 + C2)
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Release confirmed profit: 95% -> PIL, 5% -> OmegaDAO.
    ///         Requires: proof verified AND confirmation_depth >= 12 AND actual balance
    ///         still covers the amount (Gate reconciliation).
    /// @dev    Certora C6: gated by proof + depth.
    ///         Certora C9: pil_share + dao_fee == netProfit, dao_fee <= 10%.
    function releaseProfit(bytes32 blueprintHash) external nonReentrant {
        if (!proof_verified[blueprintHash])
            revert ProofNotVerified(blueprintHash);
        if (confirmation_depth[blueprintHash] < MIN_CONFIRMATION_DEPTH)
            revert InsufficientDepth(confirmation_depth[blueprintHash], MIN_CONFIRMATION_DEPTH);

        if (released[blueprintHash])
            revert AlreadyReleased(blueprintHash);

        uint256 net = pending_profit[blueprintHash];
        if (net == 0) revert NoPendingProfit(blueprintHash);

        if (net > PER_TRANSFER_CAP)
            revert ExceedsPerTransferCap(net, PER_TRANSFER_CAP);

        _refreshDailyWindow();
        if (daily_released + net > DAILY_CAP)
            revert ExceedsDailyCap(net, DAILY_CAP - daily_released);

        // 🔒 Vault reconciliation — actual on-chain balance must still cover what accounting
        // says we're about to pay out, right before we pay it out (C2).
        bytes32 opId = keccak256(abi.encode("vault_release", blueprintHash));
        profit_token.requireVaultReconciliation(net, opId);

        // ── Effects BEFORE external calls (CEI) ──────────────────────────────
        // Ordering within this block matters, not just relative to the transfers
        // below: pending_profit is zeroed and totalPendingProfit is decremented
        // BEFORE any token movement happens, so a hypothetical reentrant call
        // into rescueERC20 (already independently blocked by the shared
        // nonReentrant lock) would in any case see this blueprint's share
        // already excluded from totalPendingProfit, never double-counted as
        // "surplus" while the transfer below is still in flight.
        released[blueprintHash]         = true;
        pending_profit[blueprintHash]   = 0;
        // Solidity 0.8's checked arithmetic already reverts on underflow (panic
        // 0x11), but that would surface as an opaque panic rather than a named
        // error — and more importantly, this makes "totalPendingProfit can never
        // be decremented below what was actually added" an explicit, auditable
        // assertion at the call site rather than an incidental consequence of
        // the compiler's default checked-math behavior. Should be unreachable in
        // practice: `net <= totalPendingProfit` always holds because every
        // increment of totalPendingProfit (in receivePendingProfit) has exactly
        // one corresponding blueprintHash-scoped decrement (here), gated by
        // `released[blueprintHash]` replay protection preventing double-decrement
        // for the same hash.
        if (net > totalPendingProfit) revert TotalPendingProfitUnderflow(totalPendingProfit, net);
        totalPendingProfit             -= net;
        daily_released                 += net;
        gateState.trackVaultProofClose();

        // ── C9: DAO fee split ─────────────────────────────────────────────────
        uint256 dao_fee   = (net * dao_fee_bps) / 10_000;
        uint256 pil_share = net - dao_fee;

        // Hard invariant check — mathematically implied by dao_fee_bps <= MAX_DAO_FEE_BPS
        // already being enforced on every path that can set dao_fee_bps, but kept as an
        // explicit belt-and-suspenders check rather than relying on that alone.
        if (dao_fee > net / 10)
            revert DaoFeeExceedsMax(dao_fee_bps, MAX_DAO_FEE_BPS);

        profit_token.safeTransfer(pil_treasury,    pil_share);
        profit_token.safeTransfer(dao_fee_address, dao_fee);

        emit ProfitSplit(blueprintHash, pil_share, dao_fee, dao_fee_address);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Emergency fund rescue (v14.1)
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Rescue ERC20 tokens stranded in this contract — airdrops, tokens sent here
    ///         by mistake, or (for `profit_token` specifically) any balance sitting above
    ///         what's currently owed to PIL/DAO.
    /// @dev    UNLIKE the Orchestrator's `rescueERC20`, this one CANNOT sweep unconditionally
    ///         for `profit_token` — a naive "admin can move any balance" rescue on THIS
    ///         contract would let an admin drain funds still legitimately owed to PIL/DAO for
    ///         a blueprint that's deposited but not yet released (awaiting proof/depth). So
    ///         for `profit_token` only, the rescuable amount is capped at
    ///         `balance - totalPendingProfit` — exactly the surplus that ISN'T earmarked for
    ///         any pending release. This preserves the C9 invariant (every dollar of pending
    ///         profit still resolves to PIL+DAO, nothing else) while still allowing recovery
    ///         of genuine dust (e.g. a rounding remainder, or `profit_token` sent here
    ///         directly rather than via `receivePendingProfit`).
    ///         For any OTHER ERC20 (an airdrop, or a different token sent by mistake), the
    ///         full balance is rescuable — this Vault has no legitimate claim on it in the
    ///         first place, `totalPendingProfit` only ever tracks `profit_token`.
    function rescueERC20(
        address token,
        address to,
        uint256 amount
    ) external onlyRole(DEFAULT_ADMIN_ROLE) nonReentrant {
        if (to == address(0)) revert ZeroAddress();

        if (token == address(profit_token)) {
            uint256 rescuable = _rescuableProfitTokenSurplus();
            if (amount > rescuable) revert InsufficientRescuableBalance(amount, rescuable);
        }

        IERC20(token).safeTransfer(to, amount);
        emit FundsRescued(token, to, amount);
    }

    /// @notice Rescue ETH stranded in this contract.
    /// @dev    This contract has no `receive()`/`fallback()`, so a plain ETH transfer already
    ///         reverts before landing here — but ETH can still be force-sent via
    ///         `selfdestruct` from another contract. This is the recovery path for that case.
    ///         ETH has no encumbrance concept here (the Vault's accounting is entirely in
    ///         `profit_token`), so the full balance is always rescuable.
    function rescueETH(
        address payable to,
        uint256 amount
    ) external onlyRole(DEFAULT_ADMIN_ROLE) nonReentrant {
        if (to == address(0)) revert ZeroAddress();
        (bool success, ) = to.call{value: amount}("");
        if (!success) revert EthTransferFailed();
        emit EthRescued(to, amount);
    }

    /// @notice The amount of `profit_token` currently rescuable — i.e. this contract's
    ///         actual balance minus `totalPendingProfit` (what's owed to PIL/DAO across every
    ///         blueprint awaiting release). Exposed so off-chain tooling/relayers can check
    ///         before calling `rescueERC20`, rather than guessing and hitting
    ///         `InsufficientRescuableBalance`.
    function rescuableProfitTokenSurplus() external view returns (uint256) {
        return _rescuableProfitTokenSurplus();
    }

    function _rescuableProfitTokenSurplus() internal view returns (uint256) {
        uint256 balance = profit_token.balanceOf(address(this));
        return balance > totalPendingProfit ? balance - totalPendingProfit : 0;
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

    /// @notice Number of blueprints with profit deposited but not yet released. Exposed so
    ///         OTHER contracts (e.g. Orchestrator, before finalizing a key rotation) can
    ///         confirm nothing is stuck mid-flight in this Vault.
    /// @dev    SNAPSHOT, NOT A LOCK. This reflects state at the moment it's read. A caller
    ///         that checks this, then performs further actions, then relies on the value
    ///         still being accurate is checking a point-in-time observation, not an atomic
    ///         guarantee — a new deposit can land between the read and whatever depends on
    ///         it. Rotation/registration gating built on this getter (see Orchestrator) is
    ///         guaranteed only with respect to the state observed at validation time; any
    ///         profit deposited afterward is part of the next operational epoch, not a
    ///         violation of the guard that already ran.
    function pendingProofCount() external view returns (uint256) {
        return gateState.pendingVaultProofsCount(); // library view helper, reads this Vault's own state
    }

    /// @notice Deterministically recomputes the ZK public-inputs hash that `blueprintHash`
    ///         is (or would be) bound to for a given `netProfit`, using the exact same
    ///         encoding `receivePendingProfit` uses internally. Exposed as `public view` so
    ///         off-chain relayers/provers, indexers, and verification tooling can compute it
    ///         without duplicating (and risking drift from) the binding logic.
    /// @dev    PROTOCOL COMMITMENT — the encoding below is not an implementation detail, it
    ///         is the public inputs schema for every STARK proof accepted by this contract.
    ///         Any prover circuit built against `stark_verifier` must produce a proof whose
    ///         public inputs hash to exactly this value, computed exactly this way. Changing
    ///         any of the following is a BREAKING protocol change, not a refactor, and
    ///         requires bumping `PUBLIC_INPUTS_VERSION` (folded into the hash below) so old
    ///         and new schemes can never collide or be silently accepted for each other:
    ///           - Parameter order: (address(this), blueprintHash, netProfit,
    ///             address(profit_token)) — in exactly this order.
    ///           - Encoding: `abi.encode`, NOT `abi.encodePacked`. `abi.encode` pads every
    ///             argument to a fixed 32-byte word, which is what makes the encoding
    ///             injective over these argument types (no two distinct input tuples can
    ///             encode to the same bytes). `abi.encodePacked` would NOT have this
    ///             property here — e.g. packed encoding cannot in general distinguish
    ///             different splits of adjacent dynamic-length-adjacent fields, and even for
    ///             this specific fixed-width tuple, switching encodings changes every
    ///             historically-bound hash, so this must stay `abi.encode`.
    ///           - Domain separation: `address(this)` (the Vault's own address) is included
    ///             specifically so a proof bound against one Vault deployment cannot be
    ///             replayed against a different Vault deployment sharing the same
    ///             `stark_verifier` (e.g. a staging/canary deployment vs. production).
    ///           - Versioning: `PUBLIC_INPUTS_VERSION` is folded in first so that if the
    ///             schema ever changes (e.g. adding a recipient field, a nonce, or a chain
    ///             id), the new scheme's hashes are guaranteed non-colliding with every hash
    ///             ever bound under the old scheme, rather than relying on the new field
    ///             values happening not to collide by chance.
    ///         A Certora/CVL spec that wants to assert bindings are "semantically bound, not
    ///         opaque" should call THIS function, not reimplement the hash inline — that
    ///         keeps the specification checking protocol *behavior* against this single
    ///         source of truth, rather than maintaining a second hashing implementation that
    ///         could silently diverge from this one after a future change here.
    uint256 public constant PUBLIC_INPUTS_VERSION = 1;

    function computePublicInputsHash(
        bytes32 blueprintHash,
        uint256 netProfit
    ) public view returns (bytes32) {
        return keccak256(
            abi.encode(PUBLIC_INPUTS_VERSION, address(this), blueprintHash, netProfit, address(profit_token))
        );
    }

    /// @notice Whether a blueprint's ZK public inputs have been bound yet.
    function proofInputsBound(bytes32 blueprintHash) external view returns (bool) {
        return gateState.proofInputsBound[blueprintHash];
    }

    /// @notice The bound ZK public-inputs hash for a blueprint (zero if never bound).
    function boundProofInputsOf(bytes32 blueprintHash) external view returns (bytes32) {
        return gateState.boundProofInputs[blueprintHash];
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// IStarkVerifier interface
// ─────────────────────────────────────────────────────────────────────────────
interface IStarkVerifier {
    /// @dev `publicInputsHash` must be included in what the proof attests to — a verifier
    ///      that ignores this parameter reintroduces the C4 gap this version closes on-chain.
    function verify(
        bytes calldata proof,
        bytes32 blueprintHash,
        bytes32 publicInputsHash
    ) external view returns (bool);
}
