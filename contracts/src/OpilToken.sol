// contracts/src/OpilToken.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/extensions/ERC20Votes.sol";
import "@openzeppelin/contracts/access/AccessControl.sol";

/// @title OpilToken -- v13 Final (OZ 4.9.6 port)
/// @notice ERC-20 + ERC-20Permit + ERC20Votes
///         Represents a share of Omega Profit Interest Liability pool.
///         Minted by PIL treasury only on CONFIRMED profit (not pending).
///         Burned on yield redemption.
///
/// @dev Anti-flash-loan governance protection:
///      A holder's voting power is locked until their tokens have been held, on a
///      balance-weighted-average basis, for 7 continuous days. See
///      _afterTokenTransfer() for exactly how that average is maintained and why.
///
/// OPIL minted on pil_share only (not gross netProfit). OPIL holders
/// are NOT diluted by the 5% DAO fee -- minting is always post-split.
///
/// ## Port notes (OZ 5.x -> 4.9.6)
/// 1. ERC20Votes in 4.9.6 already inherits ERC20Permit -- do NOT also inherit
///    ERC20Permit (or ERC20) directly; confirmed directly from the compiler's
///    own note against the installed library: "abstract contract ERC20Votes
///    is ERC20Permit, IERC5805".
/// 2. OZ 4.9 uses _beforeTokenTransfer / _afterTokenTransfer, not _update.
///    The weighted-lock update runs in _afterTokenTransfer after super, so
///    balanceOf(to) already reflects the transfer (same arithmetic as the
///    OZ5 post-_update path).
/// 3. No separate Nonces base class in 4.9.6 -- nonces() is provided solely
///    by ERC20Permit via Counters. The dual-override nonces() is deleted.
/// 4. clock() still overridden to timestamp mode so holding_since (unix time)
///    and getPastVotes timepoints share units. CLOCK_MODE() is `pure`, not
///    `view` -- it returns a fixed literal ("mode=timestamp") and, per the note
///    below, deliberately never calls super, so nothing in its body reads
///    contract state at all. It was originally left as `view` defensively, but
///    the compiler correctly flags that as stricter than necessary (Warning
///    2018), and there's no reason to keep a wider mutability annotation than
///    the function actually needs.
///    CLOCK_MODE does not call super, regardless of whether 4.9.6 enforces a
///    hard consistency check at runtime: calling super would return the
///    wrong description string ("blocknumber" instead of "timestamp")
///    either way, so returning the literal string directly is correct
///    independent of that detail.
contract OpilToken is ERC20Votes, AccessControl {
    // -----------------------------------------------------------------------
    // Roles
    // -----------------------------------------------------------------------
    bytes32 public constant MINTER_ROLE = keccak256("MINTER"); // PIL treasury only
    bytes32 public constant BURNER_ROLE = keccak256("BURNER"); // PIL treasury only

    // -----------------------------------------------------------------------
    // Vote-power lock
    // -----------------------------------------------------------------------
    uint256 public constant VOTE_LOCK_DURATION = 7 days;

    /// @notice Balance-weighted-average "since" timestamp for each holder.
    /// Voting power is zero until block.timestamp >= holding_since[account] + 7 days.
    mapping(address => uint256) public holding_since;

    // -----------------------------------------------------------------------
    // Events
    // -----------------------------------------------------------------------
    event Minted(address indexed to, uint256 amount);
    event Burned(address indexed from, uint256 amount);

    // -----------------------------------------------------------------------
    // Errors
    // -----------------------------------------------------------------------
    error ZeroAmount();
    error ZeroAddress();

    // -----------------------------------------------------------------------
    // Constructor
    // -----------------------------------------------------------------------
    constructor(
        address _pilTreasury,
        address _admin
    )
        ERC20("Omega Profit Interest Liability", "OPIL")
        ERC20Permit("OPIL")
    {
        if (_pilTreasury == address(0) || _admin == address(0)) revert ZeroAddress();
        _grantRole(DEFAULT_ADMIN_ROLE, _admin);
        _grantRole(MINTER_ROLE, _pilTreasury);
        _grantRole(BURNER_ROLE, _pilTreasury);
    }

    // -----------------------------------------------------------------------
    // Mint / Burn
    // -----------------------------------------------------------------------

    function mint(address to, uint256 amount) external onlyRole(MINTER_ROLE) {
        if (to == address(0)) revert ZeroAddress();
        if (amount == 0) revert ZeroAmount();
        _mint(to, amount);
        emit Minted(to, amount);
    }

    function burn(address from, uint256 amount) external onlyRole(BURNER_ROLE) {
        if (from == address(0)) revert ZeroAddress();
        if (amount == 0) revert ZeroAmount();
        _burn(from, amount);
        emit Burned(from, amount);
    }

    // -----------------------------------------------------------------------
    // Vote-power lock overrides
    // -----------------------------------------------------------------------

    function getVotes(address account) public view override returns (uint256) {
        if (block.timestamp < holding_since[account] + VOTE_LOCK_DURATION) {
            return 0;
        }
        return super.getVotes(account);
    }

    function getPastVotes(
        address account,
        uint256 timepoint
    ) public view override returns (uint256) {
        if (timepoint < holding_since[account] + VOTE_LOCK_DURATION) {
            return 0;
        }
        return super.getPastVotes(account, timepoint);
    }

    function clock() public view override returns (uint48) {
        return uint48(block.timestamp);
    }

    // solhint-disable-next-line func-name-mixedcase
    function CLOCK_MODE() public pure override returns (string memory) {
        return "mode=timestamp";
    }

    // -----------------------------------------------------------------------
    // Transfer hook -- weighted holding_since (OZ 4.9 two-hook model)
    // -----------------------------------------------------------------------

    function _afterTokenTransfer(
        address from,
        address to,
        uint256 amount
    ) internal virtual override(ERC20Votes) {
        super._afterTokenTransfer(from, to, amount);

        if (to != address(0) && amount > 0) {
            uint256 balanceAfter = balanceOf(to);
            uint256 balanceBefore = balanceAfter - amount;
            if (balanceBefore == 0) {
                holding_since[to] = block.timestamp;
            } else {
                holding_since[to] =
                    (balanceBefore * holding_since[to] + amount * block.timestamp) / balanceAfter;
            }
        }
    }

    function _mint(address account, uint256 amount) internal override(ERC20Votes) {
        super._mint(account, amount);
    }

    function _burn(address account, uint256 amount) internal override(ERC20Votes) {
        super._burn(account, amount);
    }

    // -----------------------------------------------------------------------
    // View helpers
    // -----------------------------------------------------------------------

    function voteUnlockIn(address account) external view returns (uint256) {
        uint256 unlockAt = holding_since[account] + VOTE_LOCK_DURATION;
        if (block.timestamp >= unlockAt) return 0;
        return unlockAt - block.timestamp;
    }

    function voteUnlockAt(address account) external view returns (uint256) {
        return holding_since[account] + VOTE_LOCK_DURATION;
    }
}