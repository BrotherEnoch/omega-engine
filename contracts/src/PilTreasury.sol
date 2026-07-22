// contracts/src/PilTreasury.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import "@openzeppelin/contracts/utils/Pausable.sol";
import "@openzeppelin/contracts/access/AccessControl.sol";
import "@openzeppelin/contracts/utils/math/Math.sol";

/// @title PilTreasury — v1
/// @notice The contract OpilToken.sol's own docstring refers to as "PIL treasury" — the
///         thing that actually holds `MINTER_ROLE`/`BURNER_ROLE` on OpilToken and turns
///         confirmed profit into a redeemable claim. This contract did not exist anywhere
///         in the files you'd given me; OpilToken and OmegaVault both referenced its role
///         by name/address but no implementation was ever provided. This is a new file, not
///         a rewrite of something you gave me — flagged clearly because you asked me not to
///         guess, and the single biggest guess in this file is spelled out below.
///
/// @dev    WHAT THIS CONTRACT DOES:
///           - Receives `profit_token` two ways: (a) confirmed profit swept in from
///             OmegaVault.releaseProfit() as a plain ERC20 transfer (no hook fires on that —
///             it just increases this contract's balance), and (b) voluntary deposits from
///             anyone via `deposit()`.
///           - `totalAssets()` is simply `profit_token.balanceOf(address(this))` — since a
///             plain `transfer()` doesn't call back into this contract, there is nothing to
///             "receive" or acknowledge separately; the balance itself IS the state.
///           - OPIL is a claim on that balance, redeemable pro-rata via `redeem()`.
///
///         THE ONE DECISION I MADE INSTEAD OF ASKING (flagged, not hidden):
///           OpilToken.sol's own docstring says OPIL is "minted... on confirmed profit" —
///           read literally, that means every single profit sweep should mint new OPIL to
///           *someone*. But no file anywhere names who that recipient should be, and
///           inventing a recipient for newly-printed governance-weighted tokens is exactly
///           the kind of decision I was told not to guess at.
///
///           So instead, this contract treats confirmed-profit sweeps as *value accrual to
///           existing OPIL holders* (their existing shares become worth more of the
///           underlying token — nothing new is minted), and reserves minting for the one case
///           where a recipient is unambiguous: someone voluntarily depositing `profit_token`
///           in exchange for shares, via `deposit()`. This is the standard share/vault pattern
///           (ERC-4626 in spirit, though this contract isn't the share token itself — OpilToken
///           is a separate, already-deployed contract, so I couldn't inherit OZ's ERC4626
///           directly and instead call out to it as an external contract).
///
///           This still satisfies everything that's explicitly promised elsewhere:
///             - "OPIL represents a share of the PIL pool"        — yes, via totalAssets/totalSupply.
///             - "Burned on yield redemption"                     — yes, exactly what redeem() does.
///             - "OPIL holders not diluted by the DAO fee"        — trivially true: the DAO's 5%
///               cut never enters this contract's balance at all (Vault sends it elsewhere),
///               so it can't dilute anything here regardless of mint/accrual mechanics.
///
///           What it does NOT do: mint new OPIL directly to some address on every profit
///           sweep. If your actual intended design is "print OPIL to specific party X on every
///           confirmed profit event" rather than "existing holders' shares appreciate", tell me
///           who X is and I'll change this — I'm not going to invent them.
///
///         DEPLOYMENT ORDER (there's a real circular dependency here):
///           OpilToken's constructor takes `_pilTreasury` and grants it MINTER_ROLE/BURNER_ROLE
///           at construction time — so OpilToken needs this contract's address before it can be
///           deployed. But this contract calls INTO OpilToken (mint/burn/totalSupply), so it
///           needs OpilToken's address too. To avoid requiring CREATE2 address precomputation
///           (an assumption about your deployment tooling I'm not going to make), `opil_token`
///           here is NOT an immutable constructor argument — it's set once, after both
///           contracts exist, via `setOpilToken()`. Deploy order:
///             1. Deploy PilTreasury(profit_token, admin, vault_address_for_sanity_check).
///             2. Deploy OpilToken(pilTreasuryAddress, admin).
///             3. Call PilTreasury.setOpilToken(opilTokenAddress) once.
contract PilTreasury is ReentrancyGuard, Pausable, AccessControl {
    using SafeERC20 for IERC20;
    using Math for uint256;

    // ─────────────────────────────────────────────────────────────────────────
    // Immutables / state
    // ─────────────────────────────────────────────────────────────────────────
    IERC20     public immutable profit_token;
    IOpilToken public opil_token;          // set once post-deploy, see note above
    bool       private _opilTokenSet;

    // ─────────────────────────────────────────────────────────────────────────
    // Events
    // ─────────────────────────────────────────────────────────────────────────
    event OpilTokenSet(address indexed opilToken);
    event Deposited(address indexed depositor, address indexed receiver, uint256 assets, uint256 shares);
    event Redeemed(address indexed owner, address indexed receiver, uint256 assets, uint256 shares);

    // ─────────────────────────────────────────────────────────────────────────
    // Errors
    // ─────────────────────────────────────────────────────────────────────────
    error ZeroAddress();
    error ZeroAmount();
    error ZeroShares();
    error OpilTokenAlreadySet();
    error OpilTokenNotSet();
    error VaultTokenMismatch(address vaultProfitToken, address thisProfitToken);

    // ─────────────────────────────────────────────────────────────────────────
    // Constructor
    // ─────────────────────────────────────────────────────────────────────────

    /// @param _profitToken   MUST be the exact same token address as OmegaVault's
    ///                       immutable `profit_token` — this contract has no other way to
    ///                       verify that on every call, only at construction (see below).
    /// @param _admin         Gets DEFAULT_ADMIN_ROLE (pause/unpause, setOpilToken).
    /// @param _vaultForCheck Optional. If non-zero, this constructor calls
    ///                       `IOmegaVaultProfitToken(_vaultForCheck).profit_token()` and
    ///                       reverts on mismatch — a cheap, real guard against the single
    ///                       easiest deployment mistake here (passing the wrong token address
    ///                       so Vault's transfers land in a balance nobody is accounting for
    ///                       correctly). Pass address(0) to skip the check if you don't have
    ///                       the Vault deployed yet at this point in your deploy sequence.
    constructor(address _profitToken, address _admin, address _vaultForCheck) {
        if (_profitToken == address(0) || _admin == address(0)) revert ZeroAddress();

        if (_vaultForCheck != address(0)) {
            address vaultToken = address(IOmegaVaultProfitToken(_vaultForCheck).profit_token());
            if (vaultToken != _profitToken) revert VaultTokenMismatch(vaultToken, _profitToken);
        }

        profit_token = IERC20(_profitToken);
        _grantRole(DEFAULT_ADMIN_ROLE, _admin);
    }

    /// @notice One-time wiring of the OpilToken address (see deployment-order note above).
    function setOpilToken(address _opilToken) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (_opilTokenSet) revert OpilTokenAlreadySet();
        if (_opilToken == address(0)) revert ZeroAddress();
        opil_token = IOpilToken(_opilToken);
        _opilTokenSet = true;
        emit OpilTokenSet(_opilToken);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Accounting
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Total profit_token held by this contract. This is the ONLY source of truth —
    ///         there is no separately-tracked accounting variable to drift out of sync with
    ///         it, because a plain ERC20 transfer (which is exactly how OmegaVault.releaseProfit
    ///         pays this contract) never calls back into this contract to update one.
    function totalAssets() public view returns (uint256) {
        return profit_token.balanceOf(address(this));
    }

    /// @dev Converts an asset amount to a share amount, rounding down, using the same
    ///      virtual-offset formula OpenZeppelin's own ERC4626 uses by default
    ///      (`assets * (totalSupply + 1) / (totalAssets + 1)`). This isn't decorative — it's
    ///      the standard defense against the classic ERC4626 "first depositor inflation"
    ///      attack, where an attacker mints 1 wei of shares then donates a large amount of the
    ///      underlying directly to the vault to round everyone else's deposit down to zero
    ///      shares. Borrowed deliberately from OZ's already-audited approach rather than
    ///      inventing a new one.
    function _convertToShares(uint256 assets, uint256 assetsBefore, uint256 supply) internal pure returns (uint256) {
        return assets.mulDiv(supply + 1, assetsBefore + 1, Math.Rounding.Floor);
    }

    function _convertToAssets(uint256 shares, uint256 assetsNow, uint256 supply) internal pure returns (uint256) {
        return shares.mulDiv(assetsNow + 1, supply + 1, Math.Rounding.Floor);
    }

    /// @notice Preview how many shares a deposit of `assets` would currently mint.
    function previewDeposit(uint256 assets) external view returns (uint256) {
        return _convertToShares(assets, totalAssets(), opil_token.totalSupply());
    }

    /// @notice Preview how many assets redeeming `shares` would currently return.
    function previewRedeem(uint256 shares) external view returns (uint256) {
        return _convertToAssets(shares, totalAssets(), opil_token.totalSupply());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Deposit — the only path that mints new OPIL (see file header)
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Deposit `assets` of profit_token, minting shares to `receiver` at the current
    ///         exchange rate. Deliberately left permissionless (no role gate): OpilToken
    ///         already carries a 7-day balance-weighted-average vote-power lock specifically
    ///         to defend against a large deposit buying instant governance power — that
    ///         mechanism only makes sense to have built if deposits were meant to be open in
    ///         the first place. If you actually want deposits restricted to specific parties,
    ///         say so and I'll add a role gate; I'm treating the existing anti-flash-loan lock
    ///         as real evidence of intent rather than adding a restriction with no basis.
    function deposit(uint256 assets, address receiver) external nonReentrant whenNotPaused returns (uint256 shares) {
        if (!_opilTokenSet) revert OpilTokenNotSet();
        if (assets == 0) revert ZeroAmount();
        if (receiver == address(0)) revert ZeroAddress();

        uint256 assetsBefore = totalAssets();
        uint256 supply       = opil_token.totalSupply();

        shares = _convertToShares(assets, assetsBefore, supply);
        if (shares == 0) revert ZeroShares();

        profit_token.safeTransferFrom(msg.sender, address(this), assets);
        opil_token.mint(receiver, shares);

        emit Deposited(msg.sender, receiver, assets, shares);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Redeem
    // ─────────────────────────────────────────────────────────────────────────

    /// @notice Burn `shares` of the CALLER's own OPIL for a pro-rata share of profit_token.
    ///         Deliberately NOT paused by `whenNotPaused` — an emergency pause should stop new
    ///         deposits, never trap people who already hold shares and want out. This is a
    ///         considered choice, not an oversight.
    /// @dev    Always burns `msg.sender`'s own balance, never an address the caller supplies —
    ///         this contract holds BURNER_ROLE on OpilToken unconditionally, and
    ///         OpilToken.burn() itself performs no allowance check, so exposing a
    ///         caller-chosen `from` here would let anyone burn anyone else's OPIL. There is no
    ///         `redeemFrom`; there should not be one without a real allowance mechanism.
    function redeem(uint256 shares, address receiver) external nonReentrant returns (uint256 assets) {
        if (!_opilTokenSet) revert OpilTokenNotSet();
        if (shares == 0) revert ZeroAmount();
        if (receiver == address(0)) revert ZeroAddress();

        uint256 assetsNow = totalAssets();
        uint256 supply     = opil_token.totalSupply();

        assets = _convertToAssets(shares, assetsNow, supply);
        if (assets == 0) revert ZeroAmount();

        opil_token.burn(msg.sender, shares);
        profit_token.safeTransfer(receiver, assets);

        emit Redeemed(msg.sender, receiver, assets, shares);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Emergency controls
    // ─────────────────────────────────────────────────────────────────────────

    function pause() external onlyRole(DEFAULT_ADMIN_ROLE) { _pause(); }
    function unpause() external onlyRole(DEFAULT_ADMIN_ROLE) { _unpause(); }
}

// ─────────────────────────────────────────────────────────────────────────────
// Interfaces
// ─────────────────────────────────────────────────────────────────────────────

interface IOpilToken is IERC20 {
    function mint(address to, uint256 amount) external;
    function burn(address from, uint256 amount) external;
}

/// @dev Minimal interface for the one-time constructor sanity check against OmegaVault.
interface IOmegaVaultProfitToken {
    function profit_token() external view returns (IERC20);
}