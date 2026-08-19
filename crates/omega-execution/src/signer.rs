// crates/omega-execution/src/signer.rs
//
// TransactionSigner — the genuinely missing piece identified in
// ExecutionPipelineSpecification.md §8.
//
// ## Status as of this revision — real progress against a real contract
//
// The prior revision of this file listed four unconfirmed items blocking
// `build_execute_calldata`. This revision was given the real
// `contracts/src/OmegaOrchestrator.sol` source for the first time. Against
// that real text:
//
//   1. RESOLVED — ABI encoder. `alloy-sol-types` is now a workspace
//      dependency (matching the existing `alloy-primitives = "0.8"` pin).
//      `Blueprint` / `DomainSeparatedBlueprint` / `execute` below are
//      `sol!`-macro-generated types whose field ORDER and TYPES are
//      transcribed directly from OmegaOrchestrator.sol's own
//      `execute()` decode statement:
//        (uint64, uint64, bytes32, FlashloanProviderType, address, address,
//         bytes, uint256, uint256, uint256)
//      Solidity ABI-encodes a struct identically to a tuple of the same
//      field types in the same order, and identically encodes an enum to
//      its `uint8` ordinal — so `Blueprint::abi_encode()` produces the
//      exact bytes `abi.decode(blueprintCalldata, (...))` expects,
//      PROVIDED `provider_type` is passed as the correct ordinal. That
//      last piece is not a guess: `omega_core::types::flashloan_provider`
//      already carries a passing test,
//      `ordinals_match_solidity_enum_order`, guaranteeing
//      `FlashloanProviderType as u8` matches this exact contract's enum
//      order. Field-name casing in the `sol!` blocks below was chosen as
//      plain Rust snake_case rather than copied verbatim from the
//      contract (`strategyId`, `flashloanToken`, etc.) — ABI encoding of
//      a struct depends only on field TYPE and ORDER, never on the names
//      chosen for them in the encoding library, so this is a readability
//      choice, not a behavioral one, and avoids `-D warnings` tripping
//      on `non_snake_case` for no benefit.
//
//   2. RESOLVED (by inspection, not yet byte-verified against a compiled
//      solc/EVM decode) — the flashloan-identity ABI shape. Confirmed
//      directly from the real contract: `providerType` is `uint8`,
//      `providerContract` is `address` (meaningful ONLY for UniswapV3 —
//      the contract's own comment says pass `address(0)` for
//      Balancer/AaveV3, which use fixed admin-set addresses instead),
//      `flashloanToken` is `address`. Mapped from `ExecutionBlueprint`'s
//      already-real `flashloan_provider_type` / `provider_contract` /
//      `flashloan_token` fields below.
//
//   3. RESOLVED (semantic match, not an explicit written cross-reference)
//      — `minNetProfit`'s source. The contract's own check —
//      `if (netProfit < minNetProfit) revert InsufficientProfit(...)` —
//      is exactly the on-chain profit floor `dynamic_min_profit` already
//      represents off-chain throughout this workspace's pre-trade risk
//      checks (see `omega_risk::checks`'s own
//      `insufficient_profit_fails_at_check_5`). Mapped directly:
//      `min_net_profit: bp.dynamic_min_profit`.
//
//   4. STILL OPEN — the `StrategyId -> bytes32 strategyId` mapping.
//      Read directly against the real contract: `registerStrategy(bytes32
//      strategyId, address implementation)` accepts an ARBITRARY bytes32
//      chosen by whoever calls it — there is no on-chain derivation rule
//      from a string like "SA"/"LA" to a bytes32 value. This is real
//      DEPLOYMENT CONFIGURATION (whatever bytes32 was actually passed to
//      `registerStrategy()` for each strategy), not something derivable
//      from code, and guessing a hash convention here (e.g.
//      `keccak256("SA")`) for a contract that moves flashloaned funds
//      would be exactly the fabrication this codebase has refused
//      everywhere else. `KeyManagerTransactionSigner::new` now takes a
//      required `strategy_onchain_ids: HashMap<String, [u8; 32]>`
//      parameter — keyed by the same `StrategyId::to_string()` values
//      (`"SA"`, `"LA"`, ...) already used by `IntegrityRegistry` and
//      `DeploymentManifest` elsewhere in this workspace — that MUST be
//      sourced from real deployment records (the actual arguments passed
//      to `registerStrategy()` on-chain), never fabricated. A lookup miss
//      fails loudly and specifically, per-strategy, rather than silently
//      defaulting to a placeholder.
//
// ## RESOLVED (this revision): the blueprint-authorization signature
//
// `omega-security/src/signer.rs`'s real source was provided and directly
// confirms `BlueprintSigner::sign_raw_hash()` is exactly the primitive
// needed: it signs a hash with NO prefix of any kind and returns a
// 65-byte `[r(32) || s(32) || v(1)]` signature with `v = recovery_id +
// 27` — precisely the input `OmegaOrchestrator.sol`'s `bpHash.recover(sig)`
// (OpenZeppelin `ECDSA.recover(bytes32, bytes)`, v ∈ {27, 28}) expects.
// `BlueprintSigner::sign()` (the OTHER method on that same type) is
// confirmed NOT usable here — it applies an EIP-191 prefix intended for
// the Flashbots reputation header, a different, unrelated signature.
//
// `KeyManagerTransactionSigner` now takes a required
// `blueprint_signer: Arc<BlueprintSigner>` — deliberately a SEPARATE
// dependency from `tx_key_manager`, since (per this struct's own doc
// comment) the gas-paying transaction-envelope key and the on-chain
// blueprint-authorization key are two independent concerns; a caller may
// legitimately construct both from the same `KeyManager`/`Arc` or from
// two different ones, and this file does not assume either. With this
// wired in, `build_execute_calldata` — and therefore `sign_transaction`
// — can now succeed end-to-end for a strategy present in
// `strategy_onchain_ids`. See `build_execute_calldata_succeeds_with_
// real_blueprint_signer` / `sign_transaction_succeeds_end_to_end_with_
// real_blueprint_signer` below for the regression tests proving this.
//
// The ONLY remaining gap in this file is the same one item 4 (above)
// already names: real, deployment-sourced values for
// `strategy_onchain_ids` — not something this file can supply for
// itself.
//
// `omega_simulation::SimulationSubmitter` DOES sign and send real
// transactions, but only against a local Anvil fork with a dev-funded
// Anvil test key, and is deliberately, structurally walled off from any
// live transport (`reject_if_live_looking`, construction only via
// `SimulationSubmitter::bound_to(&ForkHandle, ..)`) — not a substitute
// for this type.
//
// `ExecutionPipeline` is generic over `S: TransactionSigner` specifically
// so the rest of the pipeline (integrity check, kill switch, 15 pre-trade
// checks, idempotency dedup, DAG bookkeeping) is fully implemented and
// fully testable today, independent of this gap.
//
// ## Audit fix (carried forward): test helper missing flashloan/token fields
//
// `tests::sample_bp` predates `ExecutionBlueprint` gaining
// `flashloan_provider_type`, `provider_contract`, and `flashloan_token`
// (it already included `max_base_fee_gwei`). As of THIS revision, those
// three fields ARE read by the real `build_blueprint_calldata` path
// below — `sample_bp()` now sets them to real, meaningful (not merely
// inert) values so the tests that exercise that path actually exercise
// something, rather than keeping the old `Address::ZERO`-everywhere
// convention that made sense only while these fields were unread.
//
// ## Audit fix (carried forward, 2): clippy::too_many_arguments
//
// `encode_eip1559_unsigned` / `encode_eip1559_signed` carry
// `#[allow(clippy::too_many_arguments)]` — their argument lists are
// mandated 1:1 by EIP-1559's own field order (see the "EIP-1559 RLP
// encoding helpers" section below), so a struct-of-params refactor would
// obscure the correspondence to the spec this code deliberately
// preserves, not simplify anything.

use std::collections::HashMap;
use std::sync::Arc;

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
// See this file's top doc comment, item 1, for the encoding-equivalence
// argument (struct-of-fields == tuple-of-same-fields in Solidity ABI
// encoding; field names below are plain Rust snake_case by choice, not
// copied contract identifiers, since names do not affect encoded bytes).
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
/// module's doc comment for why no COMPLETE production implementation
/// exists yet.
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
    /// see this file's top doc comment, "RESOLVED (this revision)", for
    /// why the latter is confirmed incompatible with this contract.
    blueprint_signer: Arc<BlueprintSigner>,
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
            secp: Secp256k1::new(),
        }
    }

    /// The address that will appear as `from` on every transaction this
    /// signer produces. Useful for pre-funding checks / logging without
    /// exposing key material.
    pub fn active_address(&self) -> [u8; 20] {
        self.tx_key_manager.active_address()
    }

    /// Build the real, ABI-encoded `blueprintCalldata` bytes for `bp` —
    /// see this file's top doc comment, items 1-4, for exactly what's
    /// confirmed here and against what evidence.
    ///
    /// The ONLY failure mode is a strategy missing from
    /// `strategy_onchain_ids` — everything else is a pure, infallible
    /// transformation of already-real `ExecutionBlueprint` fields.
    fn build_blueprint_calldata(&self, bp: &ExecutionBlueprint) -> Result<Vec<u8>, ExecutionError> {
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

        Ok(blueprint.abi_encode())
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
        omega_security::keccak256(&domain.abi_encode())
    }

    /// Build the ABI-encoded outer calldata for
    /// `OmegaOrchestrator.execute(bytes blueprintCalldata, bytes sig)`,
    /// including the real on-chain-authorization signature.
    ///
    /// As of this revision this is fully real end-to-end: `blueprintCalldata`
    /// and `bp_hash` are built and hashed for real (see
    /// `build_blueprint_calldata` / `compute_bp_hash` above), `sig` is
    /// produced by `BlueprintSigner::sign_raw_hash()` — the primitive
    /// confirmed compatible with `OmegaOrchestrator.sol`'s
    /// `bpHash.recover(sig)` (see this file's top doc comment, "RESOLVED
    /// (this revision)") — and the three are assembled via
    /// `encode_execute_call`. The only remaining failure mode is a
    /// strategy missing from `strategy_onchain_ids` (real deployment
    /// configuration this file cannot supply for itself) or a signing
    /// failure surfaced from `BlueprintSigner` itself (e.g. no active
    /// key) — both fail loudly and specifically, never silently.
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
        // Fails loudly here, before touching key material, per this
        // module's top doc comment. Everything below this point is dead
        // code until build_execute_calldata has a real implementation, but
        // is left in place (rather than deleted) so the RLP/signing path
        // is reviewable and ready to wire up once the blueprint-
        // authorization signature is resolved.
        let data = self.build_execute_calldata(bp, chain_id)?;

        let secret_key =
            self.tx_key_manager
                .active_secret_key()
                .ok_or(ExecutionError::SigningFailed {
                    detail: "tx_key_manager has no active signing key".into(),
                })?;

        let gas_limit = bp.total_l2_gas_budget();
        let max_priority_fee_per_gas =
            U256::from(bp.priority_fee_gwei).saturating_mul(U256::from(1_000_000_000u64));
        // NOTE: this fee formula is NOT a confirmed policy decision — see
        // this module's top doc comment. It's a placeholder conservative
        // estimate (base_fee_at_creation + 2x priority, converted to wei)
        // used only to exercise the RLP-encoding path in tests; whoever
        // owns the gas/fee policy should confirm or replace this before
        // any real transaction is built from it.
        let max_fee_per_gas = U256::from(bp.base_fee_at_creation)
            .saturating_add(U256::from(bp.priority_fee_gwei).saturating_mul(U256::from(2u64)))
            .saturating_mul(U256::from(1_000_000_000u64));

        let unsigned_rlp = encode_eip1559_unsigned(
            chain_id,
            bp.nonce,
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
        // (EIP-2), and separately the requirement OmegaOrchestrator.sol's
        // ECDSA.recover applies to the blueprint-authorization signature
        // once that call is wired. No extra normalization step is needed
        // here.
        let (recovery_id, compact) = self
            .secp
            .sign_ecdsa_recoverable(&msg, &secret_key)
            .serialize_compact();
        let y_parity = recovery_id.to_i32() as u8;

        let signed_rlp = encode_eip1559_signed(
            chain_id,
            bp.nonce,
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

        let raw_tx_hex = format!("0x{}", hex::encode(&signed_rlp));

        tracing::debug!(
            blueprint_hash = %bp.blueprint_hash,
            chain_id,
            nonce = bp.nonce,
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
/// — see the tests below. As of this revision, genuinely called from
/// `build_execute_calldata` above (no longer dead code) — kept as a free
/// function regardless, since that keeps the "pure assembly" step
/// independently testable from the signing step that produces its `sig`
/// input.
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
// NOT yet checked against a known signed-transaction test vector (none was
// available in this investigation) — see this file's top doc comment.
// self-contained rather than pulling in alloy's consensus/signer feature
// set, since this encoder does not need the full consensus surface, only
// RLP.
//
// Both `encode_eip1559_unsigned` and `encode_eip1559_signed` carry
// `#[allow(clippy::too_many_arguments)]` — see this file's module-level
// "Audit fix (carried forward, 2)" note for why: their argument lists are
// dictated 1:1 by EIP-1559's own field order, not an accident of API
// design that a struct would clean up.

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
        rlp_bytes(r),
        rlp_bytes(s),
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

    // ── Construction guard ────────────────────────────────────────────────

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

    // ── sign_transaction currently fails loudly, not silently ──────────────

    #[tokio::test]
    async fn sign_transaction_succeeds_end_to_end_with_real_blueprint_signer() {
        // Regression guard for the OPPOSITE direction from this file's
        // prior revision: now that build_execute_calldata has a real
        // strategy_onchain_ids entry AND a real BlueprintSigner, signing
        // must actually succeed — producing a real signed RLP transaction
        // — not fail at any point. If this starts failing, either the ABI
        // encoding, the domain hash, or the BlueprintSigner wiring
        // regressed.
        let tx_km = make_km(0x03);
        let signer = KeyManagerTransactionSigner::new(
            tx_km,
            Address::from([0x01; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(0x0d),
        );
        let bp = sample_bp();

        let result = signer.sign_transaction(&bp, 42161).await;
        let signed = result.expect("sign_transaction must succeed with real config wired");
        assert!(signed.raw_tx_hex.starts_with("0x02"), "must be an EIP-1559 typed tx");
        assert!(signed.raw_tx_hex.len() > 4, "must contain real RLP payload, not just the type byte");
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
        // Distinguishes the ONE remaining failure reason this signer can
        // still produce: an unconfigured strategy_onchain_ids entry fails
        // BEFORE bp_hash is ever computed or the BlueprintSigner is ever
        // called, with a message naming the missing strategy specifically.
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
        // Strong self-consistency check in place of an external solc/EVM
        // oracle: decode what we just encoded via alloy-sol-types' own
        // abi_decode, and confirm every field survives the round trip.
        // This validates the `Blueprint` sol! type is internally
        // consistent and that every field was assigned to the position I
        // intended — it does NOT independently prove byte-for-byte
        // agreement with solc's own encoder, though alloy-sol-types is a
        // widely-used, spec-compliant ABI implementation.
        let km = make_km(0x07);
        let signer = KeyManagerTransactionSigner::new(
            km,
            Address::from([0x01; 20]),
            strategy_ids_with_sa(),
            make_blueprint_signer(0x13),
        );
        let bp = sample_bp();
        let encoded = signer.build_blueprint_calldata(&bp).unwrap();

        let decoded = Blueprint::abi_decode(&encoded, true).unwrap();
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
        let decoded = Blueprint::abi_decode(&encoded, true).unwrap();
        let expected =
            U256::from(bp.max_base_fee_gwei).saturating_mul(U256::from(1_000_000_000u64));
        assert_eq!(decoded.max_base_fee, expected);
    }

    // ── compute_bp_hash — domain separation ─────────────────────────────────

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

    // ── encode_execute_call — outer calldata assembly ───────────────────────

    #[test]
    fn encode_execute_call_prefixes_the_real_function_selector() {
        // Self-consistency check against alloy-sol-types' own macro-derived
        // selector constant, not an independently hand-computed value —
        // see this file's top doc comment for why that's the honest
        // framing here.
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
            // genuinely reads these three — see this file's top doc
            // comment, "Audit fix (carried forward)".
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