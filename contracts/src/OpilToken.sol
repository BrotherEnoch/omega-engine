// contracts/src/OpilToken.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/extensions/ERC20Permit.sol";
import "@openzeppelin/contracts/token/ERC20/extensions/ERC20Votes.sol";
import "@openzeppelin/contracts/access/AccessControl.sol";

/// @title OpilToken — v12 Final
/// @notice ERC-20 + ERC-20Permit + ERC20Votes
///         Represents a share of Omega Profit Interest Liability pool.
///         Minted by PIL treasury only on CONFIRMED profit (not pending).
///         Burned on yield redemption.
///
/// @dev    Anti-flash-loan governance protection:
///           7-day vote-power lock — voting power accrues only after 7 days of
///           continuous holding. Transferring resets the clock for the recipient.
///           Cost of a flash-loan governance attack = opportunity cost of
///           locking capital for 7 days.
///
///         OPIL minted on pil_share only (not gross netProfit). OPIL holders
///         are NOT diluted by the 5% DAO fee — minting is always post-split.
contract OpilToken is ERC20Permit, ERC20Votes, AccessControl {

    // ─────────────────────────────────────────────────────────────────────────
    // Roles
    // ─────────────────────────────────────────────────────────────────────────
    bytes32 public constant MINTER_ROLE = keccak256("MINTER");  // PIL treasury only
    bytes32 public constant BURNER_ROLE = keccak256("BURNER");  // PIL treasury only

    // ─────────────────────────────────────────────────────────────────────────
    // Vote-power lock
    // ─────────────────────────────────────────────────────────────────────────
    uint256 public constant VOTE_LOCK_DURATION = 7 days;

    /// @notice Timestamp at which a given address last received tokens.
    ///         Voting power is zero until block.timestamp >= holding_since[account] + 7 days.
    mapping(address => uint256) public holding_since;

    // ─────────────────────────────────────────────────────────────────────────
    // Events
    // ─────────────────────────────────────────────────────────────────────────
    event Minted(address indexed to, uint256 amount);
    event Burned(address indexed from, uint256 amount);

    // ─────────────────────────────────────────────────────────────────────────
    // Errors
    // ─────────────────────────────────────────────────────────────────────────
    error ZeroAmount();
    error ZeroAddress();

    // ─────────────────────────────────────────────────────────────────────────
    // Constructor
    // ─────────────────────────────────────────────────────────────────────────
    constructor(
        address _pilTreasury,
        address _admin
    )
        ERC20("Omega Profit Interest Liability", "OPIL")
        ERC20Permit("OPIL")
    {
        if (_pilTreasury == address(0) || _admin == address(0)) revert ZeroAddress();
        _grantRole(DEFAULT_ADMIN_ROLE, _admin);
        _grantRole(MINTER_ROLE,        _pilTreasury);
        _grantRole(BURNER_ROLE,        _pilTreasury);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Mint / Burn
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Mint OPIL to an address. Called by PIL treasury on confirmed profit.
    ///         Amount is based on pil_share only — never gross netProfit.
    function mint(address to, uint256 amount) external onlyRole(MINTER_ROLE) {
        if (to == address(0)) revert ZeroAddress();
        if (amount == 0) revert ZeroAmount();
        _mint(to, amount);
        emit Minted(to, amount);
    }

    /// @notice Burn OPIL from an address. Called by PIL treasury on yield redemption.
    function burn(address from, uint256 amount) external onlyRole(BURNER_ROLE) {
        if (from == address(0)) revert ZeroAddress();
        if (amount == 0) revert ZeroAmount();
        _burn(from, amount);
        emit Burned(from, amount);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Vote-power lock override
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Returns voting power for an account.
    ///         ZERO if the account has held tokens for fewer than 7 days.
    ///         This prevents flash-loan governance attacks.
    function getVotes(address account) public view override returns (uint256) {
        if (block.timestamp < holding_since[account] + VOTE_LOCK_DURATION) {
            return 0;
        }
        return super.getVotes(account);
    }

    /// @notice Past votes also respect the lock — if the lock hasn't expired
    ///         at the time of the snapshot, return 0.
    function getPastVotes(
        address account,
        uint256 timepoint
    ) public view override returns (uint256) {
        // If the account received tokens after the timepoint snapshot,
        // they had no votes at that point anyway — standard ERC20Votes handles this.
        // Additionally enforce: lock must have expired by timepoint.
        if (timepoint < holding_since[account] + VOTE_LOCK_DURATION) {
            return 0;
        }
        return super.getPastVotes(account, timepoint);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Internal overrides — update holding_since on every receive
    // ─────────────────────────────────────────────────────────────────────────

    function _afterTokenTransfer(
        address from,
        address to,
        uint256 amount
    ) internal override(ERC20, ERC20Votes) {
        super._afterTokenTransfer(from, to, amount);
        // Reset vote lock clock for the recipient on every receipt.
        // Burns (to == address(0)) and mints (from == address(0)) also handled.
        if (to != address(0)) {
            holding_since[to] = block.timestamp;
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Required overrides (diamond inheritance resolution)
    // ─────────────────────────────────────────────────────────────────────────

    function _mint(
        address to,
        uint256 amount
    ) internal override(ERC20, ERC20Votes) {
        super._mint(to, amount);
    }

    function _burn(
        address from,
        uint256 amount
    ) internal override(ERC20, ERC20Votes) {
        super._burn(from, amount);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // View helpers
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Returns seconds remaining until an account's votes unlock.
    ///         Returns 0 if already unlocked.
    function voteUnlockIn(address account) external view returns (uint256) {
        uint256 unlockAt = holding_since[account] + VOTE_LOCK_DURATION;
        if (block.timestamp >= unlockAt) return 0;
        return unlockAt - block.timestamp;
    }

    /// @notice Returns the timestamp at which an account's votes will unlock.
    function voteUnlockAt(address account) external view returns (uint256) {
        return holding_since[account] + VOTE_LOCK_DURATION;
    }
}
