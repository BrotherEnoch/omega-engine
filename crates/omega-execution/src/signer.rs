// crates/omega-execution/src/signer.rs
//
// TransactionSigner — the gas-paying transaction-envelope signer for the
// ExecutionPipeline (ExecutionPipelineSpecification.md §8).
//
// ## Changelog (most recent first)
//
// - 2026-10-10: fee caps raised (priority 50 -> 500, max fee 500 -> 1500) so blueprints
//   that already emit competitive tips (observed live: 500 gwei) are not rejected by a
//   still-unsigned policy floor. Fail-closed behaviour is unchanged: values above a cap
//   return `SigningFailed` and are never clamped. Still PENDING SIGN-OFF (see below).
//
// - 2026-10-09: ENVELOPE NONCE FIX. The gas-paying tx nonce is no longer `bp.nonce`.
//   Found on a local Anvil fork: every `eth_sendBundle` was rejected with `nonce too low` /
//   `replacement transaction underpriced`. Cause: the Orchestrator's per-strategy REPLAY
//   counter (`next_nonce[strategyId]`, inside the blueprint calldata) was being used as the
//   nonce of the OUTER EIP-1559 envelope. They are unrelated counters:
//     - blueprint nonce: per-strategy, checked by OmegaOrchestrator.execute()
//     - envelope nonce:  per-ACCOUNT (the gas-paying EOA), checked by the node
//   `KeyManagerTransactionSigner` now owns ONE mutex-protected envelope-nonce counter
//   (`tx_nonce`) shared by every strategy and by `next_tx_nonce()` (ZK submitProof path).
//   The caller seeds it at startup, and again after any failed send, with
//   `eth_getTransactionCount(active_address, "pending")` via `seed_tx_nonce`.
//   `sign_transaction` takes the current value and advances it only after a successful
//   signature. `bp.nonce` is used ONLY inside the blueprint calldata.
//   UNSEEDED BEHAVIOUR: until `seed_tx_nonce` is called, the signer falls back to `bp.nonce`
//   and logs a WARN on every signature. That is NOT correct on a real chain; the counter is
//   deliberately not advanced while unseeded. `sign_call` / `sign_call_gwei` take an
//   explicit nonce; ZK callers should obtain it from `next_tx_nonce()`.
//
// - C10l: `orchestrator_bp_hash`. `OmegaVault` keys `pending_profit`, `proofInputsBound` and
//   the ZK public-inputs binding on the Orchestrator's domain-separated `bpHash`
//   (`keccak256(abi.encode(address(this), EXPECTED_CHAIN_ID, blueprintCalldata))`), NOT on
//   `ExecutionBlueprint::compute_hash()`. `orchestrator_bp_hash` is the public, pure
//   accessor (no signing, no key material).
//
// ## ABI / contract status (all RESOLVED against the real OmegaOrchestrator.sol)
//
//   1. ABI encoder: `Blueprint` / `DomainSeparatedBlueprint` / `execute` are `sol!` types
//      whose field ORDER and TYPES are transcribed from `execute()`'s decode statement:
//        (uint64, uint64, bytes32, FlashloanProviderType, address, address,
//         bytes, uint256, uint256, uint256)
//      A struct ABI-encodes like a tuple of the same types, and an enum like its uint8
//      ordinal (guarded by `omega_core`'s `ordinals_match_solidity_enum_order` test). Field
//      names are Rust snake_case by choice; names never affect encoded bytes.
//   2. Flashloan identity: `providerType` uint8, `providerContract` address (meaningful
//      only for UniswapV3; address(0) for Balancer/AaveV3), `flashloanToken` address.
//      Confirmed byte-for-byte against a real solc golden vector.
//   3. `minNetProfit` = `bp.dynamic_min_profit` (semantic match with the contract's
//      `InsufficientProfit` check and omega_risk check 5).
//   4. `StrategyId -> bytes32`: `registerStrategy` accepts an arbitrary bytes32, so this is
//      deployment configuration, never derived or guessed here. The caller
//      (`main.rs::strategy_onchain_ids()`) supplies it, transcribed from
//      `contracts/src/StrategyIds.sol` (`keccak256("OMEGA_STRATEGY_<X>")`). All five were
//      re-derived with an independent keccak implementation, and
//      `known_strategy_onchain_ids_match_their_documented_keccak256_preimages` re-checks
//      them on every `cargo test`. MANUAL-SYNC RISK: nothing keeps StrategyIds.sol,
//      main.rs and arbitrum.toml in sync automatically. `implementation` / `bytecode_hash`
//      in arbitrum.toml remain TODO placeholders needing a live deployment.
//
// ## Blueprint-authorization signature
//
// `BlueprintSigner::sign_raw_hash()` (no prefix, 65-byte r||s||v with v = recid + 27) is
// the primitive `OmegaOrchestrator.sol`'s `bpHash.recover(sig)` expects.
// `BlueprintSigner::sign()` applies an EIP-191 prefix meant for the Flashbots header and is
// NOT usable here. `blueprint_signer` is a SEPARATE dependency from `tx_key_manager`: the
// gas-paying key and the blueprint-authorization key are independent concerns.
//
// ## BUG FIXED (2026-08-24): abi_encode() vs abi_encode_params()
//
// `Blueprint::abi_encode()` carried one extra leading 0x20 offset word versus the solc
// golden vector. `abi_encode()` encodes a struct as ONE dynamic value (leading offset);
// `abi.decode(blueprintCalldata, (a, b, c, ...))` is the flat-tuple form, which is
// `abi_encode_params()`. Fixed in `build_blueprint_calldata`, in `compute_bp_hash`
// (`DomainSeparatedBlueprint`, by the same reasoning) and in the round-trip tests
// (`abi_decode_params`). `compute_bp_hash_matches_solc_golden_vector` is confirmed against
// a real `forge test` run, so both halves have a real solc/EVM oracle.
//
// ## Envelope fee formula — PROPOSED, PENDING SIGN-OFF
//
// See `docs/fee-policy.md`; its Sign-Off table is not completed. Any claim that this is
// "approved" is only as good as that table being filled in by the fee-policy owner
// through a durable channel (e.g. a signed commit); a chat message is not verification.
//   max_priority_fee_per_gas = priority_fee_gwei * 1e9
//   max_fee_per_gas          = (base_fee_at_creation + 2 * priority_fee_gwei) * 1e9
// NOTE: docs/fee-policy.md §3 argued the 2x belongs on the base term (`2*base + priority`).
// This keeps `base + 2*priority` because that was the specified formula, not because the
// judgment call was resolved. Whoever signs off should confirm the formula, not just caps.
// Fail-closed caps (checked BEFORE any key material is touched):
//   priority_fee_gwei <= MAX_PRIORITY_FEE_GWEI_CAP (500)
//   base_fee_at_creation + 2 * priority_fee_gwei <= MAX_FEE_GWEI_CAP (1500)
// Chain scope, refresh-at-sign-time (§5) and the profit-vs-gas-cost guard (§7 item 3)
// remain open and are not implemented.
//
// `omega_simulation::SimulationSubmitter` signs real transactions only against a local
// Anvil fork and is structurally walled off from live transport; it is not a substitute
// for this type. `ExecutionPipeline` is generic over `S: TransactionSigner` so every stage
// before signing stays fully testable independent of this file.
//
// ## RLP encoder verification
//
// `encode_eip1559_signed_matches_a_real_mainnet_transaction` reproduces the raw bytes of a
// real, mined Ethereum mainnet transaction (decoded independently with Python's `rlp`).
// `encode_eip1559_long_form_rlp_paths_match_independent_implementation` covers the
// long-form length-prefix branches (>= 56 bytes) against Python's `rlp`. The function
// bodies were verified by extracting them into a standalone program, because the crate
// itself could not be compiled in that sandbox.
//
// ## Lint notes
//
// - `#[allow(clippy::too_many_arguments)]` on `encode_eip1559_*`, `sign_call` and
//   `sign_call_gwei`: their argument lists are dictated 1:1 by EIP-1559's field order, and
//   a params struct would obscure that correspondence.
// - Tests bind `ref detail` in `matches!` patterns so `err` is not partially moved (E0382)
//   before `{err:?}` is formatted.
// - `tests::sample_bp` sets `flashloan_provider_type`, `provider_contract` and
//   `flashloan_token` to real, meaningful values because `build_blueprint_calldata` reads them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolValue};
use async_trait::async_trait;
use omega_core::types::blueprint::ExecutionBlueprint;
use omega_security::key_manager::KeyManager;
use omega_security::signer::BlueprintSigner;
use secp256k1::{Message, Secp256k1};

use crate::error::ExecutionError;

// ── ABI types — transcribed directly from OmegaOrchestrator.sol's real source ──
//
// See the header, item 1, for the encoding-equivalence argument (struct-of-fields ==
// tuple-of-same-fields in Solidity ABI encoding; names do not affect encoded bytes).
sol! {
    /// Mirrors OmegaOrchestrator.sol's `blueprintCalldata` layout:
    ///   abi.encode(uint64, uint64, bytes32, FlashloanProviderType,
    ///              address, address, bytes, uint256, uint256, uint256)
    /// in EXACTLY that field order — transcribed from that file's real
    /// `execute()` decode statement, not from its prose "Blueprint
    /// layout" comment alone.
    #[derive(Debug)]
    struct Blueprint {
        uint64 expiry_block;
        uint64 nonce;
        bytes32 strategy_id;
        uint8 provider_type;
        address flashloan_token;
        address provider_contract;
        bytes strategy_calldata;
        uint256 flashloan_amount;
        uint256 min_net_profit;
        uint256 max_base_fee;
    }

    /// Mirrors `keccak256(abi.encode(address(this), EXPECTED_CHAIN_ID,
    /// blueprintCalldata))` — the domain-separated hash
    /// OmegaOrchestrator.execute() actually signs/replay-tracks against
    /// (see that file's change #3 note), NOT a hash of blueprintCalldata
    /// alone.
    #[derive(Debug)]
    struct DomainSeparatedBlueprint {
        address orchestrator;
        uint64 chain_id;
        bytes blueprint_calldata;
    }

    /// The outer transaction target. Parameter names here do not affect
    /// the 4-byte selector (computed from `execute(bytes,bytes)` — types
    /// only), so they're plain snake_case rather than copied from the
    /// contract's `blueprintCalldata`/`sig`.
    function execute(bytes blueprint_calldata, bytes sig) external;
}

/// A fully-signed, RLP-encoded transaction ready for
/// `eth_sendBundle`/`eth_sendRawTransaction`, hex-encoded with a `0x` prefix.
#[derive(Debug, Clone)]
pub struct SignedTransaction {
    pub raw_tx_hex: String,
}

/// Produces a signed transaction from an `ExecutionBlueprint`. See this
/// module's doc comment for exactly what is and isn't confirmed as of
/// this revision.
#[async_trait]
pub trait TransactionSigner: Send + Sync {
    async fn sign_transaction(
        &self,
        bp: &ExecutionBlueprint,
        chain_id: u64,
    ) -> Result<SignedTransaction, ExecutionError>;
}

/// Always-fails signer — the honest default when no `TransactionSigner` at
/// all has been wired in. `ExecutionPipeline` can be constructed and every
/// stage BEFORE signing (integrity, kill switch, 15 checks, idempotency)
/// still runs and is still fully testable with this signer installed; it
/// never silently pretends transaction signing works. Returns
/// `ExecutionError::NoTransactionSigner` every time — never partial
/// success, never a fabricated signature.
pub struct UnconfiguredSigner;

#[async_trait]
impl TransactionSigner for UnconfiguredSigner {
    async fn sign_transaction(
        &self,
        _bp: &ExecutionBlueprint,
        _chain_id: u64,
    ) -> Result<SignedTransaction, ExecutionError> {
        Err(ExecutionError::NoTransactionSigner)
    }
}

// ── Fee policy (proposed, pending sign-off — see module doc) ────────────────

/// Hard ceiling on `priority_fee_gwei`.
///
/// Raised from the original proposed 50 → 500 (2026-10-10 residual closure)
/// so that strategy / gas-war blueprints that already emit competitive tips
/// (observed live: 500 gwei) are not rejected by a still-unsigned policy
/// floor. Fail-closed behaviour is unchanged: values above this constant
/// still return `SigningFailed` and are never clamped.
/// See `docs/fee-policy.md` and the module-level fee-policy note.
const MAX_PRIORITY_FEE_GWEI_CAP: u64 = 500;

/// Hard ceiling on `base_fee_at_creation + 2 * priority_fee_gwei`
/// (i.e. `max_fee_per_gas` expressed in gwei). Raised in lock-step with
/// the priority cap (500 → 1500) so a tip at the new ceiling still has
/// headroom for a non-zero base fee. Fail-closed, never clamped.
const MAX_FEE_GWEI_CAP: u64 = 1_500;

const GWEI_TO_WEI: u64 = 1_000_000_000;

/// Compute the EIP-1559 envelope fees (`max_priority_fee_per_gas`,
/// `max_fee_per_gas`), in wei, per the proposed policy in this module's
/// top doc comment. Fails closed — before any key material is touched —
/// if either input would produce a fee above its cap, naming which cap
/// and by how much, rather than silently clamping (the
/// `saturating_*` arithmetic below is intentionally NOT used to clamp
/// out-of-policy inputs to something signable; it's used only to avoid a
/// panic on the in-policy path, which by construction of the caps cannot
/// overflow `u64`/`U256` in any case that reaches it).
///
/// Returns `(max_priority_fee_per_gas_wei, max_fee_per_gas_wei)`.
fn envelope_fees_wei(
    base_fee_at_creation_gwei: u64,
    priority_fee_gwei: u64,
) -> Result<(U256, U256), ExecutionError> {
    if priority_fee_gwei > MAX_PRIORITY_FEE_GWEI_CAP {
        return Err(ExecutionError::SigningFailed {
            detail: format!(
                "priority_fee_gwei {priority_fee_gwei} exceeds policy cap \
                 {MAX_PRIORITY_FEE_GWEI_CAP} (docs/fee-policy.md, proposed 2026-08-24, \
                 pending sign-off)"
            ),
        });
    }

    let max_fee_gwei =
        base_fee_at_creation_gwei.saturating_add(priority_fee_gwei.saturating_mul(2));
    if max_fee_gwei > MAX_FEE_GWEI_CAP {
        return Err(ExecutionError::SigningFailed {
            detail: format!(
                "max_fee_gwei {max_fee_gwei} (base {base_fee_at_creation_gwei} + \
                 2×priority {priority_fee_gwei}) exceeds policy cap {MAX_FEE_GWEI_CAP} \
                 (docs/fee-policy.md, proposed 2026-08-24, pending sign-off)"
            ),
        });
    }

    let max_priority_fee_per_gas =
        U256::from(priority_fee_gwei).saturating_mul(U256::from(GWEI_TO_WEI));
    let max_fee_per_gas = U256::from(max_fee_gwei).saturating_mul(U256::from(GWEI_TO_WEI));
    Ok((max_priority_fee_per_gas, max_fee_per_gas))
}

// ── Partial production implementation ───────────────────────────────────────

/// Transaction-envelope signer backed by `omega_security::KeyManager`.
///
/// Deliberately a DIFFERENT key manager instance than the one behind
/// `omega_security::BlueprintSigner` may use — the transaction's `from`
/// (gas-paying, funded EOA) and the blueprint's authorizing
/// `execution_key`/`pending_key` are two independent concerns as far as
/// `OmegaOrchestrator.execute()` is concerned: the contract checks `sig`
/// against `execution_key`, not against `msg.sender` / the tx signer. A
/// relayer pattern where a separate hot wallet pays gas is legitimate and
/// this type does not assume otherwise.
///
/// See this module's top doc comment for exactly what this type can and
/// cannot do as of this revision.
pub struct KeyManagerTransactionSigner {
    /// Signs the OUTER transaction envelope (pays gas). NOT necessarily the
    /// same key as the blueprint's on-chain execution_key.
    tx_key_manager: Arc<KeyManager>,
    /// The deployed `OmegaOrchestrator` address — every transaction this
    /// signer produces calls into this contract's `execute()`. Must come
    /// from real deployment configuration; this type does not default it
    /// or accept the zero address.
    orchestrator: Address,
    /// Real, deployment-sourced mapping from `StrategyId::to_string()`
    /// (`"SA"`, `"MSA"`, `"LA"`, `"MEV"`, `"CNRY"`) to the exact `bytes32`
    /// value passed as `strategyId` when that strategy was registered
    /// on-chain via `OmegaOrchestrator.registerStrategy()`. See this
    /// file's top doc comment, item 4, for why this cannot be derived and
    /// must be supplied as real configuration. A strategy absent from
    /// this map fails loudly and specifically at signing time rather
    /// than falling back to a placeholder bytes32.
    strategy_onchain_ids: HashMap<String, [u8; 32]>,
    /// Signs the blueprint-AUTHORIZATION hash (`bpHash`) — the on-chain
    /// signature `OmegaOrchestrator.sol`'s `_acceptsKey()` checks against
    /// `execution_key`/`pending_key`. Deliberately a SEPARATE dependency
    /// from `tx_key_manager` — see this struct's top doc comment for why
    /// the two are independent concerns. Uses `BlueprintSigner::
    /// sign_raw_hash()` specifically, never `BlueprintSigner::sign()` —
    /// see this file's top doc comment for why the latter is confirmed
    /// incompatible with this contract.
    blueprint_signer: Arc<BlueprintSigner>,
    /// Next nonce for the OUTER gas-paying envelope of `tx_key_manager`'s
    /// account. ONE counter per signing account, shared by every strategy
    /// (SA, MSA, MEV, ...) and by `next_tx_nonce()` (ZK `submitProof` path).
    /// Independent of `ExecutionBlueprint::nonce`, which is the
    /// Orchestrator's per-strategy replay counter and lives only inside
    /// the blueprint calldata. `None` until seeded via `seed_tx_nonce`
    /// from `eth_getTransactionCount(active_address, "pending")`. See this
    /// file's top doc comment, "ENVELOPE NONCE FIX (2026-10-09)".
    tx_nonce: Mutex<Option<u64>>,
    secp: Secp256k1<secp256k1::All>,
}

impl KeyManagerTransactionSigner {
    /// # Panics
    /// Panics if `orchestrator == Address::ZERO` — a zero target is never a
    /// valid deployment configuration and should fail at construction time,
    /// not on the first signing attempt under load.
    pub fn new(
        tx_key_manager: Arc<KeyManager>,
        orchestrator: Address,
        strategy_onchain_ids: HashMap<String, [u8; 32]>,
        blueprint_signer: Arc<BlueprintSigner>,
    ) -> Self {
        assert_ne!(
            orchestrator,
            Address::ZERO,
            "KeyManagerTransactionSigner requires a real deployed OmegaOrchestrator address, \
             not the zero address"
        );
        Self {
            tx_key_manager,
            orchestrator,
            strategy_onchain_ids,
            blueprint_signer,
            tx_nonce: Mutex::new(None),
            secp: Secp256k1::new(),
        }
    }

    /// The address that will appear as `from` on every transaction this
    /// signer produces. Useful for pre-funding checks / logging without
    /// exposing key material, and the address whose pending transaction
    /// count must be passed to `seed_tx_nonce`.
    pub fn active_address(&self) -> [u8; 20] {
        self.tx_key_manager.active_address()
    }

    /// Seed or resync the envelope-nonce counter.
    ///
    /// `pending_count` MUST be `eth_getTransactionCount(active_address(), "pending")`.
    /// Call once at startup, and again after any send failure (rejected,
    /// dropped, or never broadcast) so the local counter cannot run ahead of
    /// the chain and strand later transactions behind a nonce gap.
    pub fn seed_tx_nonce(&self, pending_count: u64) {
        *self.tx_nonce.lock().unwrap_or_else(|p| p.into_inner()) = Some(pending_count);
    }

    /// Reserve and return the next envelope nonce, advancing the shared
    /// counter. Use this for `sign_call` / `sign_call_gwei` callers (ZK
    /// `submitProof`) so they draw from the same counter as
    /// `sign_transaction` and cannot collide with it. Fails closed if the
    /// counter has not been seeded.
    pub fn next_tx_nonce(&self) -> Result<u64, ExecutionError> {
        let mut guard = self.tx_nonce.lock().unwrap_or_else(|p| p.into_inner());
        let n = guard.ok_or_else(|| ExecutionError::SigningFailed {
            detail: "envelope nonce not seeded — call seed_tx_nonce(eth_getTransactionCount(\
                     active_address, \"pending\")) at startup"
                .into(),
        })?;
        *guard = Some(n.saturating_add(1));
        Ok(n)
    }

    /// Current value of the envelope-nonce counter without advancing it.
    /// `None` if not yet seeded. Intended for logging / diagnostics.
    pub fn peek_tx_nonce(&self) -> Option<u64> {
        *self.tx_nonce.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The Orchestrator's domain-separated `bpHash` for `bp` — i.e.
    /// `keccak256(abi.encode(orchestrator, chain_id, blueprintCalldata))`, the exact key
    /// `OmegaVault` stores `pending_profit` and the ZK public-inputs binding under (the
    /// Orchestrator passes its own `bpHash` to `receivePendingProfit`).
    ///
    /// Pure: builds the blueprint calldata and hashes it. No signing, no key material.
    /// Fails only if `bp.strategy_id` has no entry in `strategy_onchain_ids`.
    pub fn orchestrator_bp_hash(
        &self,
        bp: &ExecutionBlueprint,
        chain_id: u64,
    ) -> Result<[u8; 32], ExecutionError> {
        let blueprint_calldata = self.build_blueprint_calldata(bp)?;
        Ok(self.compute_bp_hash(&blueprint_calldata, chain_id))
    }

    /// Sign an EIP-1559 transaction to an arbitrary `to` with arbitrary calldata.
    ///
    /// Closes the ZK gap: `OmegaVault.submitProof` is not `Orchestrator.execute` —
    /// this is the gas-paying envelope for verified proof broadcast. Fail closed if
    /// no active tx key or empty calldata / zero `to`.
    ///
    /// The caller supplies `nonce`. To avoid colliding with `sign_transaction`
    /// (same signing account), obtain it from `next_tx_nonce()`.
    ///
    /// `#[allow(clippy::too_many_arguments)]`: see this file's header, "Lint
    /// notes" — this argument list is the minimum EIP-1559 itself requires to
    /// build an arbitrary-call envelope, matching the precedent already set by
    /// `encode_eip1559_unsigned`/`encode_eip1559_signed` below.
    #[allow(clippy::too_many_arguments)]
    pub fn sign_call(
        &self,
        chain_id: u64,
        nonce: u64,
        to: [u8; 20],
        data: &[u8],
        gas_limit: u64,
        max_priority_fee_per_gas: U256,
        max_fee_per_gas: U256,
    ) -> Result<SignedTransaction, ExecutionError> {
        let to = Address::from(to);
        if to == Address::ZERO {
            return Err(ExecutionError::SigningFailed {
                detail: "sign_call: to address must not be zero (C6/ZK fail closed)".into(),
            });
        }
        if data.is_empty() {
            return Err(ExecutionError::SigningFailed {
                detail: "sign_call: calldata must not be empty (C6/ZK fail closed)".into(),
            });
        }
        if chain_id == 0 {
            return Err(ExecutionError::SigningFailed {
                detail: "sign_call: chain_id must be non-zero (C6/ZK fail closed)".into(),
            });
        }
        if gas_limit == 0 {
            return Err(ExecutionError::SigningFailed {
                detail: "sign_call: gas_limit must be non-zero (C6/ZK fail closed)".into(),
            });
        }

        let secret_key =
            self.tx_key_manager
                .active_secret_key()
                .ok_or(ExecutionError::SigningFailed {
                    detail: "tx_key_manager has no active signing key".into(),
                })?;

        let unsigned_rlp = encode_eip1559_unsigned(
            chain_id,
            nonce,
            max_priority_fee_per_gas,
            max_fee_per_gas,
            gas_limit,
            to,
            U256::ZERO,
            data,
        );

        let digest = omega_security::keccak256(&unsigned_rlp);
        let msg =
            Message::from_digest_slice(&digest).map_err(|e| ExecutionError::SigningFailed {
                detail: format!("invalid signing digest: {e}"),
            })?;

        let (recovery_id, compact) = self
            .secp
            .sign_ecdsa_recoverable(&msg, &secret_key)
            .serialize_compact();
        let y_parity = recovery_id.to_i32() as u8;

        let signed_rlp = encode_eip1559_signed(
            chain_id,
            nonce,
            max_priority_fee_per_gas,
            max_fee_per_gas,
            gas_limit,
            to,
            U256::ZERO,
            data,
            y_parity,
            &compact[..32],
            &compact[32..],
        );

        Ok(SignedTransaction {
            raw_tx_hex: format!("0x{}", hex::encode(&signed_rlp)),
        })
    }

    /// Convenience wrapper: fees in gwei (avoids pulling U256 into binary crates).
    ///
    /// `#[allow(clippy::too_many_arguments)]`: same reasoning as
    /// `sign_call` above — this wrapper mirrors that method's argument
    /// list one-for-one, substituting `u64` gwei values for the two
    /// `U256` wei fee fields.
    #[allow(clippy::too_many_arguments)]
    pub fn sign_call_gwei(
        &self,
        chain_id: u64,
        nonce: u64,
        to: [u8; 20],
        data: &[u8],
        gas_limit: u64,
        priority_fee_gwei: u64,
        max_fee_gwei: u64,
    ) -> Result<SignedTransaction, ExecutionError> {
        let gwei = U256::from(1_000_000_000u64);
        self.sign_call(
            chain_id,
            nonce,
            to,
            data,
            gas_limit,
            U256::from(priority_fee_gwei).saturating_mul(gwei),
            U256::from(max_fee_gwei).saturating_mul(gwei),
        )
    }

    /// C6 fail-closed pre-flight before any key material is used.
    fn validate_blueprint_for_signing(
        &self,
        bp: &ExecutionBlueprint,
        chain_id: u64,
    ) -> Result<[u8; 32], ExecutionError> {
        if chain_id == 0 {
            return Err(ExecutionError::SigningFailed {
                detail: "chain_id must be non-zero (C6 fail closed)".into(),
            });
        }
        if bp.total_l2_gas_budget() == 0 {
            return Err(ExecutionError::SigningFailed {
                detail: "total_l2_gas_budget must be non-zero (C6 fail closed)".into(),
            });
        }
        self.resolve_strategy_id(bp)
    }

    /// Resolve on-chain strategyId for `bp`, or fail closed (missing / all-zero).
    fn resolve_strategy_id(&self, bp: &ExecutionBlueprint) -> Result<[u8; 32], ExecutionError> {
        let strategy_key = bp.strategy_id.to_string();
        let strategy_id_bytes: [u8; 32] = *self
            .strategy_onchain_ids
            .get(&strategy_key)
            .ok_or_else(|| ExecutionError::SigningFailed {
                detail: format!(
                    "no on-chain strategyId configured for strategy_id {strategy_key:?} — this \
                     value is real deployment configuration (whatever bytes32 was passed to \
                     OmegaOrchestrator.registerStrategy() for this strategy on-chain), not \
                     something derivable from code. Wire it into \
                     KeyManagerTransactionSigner::new's strategy_onchain_ids map, sourced from \
                     real deployment records, never guessed at."
                ),
            })?;
        if strategy_id_bytes == [0u8; 32] {
            return Err(ExecutionError::SigningFailed {
                detail: format!(
                    "on-chain strategyId for {strategy_key:?} is all-zero — refusing to sign \
                     (C6 fail closed)"
                ),
            });
        }
        Ok(strategy_id_bytes)
    }

    /// Build the real, ABI-encoded `blueprintCalldata` bytes for `bp` —
    /// see this file's top doc comment, items 1-4, for exactly what's
    /// confirmed here and against what evidence.
    ///
    /// The ONLY failure mode is a strategy missing from
    /// `strategy_onchain_ids` — everything else is a pure, infallible
    /// transformation of already-real `ExecutionBlueprint` fields.
    ///
    /// NOTE: `bp.nonce` is encoded HERE, inside the blueprint calldata, as the
    /// Orchestrator's per-strategy replay counter. It is NOT the envelope nonce.
    fn build_blueprint_calldata(&self, bp: &ExecutionBlueprint) -> Result<Vec<u8>, ExecutionError> {
        let strategy_id_bytes = self.resolve_strategy_id(bp)?;

        // Solidity ABI-encodes an enum identically to its `uint8` ordinal.
        // `omega_core::types::flashloan_provider`'s own
        // `ordinals_match_solidity_enum_order` test guarantees this cast
        // matches OmegaOrchestrator.sol's `FlashloanProviderType` enum
        // order — not an assumption made fresh here.
        let provider_type: u8 = bp.flashloan_provider_type as u8;

        // OmegaOrchestrator.sol's `maxBaseFee` is compared directly against
        // `block.basefee`, which the EVM always reports in WEI.
        // `ExecutionBlueprint::max_base_fee_gwei` is explicitly
        // gwei-denominated (see its own name and the
        // `derive_max_base_fee_gwei` helper) — converted here, not
        // silently mismatched.
        let max_base_fee_wei =
            U256::from(bp.max_base_fee_gwei).saturating_mul(U256::from(1_000_000_000u64));

        let blueprint = Blueprint {
            expiry_block: bp.expiry_block,
            nonce: bp.nonce,
            strategy_id: strategy_id_bytes.into(),
            provider_type,
            flashloan_token: bp.flashloan_token,
            provider_contract: bp.provider_contract,
            strategy_calldata: bp.calldata.clone(),
            flashloan_amount: bp.flashloan_amount,
            min_net_profit: bp.dynamic_min_profit,
            max_base_fee: max_base_fee_wei,
        };

        // .abi_encode_params(), NOT .abi_encode() — see this file's top
        // doc comment, "BUG FIXED (2026-08-24)", for why the latter
        // silently produces the wrong bytes (an extra leading offset
        // word) for a struct being decoded as a flat multi-field tuple,
        // which is exactly what OmegaOrchestrator.sol's
        // `abi.decode(blueprintCalldata, (...))` does.
        Ok(blueprint.abi_encode_params())
    }

    /// Compute the real domain-separated `bpHash` OmegaOrchestrator.sol
    /// actually signs/replay-tracks against — see this file's top doc
    /// comment for why this is NOT simply `keccak256(blueprint_calldata)`.
    fn compute_bp_hash(&self, blueprint_calldata: &[u8], chain_id: u64) -> [u8; 32] {
        let domain = DomainSeparatedBlueprint {
            orchestrator: self.orchestrator,
            chain_id,
            blueprint_calldata: blueprint_calldata.to_vec().into(),
        };
        // .abi_encode_params(), NOT .abi_encode() — same fix, same reason
        // as `build_blueprint_calldata` above. The contract's own domain
        // hash is `keccak256(abi.encode(address(this), EXPECTED_CHAIN_ID,
        // blueprintCalldata))` — three top-level params, not a struct — so
        // this must be the flat-tuple encoding, not the
        // single-dynamic-value encoding `.abi_encode()` produces.
        omega_security::keccak256(&domain.abi_encode_params())
    }

    /// Build the ABI-encoded outer calldata for
    /// `OmegaOrchestrator.execute(bytes blueprintCalldata, bytes sig)`,
    /// including the real on-chain-authorization signature.
    ///
    /// Fully real end-to-end: `blueprintCalldata` and `bp_hash` are built
    /// and hashed for real (see `build_blueprint_calldata` /
    /// `compute_bp_hash` above), `sig` is produced by
    /// `BlueprintSigner::sign_raw_hash()` — the primitive confirmed
    /// compatible with `OmegaOrchestrator.sol`'s `bpHash.recover(sig)` —
    /// and the three are assembled via `encode_execute_call`. The only
    /// remaining failure modes are a strategy missing from
    /// `strategy_onchain_ids` (real deployment configuration this file
    /// cannot supply for itself) or a signing failure surfaced from
    /// `BlueprintSigner` itself (e.g. no active key) — both fail loudly
    /// and specifically, never silently.
    fn build_execute_calldata(
        &self,
        bp: &ExecutionBlueprint,
        chain_id: u64,
    ) -> Result<Bytes, ExecutionError> {
        let blueprint_calldata = self.build_blueprint_calldata(bp)?;
        let bp_hash = self.compute_bp_hash(&blueprint_calldata, chain_id);

        let signed_bundle = self.blueprint_signer.sign_raw_hash(&bp_hash).map_err(|e| {
            ExecutionError::SigningFailed {
                detail: format!("blueprint-authorization signing failed: {e}"),
            }
        })?;

        let sig_bytes = signed_bundle.signature.bytes.to_vec();
        let calldata = encode_execute_call(blueprint_calldata, sig_bytes);

        tracing::debug!(
            blueprint_hash = %bp.blueprint_hash,
            bp_hash = %hex::encode(bp_hash),
            authorized_by = %hex::encode(signed_bundle.signer_address),
            "execute() calldata built with real blueprint-authorization signature"
        );

        Ok(calldata.into())
    }
}

#[async_trait]
impl TransactionSigner for KeyManagerTransactionSigner {
    async fn sign_transaction(
        &self,
        bp: &ExecutionBlueprint,
        chain_id: u64,
    ) -> Result<SignedTransaction, ExecutionError> {
        // C6: fail closed before any EIP-1559 RLP or secp256k1 work.
        let _ = self.validate_blueprint_for_signing(bp, chain_id)?;

        let data = self.build_execute_calldata(bp, chain_id)?;

        let secret_key =
            self.tx_key_manager
                .active_secret_key()
                .ok_or(ExecutionError::SigningFailed {
                    detail: "tx_key_manager has no active signing key".into(),
                })?;

        let gas_limit = bp.total_l2_gas_budget();

        // Fee policy (proposed, pending sign-off) — see the header and
        // `docs/fee-policy.md`. Fails closed, before any RLP is built, if
        // either input is out of policy.
        let (max_priority_fee_per_gas, max_fee_per_gas) =
            envelope_fees_wei(bp.base_fee_at_creation, bp.priority_fee_gwei)?;

        // Envelope nonce — see the header, "2026-10-09: ENVELOPE NONCE FIX".
        // The guard is held through signing so concurrent strategies (SA, MSA,
        // ...) are serialized and can never reuse a nonce. There is no `.await`
        // in this function after this point, so holding a std Mutex guard here
        // is sound and the future stays `Send`.
        let mut nonce_guard = self.tx_nonce.lock().unwrap_or_else(|p| p.into_inner());
        let envelope_nonce = match *nonce_guard {
            Some(n) => n,
            None => {
                // Legacy fallback: NOT correct on a real chain. The caller must
                // `seed_tx_nonce` at startup. The counter is deliberately left
                // unseeded (and not advanced) in this mode.
                tracing::warn!(
                    blueprint_hash = %bp.blueprint_hash,
                    fallback_nonce = bp.nonce,
                    "envelope nonce NOT seeded — falling back to blueprint nonce; this will \
                     collide across strategies and mismatch the account nonce. Call \
                     KeyManagerTransactionSigner::seed_tx_nonce at startup."
                );
                bp.nonce
            }
        };

        let unsigned_rlp = encode_eip1559_unsigned(
            chain_id,
            envelope_nonce,
            max_priority_fee_per_gas,
            max_fee_per_gas,
            gas_limit,
            self.orchestrator,
            U256::ZERO,
            &data,
        );

        let digest = omega_security::keccak256(&unsigned_rlp);
        let msg =
            Message::from_digest_slice(&digest).map_err(|e| ExecutionError::SigningFailed {
                detail: format!("invalid signing digest: {e}"),
            })?;

        // libsecp256k1's ECDSA signing (used here via the `secp256k1`
        // crate's `sign_ecdsa_recoverable`) already produces canonical,
        // low-s signatures by construction — the same low-s requirement
        // Ethereum enforces for every transaction type since Homestead
        // (EIP-2). No extra normalization step is needed here.
        let (recovery_id, compact) = self
            .secp
            .sign_ecdsa_recoverable(&msg, &secret_key)
            .serialize_compact();
        let y_parity = recovery_id.to_i32() as u8;

        let signed_rlp = encode_eip1559_signed(
            chain_id,
            envelope_nonce,
            max_priority_fee_per_gas,
            max_fee_per_gas,
            gas_limit,
            self.orchestrator,
            U256::ZERO,
            &data,
            y_parity,
            &compact[..32],
            &compact[32..],
        );

        // Advance the shared counter only after a successful signature, and only
        // when it is seeded (see the unseeded fallback above).
        if nonce_guard.is_some() {
            *nonce_guard = Some(envelope_nonce.saturating_add(1));
        }
        drop(nonce_guard);

        let raw_tx_hex = format!("0x{}", hex::encode(&signed_rlp));

        tracing::debug!(
            blueprint_hash = %bp.blueprint_hash,
            chain_id,
            blueprint_nonce = bp.nonce,
            envelope_nonce,
            gas_limit,
            "transaction signed"
        );

        Ok(SignedTransaction { raw_tx_hex })
    }
}

/// Assembles the FINAL outer `execute(bytes,bytes)` calldata from an
/// already-built `blueprint_calldata` and an already-produced
/// authorization `sig`. Free function (not a method) so it's directly
/// unit-testable without constructing a full `KeyManagerTransactionSigner`
/// — see the tests below. Genuinely called from `build_execute_calldata`
/// above — kept as a free function regardless, since that keeps the
/// "pure assembly" step independently testable from the signing step that
/// produces its `sig` input.
pub(crate) fn encode_execute_call(blueprint_calldata: Vec<u8>, sig: Vec<u8>) -> Vec<u8> {
    executeCall {
        blueprint_calldata: blueprint_calldata.into(),
        sig: sig.into(),
    }
    .abi_encode()
}

// ── EIP-1559 RLP encoding helpers ───────────────────────────────────────────
//
// Field order and signing-digest construction checked directly against
// EIP-1559's specification text (eips.ethereum.org/EIPS/eip-1559):
//
//   unsigned: 0x02 || rlp([chain_id, nonce, max_priority_fee_per_gas,
//             max_fee_per_gas, gas_limit, destination, amount, data,
//             access_list])
//   signing digest: keccak256(unsigned bytes above)
//   signed:   0x02 || rlp([..same 9 fields.., signature_y_parity,
//             signature_r, signature_s])
//
// See the header, "RLP encoder verification", for the golden-vector evidence.
// Self-contained rather than pulling in alloy's consensus/signer feature set,
// since this encoder needs only RLP.
//
// Both `encode_eip1559_unsigned` and `encode_eip1559_signed` carry
// `#[allow(clippy::too_many_arguments)]` — their argument lists are
// dictated 1:1 by EIP-1559's own field order.

#[allow(clippy::too_many_arguments)]
fn encode_eip1559_unsigned(
    chain_id: u64,
    nonce: u64,
    max_priority_fee: U256,
    max_fee: U256,
    gas_limit: u64,
    to: Address,
    value: U256,
    data: &[u8],
) -> Vec<u8> {
    let mut payload = Vec::new();
    let list = rlp_list(&[
        rlp_u64(chain_id),
        rlp_u64(nonce),
        rlp_u256(max_priority_fee),
        rlp_u256(max_fee),
        rlp_u64(gas_limit),
        rlp_address(to),
        rlp_u256(value),
        rlp_bytes(data),
        rlp_list(&[]), // empty access list
    ]);
    payload.push(0x02);
    payload.extend_from_slice(&list);
    payload
}

#[allow(clippy::too_many_arguments)]
fn encode_eip1559_signed(
    chain_id: u64,
    nonce: u64,
    max_priority_fee: U256,
    max_fee: U256,
    gas_limit: u64,
    to: Address,
    value: U256,
    data: &[u8],
    y_parity: u8,
    r: &[u8],
    s: &[u8],
) -> Vec<u8> {
    let mut payload = Vec::new();
    let list = rlp_list(&[
        rlp_u64(chain_id),
        rlp_u64(nonce),
        rlp_u256(max_priority_fee),
        rlp_u256(max_fee),
        rlp_u64(gas_limit),
        rlp_address(to),
        rlp_u256(value),
        rlp_bytes(data),
        rlp_list(&[]), // access list
        rlp_u64(y_parity as u64),
        rlp_scalar(r),
        rlp_scalar(s),
    ]);
    payload.push(0x02);
    payload.extend_from_slice(&list);
    payload
}

fn rlp_u64(v: u64) -> Vec<u8> {
    if v == 0 {
        return vec![0x80];
    }
    let bytes = v.to_be_bytes();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(7);
    rlp_bytes(&bytes[start..])
}

fn rlp_u256(v: U256) -> Vec<u8> {
    let bytes = v.to_be_bytes::<32>();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(31);
    if start == 31 && bytes[31] == 0 {
        return vec![0x80];
    }
    rlp_bytes(&bytes[start..])
}

/// RLP-encode a big-endian scalar (ECDSA r/s): leading zero bytes MUST be
/// stripped or nodes reject the transaction as non-canonical.
fn rlp_scalar(b: &[u8]) -> Vec<u8> {
    let start = b.iter().position(|&x| x != 0).unwrap_or(b.len());
    rlp_bytes(&b[start..])
}

fn rlp_address(addr: Address) -> Vec<u8> {
    rlp_bytes(addr.as_slice())
}

fn rlp_bytes(b: &[u8]) -> Vec<u8> {
    if b.len() == 1 && b[0] < 0x80 {
        return b.to_vec();
    }
    if b.len() < 56 {
        let mut out = Vec::with_capacity(1 + b.len());
        out.push(0x80 + b.len() as u8);
        out.extend_from_slice(b);
        out
    } else {
        let len_bytes = (b.len() as u64).to_be_bytes();
        let start = len_bytes.iter().position(|&x| x != 0).unwrap_or(7);
        let mut out = Vec::with_capacity(1 + (8 - start) + b.len());
        out.push(0xb7 + (8 - start) as u8);
        out.extend_from_slice(&len_bytes[start..]);
        out.extend_from_slice(b);
        out
    }
}

fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
    let total: usize = items.iter().map(|i| i.len()).sum();
    let mut out = Vec::new();
    if total < 56 {
        out.push(0xc0 + total as u8);
    } else {
        let len_bytes = (total as u64).to_be_bytes();
        let start = len_bytes.iter().position(|&x| x != 0).unwrap_or(7);
        out.push(0xf7 + (8 - start) as u8);
        out.extend_from_slice(&len_bytes[start..]);
    }
    for item in items {
        out.extend_from_slice(item);
    }
    out
}

/// Test-only fake — compiled only under `cfg(test)`, the same pattern
/// `omega_relay::client::MockRelayClient` already uses for exactly this
/// reason (a fake that's structurally impossible to link into a
/// production binary). Never gate this behind a `test-utils` feature that
/// a production build could accidentally enable.
#[cfg(test)]
pub struct MockTransactionSigner {
    pub should_fail: bool,
}

#[cfg(test)]
#[async_trait]
impl TransactionSigner for MockTransactionSigner {
    async fn sign_transaction(
        &self,
        bp: &ExecutionBlueprint,
        _chain_id: u64,
    ) -> Result<SignedTransaction, ExecutionError> {
        if self.should_fail {
            return Err(ExecutionError::SigningFailed {
                detail: "mock signer configured to fail".into(),
            });
        }
        // Deterministic fake hex derived from the blueprint hash — good
        // enough to exercise the transform/submission stages in tests;
        // never used outside #[cfg(test)].
        Ok(SignedTransaction {
            raw_tx_hex: format!("0x{}", hex::encode(bp.blueprint_hash.as_slice())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omega_core::types::flashloan_provider::FlashloanProviderType;
    use omega_security::key_manager::KeyManager;
    use omega_security::signer::BlueprintSigner;
    use secp256k1::SecretKey;

    fn make_km(byte: u8) -> Arc<KeyManager> {
        Arc::new(KeyManager::from_secret_key(
            SecretKey::from_slice(&[byte; 32]).unwrap(),
        ))
    }

    fn make_blueprint_signer(byte: u8) -> Arc<BlueprintSigner> {
        Arc::new(BlueprintSigner::new(make_km(byte)))
    }

    fn empty_strategy_ids() -> HashMap<String, [u8; 32]> {
        HashMap::new()
    }

    fn strategy_ids_with_sa() -> HashMap<String, [u8; 32]> {
        let mut m = HashMap::new();
        m.insert("SA".to_string(), [0x42u8; 32]);
        m
    }

    #[test]
    fn build_blueprint_calldata_matches_solc_golden_vector() {
        // Fixture MUST match contracts/test/BlueprintCalldataAbi.t.sol constants.
        let km = make_km(0x20);
        let mut ids = HashMap::new();
        ids.insert("SA".into(), {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(
                &hex::decode("c4bb1c851b1c74593f61f8d1f99ec07e2960d847a94d4a736e321ba387d4d2d7")
                    .expect("valid strategy id hex"),
            );
            arr
        });
        let signer = KeyManagerTransactionSigner::new(
            km,
            Address::from([0x01; 20]),
            ids,
            make_blueprint_signer(0x21),
        );

        let mut bp = sample_bp();
        // Override anything sample_bp does not pin to the Foundry fixture:
        bp.expiry_block = 1_100;
        bp.nonce = 0;
        bp.flashloan_provider_type = FlashloanProviderType::Balancer;
        bp.provider_contract = Address::ZERO;
        bp.flashloan_token = Address::from([0x99; 20]);
        bp.calldata = Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]);
        bp.flashloan_amount = U256::from(1_000_000u64);
        bp.dynamic_min_profit = U256::from(100_000u64);
        bp.max_base_fee_gwei = 30; // → 30e9 wei in build_blueprint_calldata

        let encoded = signer.build_blueprint_calldata(&bp).unwrap();

        // Golden hex confirmed against `forge test
        // --match-test test_print_golden_blueprint_calldata -vv` in
        // contracts/test/BlueprintCalldataAbi.t.sol — a real compiled
        // solc/EVM encode of the same fields, not a value derived from
        // this file's own encoder.
        let solc_golden = hex::decode(
            "000000000000000000000000000000000000000000000000000000000000044c\
            0000000000000000000000000000000000000000000000000000000000000000\
            c4bb1c851b1c74593f61f8d1f99ec07e2960d847a94d4a736e321ba387d4d2d7\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000009999999999999999999999999999999999999999\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000140\
            00000000000000000000000000000000000000000000000000000000000f4240\
            00000000000000000000000000000000000000000000000000000000000186a0\
            00000000000000000000000000000000000000000000000000000006fc23ac00\
            0000000000000000000000000000000000000000000000000000000000000004\
            deadbeef00000000000000000000000000000000000000000000000000000000",
        )
        .expect("valid golden hex");

        assert_eq!(
            encoded.as_slice(),
            solc_golden.as_slice(),
            "Rust ABI must match solc abi.encode of execute() decode tuple"
        );
    }

    // ── Known strategy onchain_ids — cross-check, not production data ──────
    //
    // See this file's header, item 4, for the full evidence chain: these five
    // values are transcribed here PURELY for a verification cross-check, not as
    // production configuration — the actual production map lives in
    // main.rs's `strategy_onchain_ids()`, sourced from
    // contracts/src/StrategyIds.sol. Never called by KeyManagerTransactionSigner.
    fn known_strategy_onchain_ids() -> HashMap<&'static str, [u8; 32]> {
        fn hash32(hex_str: &str) -> [u8; 32] {
            hex::decode(hex_str)
                .expect("valid hex")
                .try_into()
                .expect("exactly 32 bytes")
        }
        let mut m = HashMap::new();
        // StrategyIds.sol::SIMPLE_ARB — keccak256("OMEGA_STRATEGY_SA"). Same
        // value already locked into build_blueprint_calldata_matches_solc_
        // golden_vector above and cross-checked there against a real solc
        // output.
        m.insert(
            "SA",
            hash32("c4bb1c851b1c74593f61f8d1f99ec07e2960d847a94d4a736e321ba387d4d2d7"),
        );
        // StrategyIds.sol::LIQUIDATION_ARB — keccak256("OMEGA_STRATEGY_LA")
        m.insert(
            "LA",
            hash32("77b0296a1c4dae896ee0ffe05246d8b3e8ecd44a1d4a0c6591b183fb2390a698"),
        );
        // StrategyIds.sol::MULTI_STEP_ARB — keccak256("OMEGA_STRATEGY_MSA")
        m.insert(
            "MSA",
            hash32("bfd7e8e9c54a6762cb6ff399dc8bdefe2226a32400ed6001e1bee533bbaa25d2"),
        );
        // StrategyIds.sol::MEV_OFA — keccak256("OMEGA_STRATEGY_MEV")
        m.insert(
            "MEV",
            hash32("892be743cfc8880f51726a84ab1d0d0fc05336d49927c5a9eaaf926a84db319a"),
        );
        // StrategyIds.sol::CANARY_ARB — keccak256("OMEGA_STRATEGY_CNRY")
        m.insert(
            "CNRY",
            hash32("93879ddf9ec0b01c066594680539ea61eaab23f806b410fda1c18659efcc7725"),
        );
        m
    }

    #[test]
    fn known_strategy_onchain_ids_match_their_documented_keccak256_preimages() {
        // StrategyIds.sol documents each constant with a
        // `/// @dev keccak256("OMEGA_STRATEGY_<X>")` comment. This recomputes
        // that hash independently — using omega_security::keccak256, the SAME
        // primitive compute_bp_hash uses — and confirms each transcribed
        // constant equals what its own source comment claims. This verifies
        // the values against their documented DERIVATION, not just against
        // another transcription of the same numbers.
        let ids = known_strategy_onchain_ids();
        let preimages: &[(&str, &str)] = &[
            ("SA", "OMEGA_STRATEGY_SA"),
            ("LA", "OMEGA_STRATEGY_LA"),
            ("MSA", "OMEGA_STRATEGY_MSA"),
            ("MEV", "OMEGA_STRATEGY_MEV"),
            ("CNRY", "OMEGA_STRATEGY_CNRY"),
        ];
        for (name, preimage) in preimages {
            let computed = omega_security::keccak256(preimage.as_bytes());
            assert_eq!(
                ids[name], computed,
                "{name}'s onchain_id must equal keccak256(\"{preimage}\") — \
                 StrategyIds.sol's own documented derivation for this constant"
            );
        }
    }

    #[test]
    fn known_strategy_onchain_ids_are_internally_consistent() {
        let ids = known_strategy_onchain_ids();

        assert_eq!(ids.len(), 5, "expected exactly the five named strategies");

        // No two strategies may share a bytes32 id — a duplicate here would
        // mean OmegaOrchestrator.execute() could authorize the wrong
        // strategy's blueprint under another strategy's signature.
        let mut seen: Vec<[u8; 32]> = ids.values().copied().collect();
        seen.sort();
        seen.dedup();
        assert_eq!(
            seen.len(),
            5,
            "all five strategy onchain_ids must be pairwise distinct — a \
             duplicate suggests a copy-paste transcription error"
        );

        // None may be all-zero — same fail-closed reasoning
        // parse_onchain_id-style validation applies elsewhere in this
        // workspace (see omega-security::integrity.rs's parse_bytecode_hash/
        // parse_contract_address, which reject all-zero the same way).
        for (name, id) in &ids {
            assert_ne!(*id, [0u8; 32], "{name}'s onchain_id must not be all-zero");
        }

        // SA specifically must match the byte-for-byte solc-confirmed value
        // already locked into build_blueprint_calldata_matches_solc_golden_
        // vector above — the one value in this set with independent solc
        // verification behind it in addition to the keccak256 preimage
        // check above.
        let expected_sa =
            hex::decode("c4bb1c851b1c74593f61f8d1f99ec07e2960d847a94d4a736e321ba387d4d2d7")
                .unwrap();
        assert_eq!(
            ids["SA"].as_slice(),
            expected_sa.as_slice(),
            "SA's onchain_id must match the solc-golden-verified value used elsewhere in this file"
        );
    }

    // ── Construction guard ────────────────────────────────────────────────

    #[test]
    fn sign_call_rejects_zero_to() {
        let signer = KeyManagerTransactionSigner::new(
            make_km(0x31),
            Address::from([0x01; 20]),
            empty_strategy_ids(),
            make_blueprint_signer(0x31),
        );
        let err = signer
            .sign_call(
                42161,
                0,
                [0u8; 20],
                &[0xab; 4],
                100_000,
                U256::from(1_000_000_000u64),
                U256::from(50_000_000_000u64),
            )
            .unwrap_err();
        assert!(matches!(err, ExecutionError::SigningFailed { .. }));
    }

    #[test]
    #[should_panic(expected = "requires a real deployed OmegaOrchestrator address")]
    fn zero_orchestrator_address_panics_at_construction() {
        let km = make_km(0x01);
        let _ = KeyManagerTransactionSigner::new(
            km,
            Address::ZERO,
            empty_strategy_ids(),
            make_blueprint_signer(0x01),
        );
    }

    #[tokio::test]
    async fn sign_transaction_fails_closed_on_zero_chain_id() {
        let signer = KeyManagerTransactionSigner::new(
            make_km(0x21),
            Address::from([0x01; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(0x21),
        );
        let err = signer.sign_transaction(&sample_bp(), 0).await.unwrap_err();
        assert!(
            matches!(err, ExecutionError::SigningFailed { ref detail } if detail.contains("chain_id")),
            "expected chain_id fail-closed, got {err:?}"
        );
    }

    #[test]
    fn resolve_strategy_id_rejects_all_zero_mapping() {
        let mut ids = strategy_ids_with_sa();
        ids.insert("SA".into(), [0u8; 32]);
        let signer = KeyManagerTransactionSigner::new(
            make_km(0x22),
            Address::from([0x01; 20]),
            ids,
            make_blueprint_signer(0x22),
        );
        let err = signer.build_blueprint_calldata(&sample_bp()).unwrap_err();
        assert!(
            matches!(err, ExecutionError::SigningFailed { ref detail } if detail.contains("all-zero")),
            "expected all-zero strategyId fail-closed, got {err:?}"
        );
    }

    #[test]
    fn active_address_matches_key_manager() {
        let km = make_km(0x02);
        let expected = km.active_address();
        let signer = KeyManagerTransactionSigner::new(
            km,
            Address::from([0x01; 20]),
            empty_strategy_ids(),
            make_blueprint_signer(0x02),
        );
        assert_eq!(signer.active_address(), expected);
    }

    // ── sign_transaction end to end ─────────────────────────────────────────

    #[tokio::test]
    async fn sign_transaction_succeeds_end_to_end_with_real_blueprint_signer() {
        // Regression guard: with a real strategy_onchain_ids entry and a
        // real BlueprintSigner, and with sample_bp()'s fees within the
        // caps (base=10, priority=5), signing must actually succeed
        // end-to-end. If this starts failing, either the ABI encoding, the
        // domain hash, the BlueprintSigner wiring, or the fee-cap
        // thresholds regressed.
        let tx_km = make_km(0x03);
        let signer = KeyManagerTransactionSigner::new(
            tx_km,
            Address::from([0x01; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(0x0d),
        );
        signer.seed_tx_nonce(21_287);
        let bp = sample_bp();

        let result = signer.sign_transaction(&bp, 42161).await;
        let signed = result.expect("sign_transaction must succeed with real config wired");
        assert!(
            signed.raw_tx_hex.starts_with("0x02"),
            "must be an EIP-1559 typed tx"
        );
        assert!(
            signed.raw_tx_hex.len() > 4,
            "must contain real RLP payload, not just the type byte"
        );
        assert_eq!(
            signer.peek_tx_nonce(),
            Some(21_288),
            "envelope nonce must advance by exactly one after a successful signature"
        );
    }

    #[test]
    fn build_execute_calldata_succeeds_with_real_blueprint_signer() {
        let tx_km = make_km(0x0e);
        let signer = KeyManagerTransactionSigner::new(
            tx_km,
            Address::from([0x01; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(0x0f),
        );
        let bp = sample_bp();
        let calldata = signer.build_execute_calldata(&bp, 42161).unwrap();
        assert!(
            calldata.starts_with(&executeCall::SELECTOR),
            "outer calldata must start with the real execute(bytes,bytes) selector"
        );
    }

    #[tokio::test]
    async fn sign_transaction_fails_with_strategy_lookup_reason_when_unconfigured() {
        // An unconfigured strategy_onchain_ids entry fails BEFORE bp_hash is
        // ever computed or the BlueprintSigner is ever called, with a message
        // naming the missing strategy specifically.
        let km = make_km(0x04);
        let signer = KeyManagerTransactionSigner::new(
            km,
            Address::from([0x01; 20]),
            empty_strategy_ids(), // SA deliberately absent
            make_blueprint_signer(0x10),
        );
        let bp = sample_bp();

        let result = signer.sign_transaction(&bp, 42161).await;
        match result {
            Err(ExecutionError::SigningFailed { detail }) => {
                assert!(
                    detail.contains("SA"),
                    "error must name the missing strategy, got: {detail}"
                );
                assert!(
                    detail.contains("registerStrategy"),
                    "error must explain this is deployment configuration, got: {detail}"
                );
            }
            other => panic!("expected ExecutionError::SigningFailed, got {other:?}"),
        }
    }

    // ── Envelope nonce (2026-10-09 fix) ─────────────────────────────────────

    fn nonce_test_signer(km_byte: u8, bs_byte: u8) -> KeyManagerTransactionSigner {
        KeyManagerTransactionSigner::new(
            make_km(km_byte),
            Address::from([0x01; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(bs_byte),
        )
    }

    #[tokio::test]
    async fn envelope_nonce_is_independent_of_blueprint_nonce() {
        // Two blueprints with the SAME blueprint nonce (as SA and MSA had in the
        // field) must be signed with DIFFERENT, consecutive envelope nonces drawn
        // from the seeded counter — never from bp.nonce.
        let signer = nonce_test_signer(0x60, 0x61);
        signer.seed_tx_nonce(21_287);

        let mut bp1 = sample_bp();
        bp1.nonce = 22;
        let mut bp2 = sample_bp();
        bp2.nonce = 22;

        let a = signer.sign_transaction(&bp1, 42161).await.unwrap();
        let b = signer.sign_transaction(&bp2, 42161).await.unwrap();

        assert_ne!(
            a.raw_tx_hex, b.raw_tx_hex,
            "two signatures from the same account must carry different envelope nonces"
        );
        assert_eq!(signer.peek_tx_nonce(), Some(21_289));
    }

    #[tokio::test]
    async fn envelope_nonce_in_raw_tx_is_the_seeded_value_not_the_blueprint_nonce() {
        // Decode the nonce back out of the signed RLP: it must be the seeded value.
        let signer = nonce_test_signer(0x62, 0x63);
        signer.seed_tx_nonce(21_287);
        let mut bp = sample_bp();
        bp.nonce = 22;

        let signed = signer.sign_transaction(&bp, 42161).await.unwrap();

        // EIP-1559 field order: [chain_id, nonce, ...]. For chain 42161 (0xa4b1) the
        // RLP list is: 0x02 || list-header || 0x82 0xa4 0xb1 || <rlp(nonce)> ...
        let raw = hex::decode(signed.raw_tx_hex.trim_start_matches("0x")).unwrap();
        let needle_chain = [0x82u8, 0xa4, 0xb1];
        let pos = raw
            .windows(3)
            .position(|w| w == needle_chain)
            .expect("chain id field present");
        let after = &raw[pos + 3..];
        // 21287 = 0x5327 -> RLP 0x82 0x53 0x27
        assert_eq!(&after[..3], &[0x82, 0x53, 0x27]);
    }

    #[tokio::test]
    async fn unseeded_signer_falls_back_to_blueprint_nonce_and_does_not_advance() {
        // Documented legacy fallback: unseeded -> bp.nonce, counter stays None.
        let signer = nonce_test_signer(0x64, 0x65);
        assert_eq!(signer.peek_tx_nonce(), None);
        let signed = signer.sign_transaction(&sample_bp(), 42161).await.unwrap();
        assert!(signed.raw_tx_hex.starts_with("0x02"));
        assert_eq!(
            signer.peek_tx_nonce(),
            None,
            "an unseeded counter must stay unseeded after a fallback signature"
        );
    }

    #[tokio::test]
    async fn failed_signature_does_not_advance_envelope_nonce() {
        // Out-of-policy fee fails before the nonce is touched.
        let signer = nonce_test_signer(0x66, 0x67);
        signer.seed_tx_nonce(100);
        let mut bp = sample_bp();
        bp.priority_fee_gwei = MAX_PRIORITY_FEE_GWEI_CAP + 1;
        assert!(signer.sign_transaction(&bp, 42161).await.is_err());
        assert_eq!(signer.peek_tx_nonce(), Some(100));
    }

    #[test]
    fn next_tx_nonce_fails_closed_when_unseeded() {
        let signer = nonce_test_signer(0x68, 0x69);
        let err = signer.next_tx_nonce().unwrap_err();
        assert!(
            matches!(err, ExecutionError::SigningFailed { ref detail } if detail.contains("not seeded")),
            "expected not-seeded fail-closed, got {err:?}"
        );
    }

    #[test]
    fn next_tx_nonce_returns_current_then_advances() {
        let signer = nonce_test_signer(0x6a, 0x6b);
        signer.seed_tx_nonce(7);
        assert_eq!(signer.next_tx_nonce().unwrap(), 7);
        assert_eq!(signer.next_tx_nonce().unwrap(), 8);
        assert_eq!(signer.peek_tx_nonce(), Some(9));
    }

    #[test]
    fn seed_tx_nonce_resyncs_after_failure() {
        let signer = nonce_test_signer(0x6c, 0x6d);
        signer.seed_tx_nonce(10);
        let _ = signer.next_tx_nonce().unwrap();
        let _ = signer.next_tx_nonce().unwrap();
        // simulate a rejected send: caller re-reads the chain and resyncs
        signer.seed_tx_nonce(10);
        assert_eq!(signer.peek_tx_nonce(), Some(10));
    }

    #[tokio::test]
    async fn shared_counter_across_sign_transaction_and_next_tx_nonce() {
        // The ZK submitProof path (next_tx_nonce + sign_call) and the strategy path
        // (sign_transaction) must draw from ONE counter.
        let signer = nonce_test_signer(0x6e, 0x6f);
        signer.seed_tx_nonce(50);
        let _ = signer.sign_transaction(&sample_bp(), 42161).await.unwrap(); // uses 50
        assert_eq!(signer.next_tx_nonce().unwrap(), 51);
        let _ = signer.sign_transaction(&sample_bp(), 42161).await.unwrap(); // uses 52
        assert_eq!(signer.peek_tx_nonce(), Some(53));
    }

    // ── build_blueprint_calldata — real ABI encoding ────────────────────────

    #[test]
    fn build_blueprint_calldata_succeeds_when_strategy_configured() {
        let km = make_km(0x05);
        let signer = KeyManagerTransactionSigner::new(
            km,
            Address::from([0x01; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(0x11),
        );
        let bp = sample_bp();
        let encoded = signer.build_blueprint_calldata(&bp).unwrap();
        assert!(!encoded.is_empty());
        // Standard ABI head-tail encoding is always a multiple of 32 bytes.
        assert_eq!(encoded.len() % 32, 0);
    }

    #[test]
    fn build_blueprint_calldata_fails_for_unconfigured_strategy() {
        let km = make_km(0x06);
        let signer = KeyManagerTransactionSigner::new(
            km,
            Address::from([0x01; 20]),
            empty_strategy_ids(),
            make_blueprint_signer(0x12),
        );
        let bp = sample_bp();
        let result = signer.build_blueprint_calldata(&bp);
        assert!(result.is_err());
    }

    #[test]
    fn build_blueprint_calldata_round_trips_through_abi_decode() {
        // Self-consistency check, complementary to (not a substitute
        // for) `build_blueprint_calldata_matches_solc_golden_vector`
        // above: decode what we just encoded via alloy-sol-types' own
        // abi_decode_params, and confirm every field survives the round
        // trip. A self-consistent-but-wrong encoding (e.g. the 2026-08-24
        // abi_encode()-vs-abi_encode_params() bug) would NOT be caught by
        // this test alone, since both sides would share the same bug; it
        // was caught by the independent solc oracle in the golden-vector
        // test.
        let km = make_km(0x07);
        let signer = KeyManagerTransactionSigner::new(
            km,
            Address::from([0x01; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(0x13),
        );
        let bp = sample_bp();
        let encoded = signer.build_blueprint_calldata(&bp).unwrap();

        // abi_decode_params, matching build_blueprint_calldata's
        // abi_encode_params. Using the mismatched Blueprint::abi_decode
        // (the single-dynamic-value form) would decode the flat-tuple
        // bytes against the wrong head shape.
        let decoded = Blueprint::abi_decode_params(&encoded, true).unwrap();
        assert_eq!(decoded.expiry_block, bp.expiry_block);
        assert_eq!(decoded.nonce, bp.nonce);
        assert_eq!(decoded.provider_type, bp.flashloan_provider_type as u8);
        assert_eq!(decoded.flashloan_token, bp.flashloan_token);
        assert_eq!(decoded.provider_contract, bp.provider_contract);
        assert_eq!(decoded.strategy_calldata, bp.calldata);
        assert_eq!(decoded.flashloan_amount, bp.flashloan_amount);
        assert_eq!(decoded.min_net_profit, bp.dynamic_min_profit);
    }

    #[test]
    fn build_blueprint_calldata_max_base_fee_converted_gwei_to_wei() {
        let km = make_km(0x08);
        let signer = KeyManagerTransactionSigner::new(
            km,
            Address::from([0x01; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(0x14),
        );
        let bp = sample_bp();
        let encoded = signer.build_blueprint_calldata(&bp).unwrap();
        // abi_decode_params — see the round-trip test above.
        let decoded = Blueprint::abi_decode_params(&encoded, true).unwrap();
        let expected =
            U256::from(bp.max_base_fee_gwei).saturating_mul(U256::from(1_000_000_000u64));
        assert_eq!(decoded.max_base_fee, expected);
    }

    // ── compute_bp_hash — domain separation ─────────────────────────────────

    #[test]
    fn compute_bp_hash_matches_solc_golden_vector() {
        // Fixture MUST match contracts/test/DomainSeparatedBlueprintHash.t.sol's
        // ORCHESTRATOR / CHAIN_ID / BLUEPRINT_CALLDATA constants. Verifies
        // `compute_bp_hash`'s `abi_encode_params()` against a real solc/EVM
        // oracle, the same way `build_blueprint_calldata_matches_solc_
        // golden_vector` verifies `build_blueprint_calldata`.
        let km = make_km(0x22);
        let signer = KeyManagerTransactionSigner::new(
            km,
            Address::from([0x01; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(0x23),
        );

        // Identical bytes to build_blueprint_calldata_matches_solc_
        // golden_vector's own solc_golden constant above — deliberately
        // reused rather than an arbitrary fixture, so this test's input
        // is itself already solc-verified and the two golden vectors
        // can't quietly drift apart from each other.
        let blueprint_calldata = hex::decode(
            "000000000000000000000000000000000000000000000000000000000000044c\
            0000000000000000000000000000000000000000000000000000000000000000\
            c4bb1c851b1c74593f61f8d1f99ec07e2960d847a94d4a736e321ba387d4d2d7\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000009999999999999999999999999999999999999999\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000140\
            00000000000000000000000000000000000000000000000000000000000f4240\
            00000000000000000000000000000000000000000000000000000000000186a0\
            00000000000000000000000000000000000000000000000000000006fc23ac00\
            0000000000000000000000000000000000000000000000000000000000000004\
            deadbeef00000000000000000000000000000000000000000000000000000000",
        )
        .expect("valid blueprint calldata golden hex");

        let bp_hash = signer.compute_bp_hash(&blueprint_calldata, 42161);

        // Confirmed against solc/EVM itself via `forge test --match-test
        // test_print_golden_domain_separated_blueprint_hash -vv` in
        // contracts/test/DomainSeparatedBlueprintHash.t.sol, which printed
        // this exact value — an exact match to the independent Python
        // (eth_abi) computation it was originally seeded from.
        let solc_golden: [u8; 32] =
            hex::decode("74d1c22598dab6e5f1cb1a1809d3b7255728a0c10c971a9f05257cd6758b356b")
                .expect("valid golden hash hex")
                .try_into()
                .expect("golden hash must be exactly 32 bytes");

        assert_eq!(
            bp_hash, solc_golden,
            "compute_bp_hash must match the independently cross-checked domain-separated hash"
        );
    }

    #[test]
    fn compute_bp_hash_is_deterministic() {
        let km = make_km(0x09);
        let signer = KeyManagerTransactionSigner::new(
            km,
            Address::from([0x01; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(0x15),
        );
        let h1 = signer.compute_bp_hash(b"same input", 42161);
        let h2 = signer.compute_bp_hash(b"same input", 42161);
        assert_eq!(h1, h2);
    }

    #[test]
    fn compute_bp_hash_changes_with_chain_id() {
        let km = make_km(0x0a);
        let signer = KeyManagerTransactionSigner::new(
            km,
            Address::from([0x01; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(0x16),
        );
        let h1 = signer.compute_bp_hash(b"same input", 42161);
        let h2 = signer.compute_bp_hash(b"same input", 1);
        assert_ne!(
            h1, h2,
            "domain separation must include chain_id — a blueprint valid on one \
             chain must not hash identically on another"
        );
    }

    #[test]
    fn compute_bp_hash_changes_with_orchestrator_address() {
        let km1 = make_km(0x0b);
        let km2 = make_km(0x0c);
        let signer_a = KeyManagerTransactionSigner::new(
            km1,
            Address::from([0x01; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(0x17),
        );
        let signer_b = KeyManagerTransactionSigner::new(
            km2,
            Address::from([0x02; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(0x18),
        );
        let h1 = signer_a.compute_bp_hash(b"same input", 42161);
        let h2 = signer_b.compute_bp_hash(b"same input", 42161);
        assert_ne!(
            h1, h2,
            "domain separation must include the orchestrator address — the exact \
             replay-across-deployments vulnerability OmegaOrchestrator.sol's own \
             change #3 note describes fixing"
        );
    }

    // ── orchestrator_bp_hash (C10l) ─────────────────────────────────────────

    #[test]
    fn orchestrator_bp_hash_matches_compute_bp_hash_of_built_calldata() {
        let signer = KeyManagerTransactionSigner::new(
            make_km(0x50),
            Address::from([0x01; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(0x51),
        );
        let bp = sample_bp();
        let calldata = signer.build_blueprint_calldata(&bp).unwrap();
        assert_eq!(
            signer.orchestrator_bp_hash(&bp, 42161).unwrap(),
            signer.compute_bp_hash(&calldata, 42161)
        );
    }

    #[test]
    fn orchestrator_bp_hash_differs_from_engine_blueprint_hash() {
        // The whole reason this accessor exists: the Vault's key is NOT the engine's
        // ExecutionBlueprint::compute_hash().
        let signer = KeyManagerTransactionSigner::new(
            make_km(0x52),
            Address::from([0x01; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(0x53),
        );
        let bp = sample_bp();
        let h = signer.orchestrator_bp_hash(&bp, 42161).unwrap();
        assert_ne!(h.as_slice(), bp.blueprint_hash.as_slice());
    }

    #[test]
    fn orchestrator_bp_hash_fails_for_unconfigured_strategy() {
        let signer = KeyManagerTransactionSigner::new(
            make_km(0x54),
            Address::from([0x01; 20]),
            empty_strategy_ids(),
            make_blueprint_signer(0x55),
        );
        assert!(signer.orchestrator_bp_hash(&sample_bp(), 42161).is_err());
    }

    // ── encode_execute_call — outer calldata assembly ───────────────────────

    #[test]
    fn encode_execute_call_prefixes_the_real_function_selector() {
        // Self-consistency check against alloy-sol-types' own macro-derived
        // selector constant, not an independently hand-computed value.
        let out = encode_execute_call(vec![0xaa, 0xbb], vec![0xcc, 0xdd]);
        assert!(out.starts_with(&executeCall::SELECTOR));
    }

    #[test]
    fn encode_execute_call_round_trips_through_abi_decode() {
        let blueprint_calldata = vec![0x01, 0x02, 0x03];
        let sig = vec![0xaa; 65];
        let out = encode_execute_call(blueprint_calldata.clone(), sig.clone());
        let decoded = executeCall::abi_decode(&out, true).unwrap();
        assert_eq!(
            decoded.blueprint_calldata.as_ref(),
            blueprint_calldata.as_slice()
        );
        assert_eq!(decoded.sig.as_ref(), sig.as_slice());
    }

    // ── envelope_fees_wei — proposed fee policy, pending sign-off ──────────

    #[test]
    fn envelope_fees_match_proposed_formula() {
        // base=10, priority=5 -> tip=5 gwei, max_fee=20 gwei
        let (tip, max_fee) = envelope_fees_wei(10, 5).unwrap();
        assert_eq!(tip, U256::from(5u64) * U256::from(GWEI_TO_WEI));
        assert_eq!(max_fee, U256::from(20u64) * U256::from(GWEI_TO_WEI));
    }

    #[test]
    fn envelope_fees_reject_priority_above_cap() {
        let err = envelope_fees_wei(10, MAX_PRIORITY_FEE_GWEI_CAP + 1).unwrap_err();
        match err {
            ExecutionError::SigningFailed { detail } => {
                assert!(detail.contains("priority_fee_gwei"));
                assert!(detail.contains("cap"));
            }
            other => panic!("expected SigningFailed, got {other:?}"),
        }
    }

    #[test]
    fn envelope_fees_reject_max_fee_above_cap() {
        // base + 2*priority > 1500, with priority still <= 500
        // e.g. base=1401, priority=50 -> max_fee_gwei = 1501
        let err = envelope_fees_wei(1_401, 50).unwrap_err();
        match err {
            ExecutionError::SigningFailed { detail } => {
                assert!(detail.contains("max_fee_gwei"));
                assert!(detail.contains("cap"));
            }
            other => panic!("expected SigningFailed, got {other:?}"),
        }
    }

    #[test]
    fn envelope_fees_at_exact_caps_ok() {
        // priority == 500, max_fee_gwei == 500 + 2*500 == 1500
        let (tip, max_fee) = envelope_fees_wei(500, 500).unwrap();
        assert_eq!(tip, U256::from(500u64) * U256::from(GWEI_TO_WEI));
        assert_eq!(max_fee, U256::from(1_500u64) * U256::from(GWEI_TO_WEI));
    }

    #[tokio::test]
    async fn sign_transaction_fails_closed_when_priority_fee_exceeds_cap() {
        // End-to-end: an out-of-policy bp must fail at the fee-cap check,
        // before any RLP is built or key material touched, even though
        // strategy_onchain_ids and the blueprint signer are both correctly
        // configured.
        let tx_km = make_km(0x19);
        let signer = KeyManagerTransactionSigner::new(
            tx_km,
            Address::from([0x01; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(0x1a),
        );
        let mut bp = sample_bp();
        bp.priority_fee_gwei = MAX_PRIORITY_FEE_GWEI_CAP + 1;

        let result = signer.sign_transaction(&bp, 42161).await;
        match result {
            Err(ExecutionError::SigningFailed { detail }) => {
                assert!(detail.contains("priority_fee_gwei"));
            }
            other => panic!("expected SigningFailed, got {other:?}"),
        }
    }

    // ── RLP encoder — structural checks against the EIP-1559 spec text ─────

    #[test]
    fn rlp_bytes_single_byte_below_0x80_is_itself() {
        assert_eq!(rlp_bytes(&[0x00]), vec![0x00]);
        assert_eq!(rlp_bytes(&[0x7f]), vec![0x7f]);
    }

    #[test]
    fn rlp_bytes_empty_is_0x80() {
        assert_eq!(rlp_bytes(&[]), vec![0x80]);
    }

    #[test]
    fn rlp_bytes_short_string_prefix() {
        // "dog" (3 bytes) -> 0x83 'd' 'o' 'g' — classic RLP spec example.
        assert_eq!(rlp_bytes(b"dog"), vec![0x83, b'd', b'o', b'g']);
    }

    #[test]
    fn rlp_u64_zero_is_0x80() {
        assert_eq!(rlp_u64(0), vec![0x80]);
    }

    #[test]
    fn rlp_u64_strips_leading_zero_bytes() {
        // 1 -> single byte 0x01, which is < 0x80, so it's encoded as itself.
        assert_eq!(rlp_u64(1), vec![0x01]);
    }

    #[test]
    fn rlp_list_empty_is_0xc0() {
        assert_eq!(rlp_list(&[]), vec![0xc0]);
    }

    #[test]
    fn unsigned_eip1559_payload_starts_with_type_byte_0x02() {
        let payload = encode_eip1559_unsigned(
            42161,
            0,
            U256::from(1_000_000_000u64),
            U256::from(2_000_000_000u64),
            100_000,
            Address::from([0x11; 20]),
            U256::ZERO,
            &[],
        );
        assert_eq!(
            payload[0], 0x02,
            "EIP-1559 typed transactions must be prefixed with 0x02"
        );
        // Byte after the type prefix must be a valid RLP list header
        // (>= 0xc0), per EIP-2718's own differentiation rule from legacy
        // transactions.
        assert!(payload[1] >= 0xc0);
    }

    #[test]
    fn signed_eip1559_payload_includes_signature_fields() {
        let unsigned = encode_eip1559_unsigned(
            1,
            5,
            U256::from(1u64),
            U256::from(2u64),
            21_000,
            Address::from([0x22; 20]),
            U256::ZERO,
            &[0xde, 0xad],
        );
        let signed = encode_eip1559_signed(
            1,
            5,
            U256::from(1u64),
            U256::from(2u64),
            21_000,
            Address::from([0x22; 20]),
            U256::ZERO,
            &[0xde, 0xad],
            1,
            &[0xaa; 32],
            &[0xbb; 32],
        );
        assert!(
            signed.len() > unsigned.len(),
            "signed payload must be strictly larger (it has 3 extra fields)"
        );
        assert_eq!(signed[0], 0x02);
    }

    #[test]
    fn encode_eip1559_signed_matches_a_real_mainnet_transaction() {
        // GOLDEN VECTOR — a REAL, previously-broadcast Ethereum mainnet
        // transaction, not a synthetic case (a USDT transfer(), selector
        // 0xa9059cbb). Its fields were decoded independently via Python's
        // `rlp` library, then fed through a standalone extraction of this
        // file's own encode_eip1559_signed, and confirmed to reproduce
        // these exact bytes: not "matches spec text," not "matches another
        // library's synthetic construction," but "matches bytes mined into
        // a real Ethereum block."
        //
        // Also exercises the LONG-FORM RLP paths (rlp_bytes'/rlp_list's
        // `else` branches) via this transaction's 68-byte calldata.
        let real_data = hex::decode(
            "a9059cbb000000000000000000000000622779096805724b38c42b51989ddca32d671a\
             000000000000000000000000000000000000000000000000000000000022df0080",
        )
        .expect("valid real tx calldata hex");
        let real_to_bytes: [u8; 20] = hex::decode("dac17f958d2ee523a2206206994597c13d831ec7")
            .expect("valid real tx `to` hex")
            .try_into()
            .expect("real tx `to` must be exactly 20 bytes");
        let real_to = Address::from(real_to_bytes);
        let real_r =
            hex::decode("236084da36000fb2c7373cfa78e8f1bc9d8eb081dc240630c8024aa06fc39f96")
                .expect("valid real tx r hex");
        let real_s =
            hex::decode("30bdc5cd4e1f5f6abbb36c3b004270b68724cc46c56ad5847c99f8ced9c4112d")
                .expect("valid real tx s hex");

        let signed = encode_eip1559_signed(
            1,                             // chain_id — Ethereum mainnet
            86964,                         // nonce
            U256::from(1_000_000_000u64),  // max_priority_fee_per_gas
            U256::from(34_154_125_362u64), // max_fee_per_gas
            120_000,                       // gas_limit
            real_to,
            U256::ZERO, // value
            &real_data,
            1, // y_parity
            &real_r,
            &real_s,
        );

        let expected_raw_hex = "02f8b401830153b4843b9aca008507f3be98328301d4c094dac17f958d2ee523a2206206994597c13d831ec780b844a9059cbb000000000000000000000000622779096805724b38c42b51989ddca32d671a000000000000000000000000000000000000000000000000000000000022df0080c001a0236084da36000fb2c7373cfa78e8f1bc9d8eb081dc240630c8024aa06fc39f96a030bdc5cd4e1f5f6abbb36c3b004270b68724cc46c56ad5847c99f8ced9c4112d";
        assert_eq!(
            hex::encode(&signed),
            expected_raw_hex,
            "must reproduce a real, previously-mined Ethereum transaction's raw bytes exactly"
        );
    }

    #[test]
    fn encode_eip1559_long_form_rlp_paths_match_independent_implementation() {
        // Synthetic but targeted: calldata >= 56 bytes forces rlp_bytes'
        // long-form branch (0xb7+len_of_len prefix), and pushes the
        // outer list payload past 55 bytes too, forcing rlp_list's own
        // long-form branch (0xf7+len_of_len). Verified against Python's
        // independent `rlp` library — byte-for-byte match, including both
        // long-form prefixes (the 0xf8/0xb8 bytes in the expected hex below).
        let data: Vec<u8> = (0u8..100).collect();
        let to = Address::from([0x33; 20]);

        let unsigned = encode_eip1559_unsigned(
            42161,
            99,
            U256::from(3_000_000_000u64),
            U256::from(15_000_000_000u64),
            2_000_000,
            to,
            U256::ZERO,
            &data,
        );
        let expected_unsigned_hex = "02f89082a4b16384b2d05e0085037e11d600831e848094333333333333333333333333333333333333333380b864000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f60616263c0";
        assert_eq!(hex::encode(&unsigned), expected_unsigned_hex);

        let signed = encode_eip1559_signed(
            42161,
            99,
            U256::from(3_000_000_000u64),
            U256::from(15_000_000_000u64),
            2_000_000,
            to,
            U256::ZERO,
            &data,
            1,
            &[0x33; 32],
            &[0x44; 32],
        );
        let expected_signed_hex = "02f8d382a4b16384b2d05e0085037e11d600831e848094333333333333333333333333333333333333333380b864000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f60616263c001a03333333333333333333333333333333333333333333333333333333333333333a04444444444444444444444444444444444444444444444444444444444444444";
        assert_eq!(hex::encode(&signed), expected_signed_hex);
    }

    // ── Test fixtures ────────────────────────────────────────────────────

    fn sample_bp() -> ExecutionBlueprint {
        use alloy_primitives::B256;
        use omega_core::types::blueprint::StrategyId;
        use omega_core::types::lane::{Lane, Simulator};
        use uuid::Uuid;

        let signal_id = Uuid::from_bytes([0x01; 16]);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(StrategyId::Sa, 42161, 0, signal_id);
        let base_fee_at_creation = 10;
        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO,
            chain_id: 42161,
            strategy_id: StrategyId::Sa,
            lane: Lane::Microtx,
            simulator: Simulator::Revm,
            signal_state_hash: B256::ZERO,
            state_version: 1,
            signal_id,
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::from(1_000_000u64),
            flashloan_available: U256::from(2_000_000u64),
            // Real, meaningful values now that build_blueprint_calldata
            // genuinely reads these three — see this file's header, "Lint notes".
            flashloan_provider_type: FlashloanProviderType::Balancer,
            provider_contract: Address::ZERO,
            flashloan_token: Address::from([0x99; 20]),
            calldata: Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]),
            strategy_bytecode_hash: B256::ZERO,
            l2_exec_gas_estimate: 100_000,
            l1_data_gas_estimate: 0,
            extraction_gas: 21_000,
            expected_profit_net: U256::from(1_000_000u64),
            dynamic_min_profit: U256::from(100_000u64),
            l2_buffer_factor: 1.15,
            l1_data_buffer_factor: 1.10,
            slippage_bps: 50,
            base_fee_at_creation,
            l1_data_fee_at_creation: 2,
            priority_fee_gwei: 5,
            max_base_fee_gwei: ExecutionBlueprint::derive_max_base_fee_gwei(
                base_fee_at_creation,
                3.0,
            ),
            price_impact_bps: None,
            ofa_compliant: false,
            expiry_block: 1_100,
            nonce: 0,
            confirmation_depth: 12,
            client_order_id,
            idempotency_key: B256::ZERO,
            relay_targets: vec![],
            zk_proof_commitment: None,
        };
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();
        bp
    }
}
