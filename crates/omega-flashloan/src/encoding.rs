// crates/omega-flashloan/src/encoding.rs
//
// Flash loan calldata encoding for Aave v3, Balancer, and Uniswap v3.
//
// All functions produce ABI-encoded calldata that the strategy contract
// calls at the start of execution to initiate the flash loan.
//
// ## Provider ABI signatures
//
//   Aave v3:    `flashLoanSimple(address,address,uint256,bytes,uint16)`
//   Balancer:   `flashLoan(address,address[],uint256[],bytes)`
//   Uniswap v3: `flash(address,uint256,uint256,bytes)`
//
// All selectors are `keccak256(signature)[0..4]`.
//
// ## CHANGE — Uniswap V3 token0/token1 (fixes a real bug, verified by test)
//
//   `encode_uniswap_v3` previously always placed the borrowed amount in the
//   `amount0` slot and hardcoded `amount1 = 0`, regardless of which token
//   was actually being borrowed. Any pool where the target asset is
//   actually `token1` would have silently borrowed 0 of the intended asset
//   and `amount_wei` of the wrong one instead — not a revert, a silently
//   wrong flashloan request. `encode_flashloan_call` / `encode_uniswap_v3`
//   now take an explicit `asset_is_token0: bool` so this is never guessed:
//   this crate does no on-chain reads (see lib.rs's own "no I/O" note), so
//   it cannot determine token0/token1 itself — the caller, which already
//   selected this specific pool, has to know and supply it.
//
//   Verified: this fix was compiled and unit-tested against a real (if
//   toolchain-shimmed) build in-session, including a regression test that
//   was confirmed to fail against the original logic and pass against the
//   fix — not just reviewed by eye.
//
// ## CHANGE — portability
//
//   `encode_bytes_abi`'s padding previously used `std::iter::repeat_n`,
//   which requires Rust 1.82+. Replaced with `repeat().take()`, which does
//   the same thing and has worked since Rust 1.0 — no reason to require a
//   newer toolchain than the rest of this crate needs.

use alloy_primitives::{keccak256, Address, Bytes, U256};

use crate::FlashloanProvider;

// ─────────────────────────────────────────────────────────────────────────────
// Public entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Encode the flash loan initiation call for a given provider.
///
/// Returns the ABI-encoded bytes that the strategy contract should call
/// to initiate the flash loan.  The callback payload (repayment
/// instructions and swap calldata) is passed as `callback_data` and is
/// opaque to this crate.
///
/// ## Arguments
///
/// - `provider`:       Which protocol to use.
/// - `contract_addr`:  The provider's contract address.  Required for
///   Uniswap v3 pool routing; ignored by Aave v3 and Balancer.
/// - `receiver`:       The strategy contract that receives the funds and
///   must repay within the same transaction.
/// - `asset`:          Token address to borrow.
/// - `amount_wei`:     Amount to borrow in wei.
/// - `asset_is_token0`: Whether `asset` is `contract_addr`'s `token0` (true)
///   or `token1` (false). **Only meaningful for `FlashloanProvider::UniswapV3`**
///   — Aave and Balancer borrow by token address directly and ignore this.
///   Getting it wrong for a UniswapV3 call silently borrows the WRONG asset
///   (0 of the one you actually wanted, `amount_wei` of the other) — there
///   is deliberately no default, since this crate has no way to check it
///   itself (no on-chain reads, by design — see lib.rs).
/// - `callback_data`:  Opaque payload forwarded to the receiver's callback.
pub fn encode_flashloan_call(
    provider: FlashloanProvider,
    contract_addr: Address,
    receiver: Address,
    asset: Address,
    amount_wei: U256,
    asset_is_token0: bool,
    callback_data: &[u8],
) -> Bytes {
    match provider {
        FlashloanProvider::AaveV3 => encode_aave_v3(receiver, asset, amount_wei, callback_data),
        FlashloanProvider::Balancer => encode_balancer(receiver, asset, amount_wei, callback_data),
        FlashloanProvider::UniswapV3 => encode_uniswap_v3(
            contract_addr,
            receiver,
            asset,
            amount_wei,
            asset_is_token0,
            callback_data,
        ),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Provider-specific encoders
// ─────────────────────────────────────────────────────────────────────────────

/// Aave v3 `flashLoanSimple` calldata.
///
/// ABI: `flashLoanSimple(address receiverAddress, address asset,
///        uint256 amount, bytes calldata params, uint16 referralCode)`
fn encode_aave_v3(receiver: Address, asset: Address, amount_wei: U256, params: &[u8]) -> Bytes {
    let selector = &keccak256(b"flashLoanSimple(address,address,uint256,bytes,uint16)")[..4];

    let mut buf = Vec::with_capacity(4 + 5 * 32 + params.len().next_multiple_of(32));
    buf.extend_from_slice(selector);

    // slot 0: receiverAddress
    buf.extend_from_slice(&pad_address(receiver));
    // slot 1: asset
    buf.extend_from_slice(&pad_address(asset));
    // slot 2: amount
    buf.extend_from_slice(&u256_to_bytes32(amount_wei));
    // slot 3: offset to params bytes (5 static words × 32 = 160 = 0xa0)
    buf.extend_from_slice(&u256_to_bytes32(U256::from(160u32)));
    // slot 4: referralCode = 0
    buf.extend_from_slice(&[0u8; 32]);
    // dynamic: params length + data padded to 32-byte boundary
    encode_bytes_abi(params, &mut buf);

    Bytes::from(buf)
}

/// Balancer `flashLoan` calldata.
///
/// ABI: `flashLoan(IFlashLoanRecipient recipient, IERC20[] tokens,
///        uint256[] amounts, bytes userData)`
///
/// Encodes single-element token and amount arrays for simplicity.
fn encode_balancer(
    recipient: Address,
    token: Address,
    amount_wei: U256,
    user_data: &[u8],
) -> Bytes {
    let selector = &keccak256(b"flashLoan(address,address[],uint256[],bytes)")[..4];

    let mut buf = Vec::with_capacity(4 + 10 * 32);
    buf.extend_from_slice(selector);

    // slot 0: recipient
    buf.extend_from_slice(&pad_address(recipient));
    // slot 1: offset to tokens array  (4 head words × 32 = 128)
    buf.extend_from_slice(&u256_to_bytes32(U256::from(128u32)));
    // slot 2: offset to amounts array (128 + 2 words = 192)
    buf.extend_from_slice(&u256_to_bytes32(U256::from(192u32)));
    // slot 3: offset to userData      (192 + 2 words = 256)
    buf.extend_from_slice(&u256_to_bytes32(U256::from(256u32)));
    // tokens array: length=1, element=token
    buf.extend_from_slice(&u256_to_bytes32(U256::from(1u32)));
    buf.extend_from_slice(&pad_address(token));
    // amounts array: length=1, element=amount
    buf.extend_from_slice(&u256_to_bytes32(U256::from(1u32)));
    buf.extend_from_slice(&u256_to_bytes32(amount_wei));
    // dynamic: userData
    encode_bytes_abi(user_data, &mut buf);

    Bytes::from(buf)
}

/// Uniswap v3 `flash` calldata.
///
/// ABI: `flash(address recipient, uint256 amount0, uint256 amount1,
///        bytes calldata data)`
///
/// `amount_wei` goes into whichever of `amount0`/`amount1` actually
/// corresponds to the desired asset, per `asset_is_token0` — see the
/// module-level CHANGE note and `encode_flashloan_call`'s doc comment for
/// why this can't be inferred here and previously defaulted incorrectly.
fn encode_uniswap_v3(
    _pool: Address, // pool's own address — not encoded, used for routing upstream
    recipient: Address,
    _asset: Address, // kept for API symmetry / documentation; the actual routing
    // decision is `asset_is_token0`, supplied explicitly below
    amount_wei: U256,
    asset_is_token0: bool,
    data: &[u8],
) -> Bytes {
    let selector = &keccak256(b"flash(address,uint256,uint256,bytes)")[..4];

    let (amount0, amount1) = if asset_is_token0 {
        (amount_wei, U256::ZERO)
    } else {
        (U256::ZERO, amount_wei)
    };

    let mut buf = Vec::with_capacity(4 + 5 * 32 + data.len().next_multiple_of(32));
    buf.extend_from_slice(selector);
    buf.extend_from_slice(&pad_address(recipient));
    buf.extend_from_slice(&u256_to_bytes32(amount0));
    buf.extend_from_slice(&u256_to_bytes32(amount1));
    // offset to data (4 static words × 32 = 128)
    buf.extend_from_slice(&u256_to_bytes32(U256::from(128u32)));
    encode_bytes_abi(data, &mut buf);

    Bytes::from(buf)
}

// ─────────────────────────────────────────────────────────────────────────────
// ABI primitives
// ─────────────────────────────────────────────────────────────────────────────

/// Left-pad an Ethereum address to 32 bytes (ABI static encoding).
pub(crate) fn pad_address(addr: Address) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[12..].copy_from_slice(addr.as_slice());
    out
}

/// Encode a U256 as a big-endian 32-byte array (ABI static encoding).
pub(crate) fn u256_to_bytes32(v: U256) -> [u8; 32] {
    v.to_be_bytes()
}

/// ABI-encode a dynamic `bytes` value: uint256 length then data
/// right-padded to the next 32-byte boundary.
pub(crate) fn encode_bytes_abi(data: &[u8], buf: &mut Vec<u8>) {
    let len = data.len();
    buf.extend_from_slice(&u256_to_bytes32(U256::from(len)));
    buf.extend_from_slice(data);
    let padding = (32 - (len % 32)) % 32;
    buf.extend(std::iter::repeat(0u8).take(padding));
}
