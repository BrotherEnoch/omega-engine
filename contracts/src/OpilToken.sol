// contracts/src/OpilToken.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/extensions/ERC20Permit.sol";
import "@openzeppelin/contracts/token/ERC20/extensions/ERC20Votes.sol";
import "@openzeppelin/contracts/access/AccessControl.sol";

/// @title OpilToken — v13 Final
/// @notice ERC-20 + ERC-20Permit + ERC20Votes
///         Represents a share of Omega Profit Interest Liability pool.
///         Minted by PIL treasury only on CONFIRMED profit (not pending).
///         Burned on yield redemption.
///
/// @dev    Anti-flash-loan governance protection:
///           A holder's voting power is locked until their tokens have been held, on a
///           balance-weighted-average basis, for 7 continuous days. See _update() below
///           for exactly how that average is maintained and why.
///
///         OPIL minted on pil_share only (not gross netProfit). OPIL holders
///         are NOT diluted by the 5% DAO fee — minting is always post-split.
///
/// CHANGES vs prior version — these are not stylistic, the prior file would not have
/// compiled or would have behaved incorrectly against OpenZeppelin 5.x:
///   1. FIXED (would not compile) — OpenZeppelin 5.x removed `_beforeTokenTransfer` /
///      `_afterTokenTransfer` entirely, and `_mint`/`_burn` are no longer virtual/overridable.
///      All transfer/mint/burn hooking now goes through a single `_update(from, to, value)`
///      function. The prior file's overrides don't exist in this OZ version and would fail
///      to compile. Rewritten around `_update`.
///   2. FIXED (real, exploitable governance bug) — the prior design reset a holder's entire
///      lock timestamp to `block.timestamp` on ANY incoming transfer, including a 1-wei
///      transfer from an arbitrary address. That means anyone could grief any large,
///      long-standing holder's voting power to zero right before a vote, for the cost of one
///      dust transfer — cheaper and easier than the flash-loan attack this was meant to
///      prevent. Fixed with a balance-weighted-average lock timestamp: a small top-up barely
///      moves a holder's unlock time, while a large fresh inflow (e.g. a flash-loaned
///      position) is appropriately treated as mostly-new and pulls the average close to now.
///   3. FIXED (real bug, silent under normal use) — OpenZeppelin 5.x's `ERC20Votes` defaults
///      `clock()` to block-number mode, but the vote-lock check compared that `timepoint`
///      directly against a `block.timestamp`-based `holding_since` value. Block numbers and
///      unix timestamps are on wildly different scales, so `getPastVotes` would have
///      returned 0 in nearly every real call — historical vote lookups would have been
///      silently broken. Fixed by overriding `clock()`/`CLOCK_MODE()` to timestamp mode, so
///      everything the lock logic compares is in the same units.
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

    /// @notice Balance-weighted-average "since" timestamp for each holder. NOT simply "the
    ///         last time this address received any tokens" — see _update() for the weighting.
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
    ///         ZERO if the account's weighted-average holding start is fewer than 7 days old.
    function getVotes(address account) public view override returns (uint256) {
        if (block.timestamp < holding_since[account] + VOTE_LOCK_DURATION) {
            return 0;
        }
        return super.getVotes(account);
    }

    /// @notice Past votes also respect the lock — if the lock hadn't expired as of the
    ///         snapshot timepoint, return 0. `timepoint` is a unix timestamp (see clock()
    ///         override below), matching the units `holding_since` is stored in.
    function getPastVotes(
        address account,
        uint256 timepoint
    ) public view override returns (uint256) {
        if (timepoint < holding_since[account] + VOTE_LOCK_DURATION) {
            return 0;
        }
        return super.getPastVotes(account, timepoint);
    }

    /// @dev Switch ERC20Votes/Votes checkpointing to timestamp mode instead of the OZ5
    ///      default (block number). This must match the units used by holding_since/
    ///      VOTE_LOCK_DURATION above, or getPastVotes' timepoint comparisons are meaningless.
    function clock() public view override returns (uint48) {
        return uint48(block.timestamp);
    }

    // solhint-disable-next-line func-name-mixedcase
    function CLOCK_MODE() public pure override returns (string memory) {
        return "mode=timestamp";
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Required override — OZ5's ERC20Permit and ERC20Votes both inherit Nonces
    // ─────────────────────────────────────────────────────────────────────────
    function nonces(address owner) public view override(ERC20Permit, Nonces) returns (uint256) {
        return super.nonces(owner);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Internal override — single entry point for mint/burn/transfer in OZ 5.x
    // ─────────────────────────────────────────────────────────────────────────

    /// @dev Maintains a balance-weighted-average `holding_since` for the recipient on every
    ///      mint or transfer (burns don't need any update — only the sender's balance drops,
    ///      and a sender's own lock timestamp is untouched by sending tokens away).
    ///
    ///      - Brand new holder (balance was 0 before this): lock starts fresh, now.
    ///      - Existing holder receiving more tokens: new lock timestamp is the balance-weighted
    ///        average of their old lock timestamp and "now", weighted by old balance vs
    ///        incoming amount. A 1-wei top-up moves the average by a negligible amount; a
    ///        large fresh inflow pulls the average close to "now", which is exactly the
    ///        anti-flash-loan behavior this token wants for large new stakes — without also
    ///        making a long-time holder griefable via dust transfers.
    function _update(
        address from,
        address to,
        uint256 value
    ) internal override(ERC20, ERC20Votes) {
        super._update(from, to, value);

        if (to != address(0)) {
            uint256 balanceAfter  = balanceOf(to);
            uint256 balanceBefore = balanceAfter - value; // safe: balanceAfter >= value always, post-transfer

            if (balanceBefore == 0) {
                holding_since[to] = block.timestamp;
            } else {
                holding_since[to] =
                    (balanceBefore * holding_since[to] + value * block.timestamp) / balanceAfter;
            }
        }
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