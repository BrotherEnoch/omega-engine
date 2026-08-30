// crates/omega-execution/src/signer.rs
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
//      its `uint8` ordinal — so encoding `Blueprint`'s fields as a flat
//      tuple (see the 2026-08-24 bug-fix note below for exactly which
//      alloy-sol-types method that requires) produces the exact bytes
//      `abi.decode(blueprintCalldata, (...))` expects, PROVIDED
//      `provider_type` is passed as the correct ordinal. That last piece
//      is not a guess: `omega_core::types::flashloan_provider` already
//      carries a passing test, `ordinals_match_solidity_enum_order`,
//      guaranteeing `FlashloanProviderType as u8` matches this exact
//      contract's enum order. Field-name casing in the `sol!` blocks
//      below was chosen as plain Rust snake_case rather than copied
//      verbatim from the contract (`strategyId`, `flashloanToken`, etc.)
//      — ABI encoding of a struct depends only on field TYPE and ORDER,
//      never on the names chosen for them in the encoding library, so
//      this is a readability choice, not a behavioral one, and avoids
//      `-D warnings` tripping on `non_snake_case` for no benefit.
//
//   2. RESOLVED — the flashloan-identity ABI shape, confirmed byte-for-byte
//      against a compiled solc/EVM decode (see
//      `build_blueprint_calldata_matches_solc_golden_vector` below, which
//      now compares against a real `forge test` golden vector rather than
//      the earlier internal-only `abi_decode` round-trip check — and which
//      is what caught the 2026-08-24 encoding bug documented below).
//      Confirmed directly from the real contract: `providerType` is `uint8`,
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
//   4. RESOLVED (2026-08-25, in the CALLER, not this file — see below) —
//      the `StrategyId -> bytes32 strategyId` mapping.
//      Read directly against the real contract: `registerStrategy(bytes32
//      strategyId, address implementation)` accepts an ARBITRARY bytes32
//      chosen by whoever calls it — there is no on-chain derivation rule
//      from a string like "SA"/"LA" to a bytes32 value. This is real
//      DEPLOYMENT CONFIGURATION (whatever bytes32 was actually passed to
//      `registerStrategy()` for each strategy), not something derivable
//      from code, and guessing a hash convention here (e.g.
//      `keccak256("SA")`) for a contract that moves flashloaned funds
//      would be exactly the fabrication this codebase has refused
//      everywhere else. `KeyManagerTransactionSigner::new` correctly
//      still takes a required `strategy_onchain_ids: HashMap<String,
//      [u8; 32]>` parameter — keyed by the same `StrategyId::to_string()`
//      values (`"SA"`, `"LA"`, ...) already used by `IntegrityRegistry`
//      and `DeploymentManifest` elsewhere in this workspace — and this
//      file still does not, and should not, hardcode deployment data
//      itself; that would just relocate the fabrication risk rather than
//      remove it.
//
//      What's actually resolved is WHERE THE CALLER GETS REAL VALUES
//      FROM: `crates/omega-engine/src/main.rs`'s own `strategy_
//      onchain_ids()` function (its "C6" revision) now supplies this
//      map, transcribed byte-for-byte from `contracts/src/
//      StrategyIds.sol`'s five `keccak256("OMEGA_STRATEGY_<X>")`
//      constants — the same canonical values `contracts/script/
//      RegisterStrategies.s.sol` reads and asserts against every
//      deployment manifest's `onchain_id` field (via that script's own
//      `_checkManifestIdMatches` guard) before ever calling
//      `registerStrategy()` on-chain, so a typo'd manifest can't
//      silently register a strategy under the wrong id.
//
//      As of this same revision, `StrategyIds.sol`'s actual primary
//      source has been seen directly (not just referenced secondhand by
//      other files' comments), and each of its five constants was
//      independently RE-DERIVED — via a separate keccak256
//      implementation (Python's `Crypto.Hash.keccak`, not
//      alloy/solc/cast) computing `keccak256(utf8("OMEGA_STRATEGY_SA"))`
//      etc. from scratch — and confirmed to match the Solidity file's
//      own constants byte-for-byte, for all five (SA/LA/MSA/MEV/CNRY),
//      not just SA. This is a materially stronger claim than "three
//      sources happen to agree with each other": it confirms
//      `StrategyIds.sol`'s constants actually equal what their own
//      `/// @dev keccak256(...)` comments say they are, using a fourth,
//      independent computation, not just cross-referencing.
//      `config/deployment/arbitrum.toml`'s `onchain_id` fields (verified
//      there separately via `cast keccak`) and this file's own
//      `build_blueprint_calldata_matches_solc_golden_vector` SA fixture
//      (verified there via a real `forge test` run against solc) both
//      still independently agree with these same values, for the
//      overlapping cases each covers.
//
//      MANUAL-SYNC RISK, same as `main.rs`'s own doc comment already
//      flags: nothing keeps `StrategyIds.sol`, `main.rs`'s hardcoded
//      map, and `arbitrum.toml`'s `onchain_id` fields in sync
//      automatically if `StrategyIds.sol` is ever changed. See the
//      `known_strategy_onchain_ids_are_internally_consistent` test
//      below for a narrow, this-file-local guard against the specific
//      failure mode of a copy-paste/transposition error among the five
//      values as transcribed here for cross-reference — it cannot
//      detect drift against `StrategyIds.sol` itself, only internal
//      self-consistency (distinct, non-zero, and SA matching the
//      existing golden-vector fixture).
//
//      NOT YET DONE, and correctly so per `main.rs`'s own C4-A note:
//      `config/deployment/arbitrum.toml`'s `implementation` and
//      `bytecode_hash` fields are still real TODO placeholders — those
//      need an actual live deployment (or an `eth_getCode` read against
//      one), not something derivable from `StrategyIds.sol` the way
//      `onchain_id` is. `strategy_onchain_ids` and the broader
//      `DeploymentManifest` (bytecode hash / contract address /
//      integrity registration) remain two genuinely separate pieces of
//      deployment configuration — see the option-2 sketch that exists
//      for eventually unifying them into one manifest, not yet
//      implemented — closing item 4 does not imply the manifest is
//      complete.
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
// As of 2026-08-25, item 4 is also resolved — see the updated item 4
// entry above for the full evidence chain (main.rs's C6 revision,
// cross-checked against StrategyIds.sol, RegisterStrategies.s.sol, and
// arbitrum.toml). Nothing named by items 1-4 remains open in this file's
// own scope; the deployment-manifest work described in item 4's "NOT YET
// DONE" note (bytecode_hash / implementation address) is a separate,
// still-open piece of configuration this file was never responsible for.
//
// ## BUG FOUND AND FIXED (2026-08-24): Blueprint::abi_encode() was wrong
//
// The first real run of `build_blueprint_calldata_matches_solc_golden_vector`
// against the actual `forge test --match-test
// test_print_golden_blueprint_calldata` output FAILED. The Rust encoding
// carried one extra leading 32-byte word (value `0x20`) that the solc
// golden vector did not — every byte from that point on was identical
// between the two. That is the textbook signature of the difference
// between two distinct Solidity ABI-encoding forms:
//   - `abi.encode(oneDynamicValue)` — a SINGLE dynamic-typed argument
//     gets wrapped with a leading offset word pointing back to itself
//     (this is what you get when the "argument" being encoded is one
//     value, e.g. a struct, treated as a self-contained dynamic type).
//   - `abi.encode(field1, field2, ..., fieldN)` — multiple top-level
//     arguments are encoded as a flat tuple; the tuple's own encoding
//     never needs a pointer to itself, since it isn't nested inside
//     anything.
// `OmegaOrchestrator.sol` decodes with `abi.decode(blueprintCalldata,
// (uint64, uint64, bytes32, ...))` — the flat-tuple form (ten top-level
// types, not one struct-typed value) — so the Rust side must produce
// that same form. `alloy_sol_types::SolValue::abi_encode()`, called on a
// `sol!`-generated struct, encodes the struct as a single dynamic value
// (the first form above) — which is why it produced the spurious extra
// word. `SolValue::abi_encode_params()` is the method that encodes a
// struct's fields as a flat top-level tuple (the second form), matching
// what `abi.encode(a, b, c, ...)` produces in Solidity. Fixed:
//   - `build_blueprint_calldata`: `blueprint.abi_encode()` ->
//     `blueprint.abi_encode_params()`.
//   - `compute_bp_hash`: `domain.abi_encode()` -> `domain.abi_encode_params()`
//     on `DomainSeparatedBlueprint`, by the identical reasoning — the
//     contract's own domain hash is built from `abi.encode(address,
//     uint64, bytes)`, three top-level params, not a struct.
//   - The `build_blueprint_calldata_round_trips_through_abi_decode` test's
//     decode call was updated from `Blueprint::abi_decode` to
//     `Blueprint::abi_decode_params` to match the new encode call — the
//     two must use matching head-shape assumptions or the round trip
//     would silently decode garbage rather than catching a mismatch.
// `build_blueprint_calldata_matches_solc_golden_vector` passes byte-for-
// byte against the real solc/EVM golden vector with this fix in place.
//
// CAVEAT (RESOLVED 2026-08-24, later same day): the `compute_bp_hash` /
// `DomainSeparatedBlueprint` half of this fix was originally BY ANALOGY
// to the now-confirmed `Blueprint` bug only, then given its own golden-
// vector test (`compute_bp_hash_matches_solc_golden_vector` below,
// backed by `contracts/test/DomainSeparatedBlueprintHash.t.sol`) seeded
// from an independent Python (`eth_abi`) computation rather than solc
// itself. `forge test --match-test
// test_print_golden_domain_separated_blueprint_hash -vv` has now been
// run for real and printed
// `0x74d1c22598dab6e5f1cb1a1809d3b7255728a0c10c971a9f05257cd6758b356b`
// — an EXACT match to the Python-computed value already in both this
// file and the `.sol` file, with no update needed to either. This is now
// the same class of evidence `build_blueprint_calldata`'s golden vector
// has: a real solc/EVM oracle, not analogy and not a second library's
// agreement with itself.
//
// ## PROPOSED (2026-08-24), PENDING SIGN-OFF: envelope fee formula
//
// This section replaces the prior "NOTE: this fee formula is NOT a
// confirmed policy decision" marker on `sign_transaction`'s
// `max_priority_fee_per_gas` / `max_fee_per_gas` computation. It is a
// PROPOSAL under review, not a completed approval — see
// `docs/fee-policy.md` in this repo, specifically that file's Sign-Off
// table, which has not been completed as of this revision. Any comment,
// commit message, or doc claiming this is "approved" or "resolved" is
// only as good as that Sign-Off table actually being filled in by the
// person who owns this system's gas/fee policy (Andre Niemand) through
// a durable channel (e.g. a signed commit, not just a chat message
// asserting identity) — self-identification in a chat conversation is
// not independent verification and should not be treated as equivalent
// to it.
//
// Formula (blueprint fields in gwei -> RLP fields in wei), unchanged
// numerically from the prior placeholder:
//   max_priority_fee_per_gas = priority_fee_gwei * 1e9
//   max_fee_per_gas = (base_fee_at_creation + 2 * priority_fee_gwei) * 1e9
//
// NOTE: docs/fee-policy.md's own drafted analysis (§3) argued the
// *opposite* term should carry the 2x multiplier (`2*base + priority`,
// on the reasoning that base fee, not tip, is what a maxFeePerGas cap
// is conventionally sized to survive against on Arbitrum). This revision
// keeps the placeholder's original formula (`base + 2*priority`)
// because that is what was explicitly specified when this change was
// requested, not because the base-vs-tip judgment call has been
// resolved in its favor. Whoever completes the Sign-Off table should
// confirm the formula, not just the caps.
//
// Fail-closed caps (checked BEFORE any key material is touched):
//   priority_fee_gwei <= MAX_PRIORITY_FEE_GWEI_CAP (50)
//   base_fee_at_creation + 2 * priority_fee_gwei <= MAX_FEE_GWEI_CAP (500)
// Exceeding either cap returns `ExecutionError::SigningFailed` naming
// which cap was exceeded and by how much; it never silently clamps or
// signs anyway. See `envelope_fees_wei` below.
//
// This formula/cap pair is NOT yet a substitute for the broader
// `docs/fee-policy.md` review — it resolves ONLY the formula module
// comment previously marked unconfirmed and adds the caps that document
// discussed in its own §6/§7. Chain scope, refresh-at-sign-time (§5),
// and the profit-vs-gas-cost guard (§7 item 3) remain open and are not
// implemented by this revision.
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
// fully testable today, independent of any gap in this file.
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
//
// ## Build fix (this revision): E0382 partial-move in two tests
//
// `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D
// warnings` both failed to compile with two `E0382` "borrow of partially
// moved value: `err`" errors, both in this module's test suite:
// `sign_transaction_fails_closed_on_zero_chain_id` and
// `resolve_strategy_id_rejects_all_zero_mapping`. Both tests did
// `matches!(err, ExecutionError::SigningFailed { detail } if
// detail.contains(...))` and then referenced `{err:?}` in the assertion
// message on the next line. `SigningFailed { detail }`'s pattern binding
// moves `detail: String` out of `err` (since `String` is not `Copy`), so
// by the time the format string tries to borrow `err` again for `{err:?}`,
// `err` has already been partially moved from and is no longer valid to
// borrow as a whole. Fixed exactly as the compiler suggested: bind
// `ref detail` in each pattern instead of `detail`, so the match inspects
// `err` by reference and never moves out of it, leaving `err` fully
// intact for the subsequent `{err:?}` use.
//
// ## Build fix (this revision, 2): clippy::too_many_arguments on the new
// ## sign_call / sign_call_gwei ZK-envelope helpers
//
// `cargo clippy --workspace --all-targets -- -D warnings` failed with
// two `too_many_arguments` errors (8/7) on `sign_call` and
// `sign_call_gwei`, added this revision to close the ZK gap
// (`OmegaVault.submitProof` is a different call target than
// `OmegaOrchestrator.execute`, so it needs its own gas-paying-envelope
// signer entry point rather than reusing `sign_transaction`, which is
// hardcoded to `self.orchestrator` and to blueprint-shaped calldata).
// Both functions' argument lists are dictated by the same source
// `encode_eip1559_unsigned`/`encode_eip1559_signed` already carry
// `#[allow(clippy::too_many_arguments)]` for — see "Audit fix (carried
// forward, 2)" above: `chain_id`, `nonce`, `to`, `data`, `gas_limit`, and
// the fee pair are the minimum set EIP-1559 itself requires to build an
// unsigned/signed envelope for an arbitrary call, and bundling them into
// a params struct would obscure that direct correspondence rather than
// simplify it, exactly as reasoned there for the RLP helpers. Fixed by
// adding the same `#[allow(clippy::too_many_arguments)]` to both new
// methods, consistent with the existing precedent in this same file
// rather than inventing a different justification.

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

/// Hard ceiling on `priority_fee_gwei` — see module doc, "PROPOSED
/// (2026-08-24), PENDING SIGN-OFF: envelope fee formula". Not final until
/// `docs/fee-policy.md`'s Sign-Off table is completed by Andre Niemand
/// through a durable channel.
const MAX_PRIORITY_FEE_GWEI_CAP: u64 = 50;

/// Hard ceiling on `base_fee_at_creation + 2 * priority_fee_gwei`
/// (i.e. `max_fee_per_gas` expressed in gwei). Same sign-off status as
/// `MAX_PRIORITY_FEE_GWEI_CAP` above.
const MAX_FEE_GWEI_CAP: u64 = 500;

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

    /// Sign an EIP-1559 transaction to an arbitrary `to` with arbitrary calldata.
    ///
    /// Closes the ZK gap: `OmegaVault.submitProof` is not `Orchestrator.execute` —
    /// this is the gas-paying envelope for verified proof broadcast. Fail closed if
    /// no active tx key or empty calldata / zero `to`.
    ///
    /// `#[allow(clippy::too_many_arguments)]`: see this file's top doc
    /// comment, "Build fix (this revision, 2)" — this argument list is
    /// the minimum EIP-1559 itself requires to build an arbitrary-call
    /// envelope, matching the precedent already set by
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

        let secret_key = self
            .tx_key_manager
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
                    "no on-chain strategyId configured for strategy_id {strategy_key:?} — this                      value is real deployment configuration (whatever bytes32 was passed to                      OmegaOrchestrator.registerStrategy() for this strategy on-chain), not                      something derivable from code. Wire it into                      KeyManagerTransactionSigner::new's strategy_onchain_ids map, sourced from                      real deployment records, never guessed at."
                ),
            })?;
        if strategy_id_bytes == [0u8; 32] {
            return Err(ExecutionError::SigningFailed {
                detail: format!(
                    "on-chain strategyId for {strategy_key:?} is all-zero — refusing to sign                      (C6 fail closed)"
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
        // doc comment, "BUG FOUND AND FIXED (2026-08-24)", for why the
        // latter silently produces the wrong bytes (an extra leading
        // offset word) for a struct being decoded as a flat multi-field
        // tuple, which is exactly what OmegaOrchestrator.sol's
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
        // as `build_blueprint_calldata` above (see this file's top doc
        // comment, "BUG FOUND AND FIXED (2026-08-24)"). The contract's
        // own domain hash is `keccak256(abi.encode(address(this),
        // EXPECTED_CHAIN_ID, blueprintCalldata))` — three top-level
        // params, not a struct — so this must be the flat-tuple encoding,
        // not the single-dynamic-value encoding `.abi_encode()` produces.
        omega_security::keccak256(&domain.abi_encode_params())
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

        // Fee policy (proposed, pending sign-off) — see module doc,
        // "PROPOSED (2026-08-24), PENDING SIGN-OFF: envelope fee formula",
        // and `docs/fee-policy.md`. Fails closed, before any RLP is built,
        // if either input is out of policy.
        let (max_priority_fee_per_gas, max_fee_per_gas) =
            envelope_fees_wei(bp.base_fee_at_creation, bp.priority_fee_gwei)?;

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
// RESOLVED (2026-08-25): the "not yet checked against a known
// signed-transaction test vector" gap this file previously flagged here
// is closed. `encode_eip1559_signed_matches_a_real_mainnet_transaction`
// below feeds this exact algorithm a REAL, previously-broadcast Ethereum
// mainnet transaction's decoded fields (found via web search, decoded
// independently via Python's `rlp` library) and confirms it reproduces
// that transaction's real raw bytes — not spec prose, not another
// library's synthetic construction, but bytes a real node actually
// accepted onto mainnet. `encode_eip1559_long_form_rlp_paths_match_
// independent_implementation` separately closes a gap NEITHER the real
// transaction test nor any prior test in this file covered: RLP's
// long-form length-prefix branches (byte strings/lists >= 56 bytes,
// `rlp_bytes`'/`rlp_list`'s `else` arms), the most bug-prone part of a
// hand-written RLP encoder — verified against Python's `rlp` library on
// a 100-byte-calldata synthetic case, since the real mainnet transaction
// found happened to have 68-byte calldata (itself already past the
// 56-byte threshold, so it also exercises this path, but the synthetic
// case makes the coverage explicit and independent of what a single
// found transaction happens to contain).
//
// VERIFICATION METHOD, stated plainly: this crate could not be compiled
// directly in the sandbox that did this verification (its current
// alloy-primitives pin has transitive deps requiring a newer Rust
// edition than was available there). The actual function bodies below
// were extracted verbatim into a standalone, dependency-minimal Rust
// program (only a trivial `Address`/`U256` byte-layout stand-in
// swapped in — see that program's own header for exactly what was and
// wasn't changed), compiled and run for real, and cross-checked against
// Python's independent `rlp` library. This is the same "second,
// independent, spec-compliant implementation" strategy already used to
// verify the ABI-encoding fix earlier in this file's history, applied
// here to RLP instead of ABI encoding.
//
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

    #[test]
    fn build_blueprint_calldata_matches_solc_golden_vector() {
        // Fixture MUST match contracts/test/BlueprintCalldataAbi.t.sol constants.
        let km = make_km(0x20);
        let mut ids = HashMap::new();
        ids.insert(
            "SA".into(),
            {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(
                    &hex::decode("c4bb1c851b1c74593f61f8d1f99ec07e2960d847a94d4a736e321ba387d4d2d7")
                        .expect("valid strategy id hex"),
                );
                arr
            },
        );
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
    // See this file's top doc comment, item 4 (RESOLVED 2026-08-25), for the
    // full evidence chain: these five values are transcribed here PURELY for
    // a verification cross-check, not as production configuration — the
    // actual production map lives in crates/omega-engine/src/main.rs's own
    // `strategy_onchain_ids()`, sourced from contracts/src/StrategyIds.sol.
    // This function is never called by KeyManagerTransactionSigner itself.
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
        // The real teeth of this test: StrategyIds.sol documents each
        // constant with a `/// @dev keccak256("OMEGA_STRATEGY_<X>")`
        // comment. This recomputes that hash independently — using
        // omega_security::keccak256, the SAME primitive compute_bp_hash
        // above uses elsewhere in this file — and confirms each of the
        // five transcribed constants actually equals what its own source
        // comment claims. This is strictly stronger than comparing this
        // list against itself: it verifies the values against their
        // documented DERIVATION, not just against another transcription of
        // the same numbers. (Cross-checked once already, outside this
        // codebase, via a second, independent keccak256 implementation —
        // Python's Crypto.Hash.keccak — with an identical result for all
        // five; this test makes that verification permanent and
        // repeatable via `cargo test`, rather than a one-off check.)
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
        let expected_sa = hex::decode(
            "c4bb1c851b1c74593f61f8d1f99ec07e2960d847a94d4a736e321ba387d4d2d7",
        )
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

    // ── sign_transaction currently fails loudly, not silently ──────────────

    #[tokio::test]
    async fn sign_transaction_succeeds_end_to_end_with_real_blueprint_signer() {
        // Regression guard: with a real strategy_onchain_ids entry and a
        // real BlueprintSigner, and with sample_bp()'s fees within the
        // proposed caps (base=10, priority=5), signing must actually
        // succeed end-to-end. If this starts failing, either the ABI
        // encoding, the domain hash, the BlueprintSigner wiring, or the
        // fee-cap thresholds regressed.
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
        // Distinguishes one of the failure reasons this signer can still
        // produce: an unconfigured strategy_onchain_ids entry fails
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
        // Self-consistency check, complementary to (not a substitute
        // for) `build_blueprint_calldata_matches_solc_golden_vector`
        // above: decode what we just encoded via alloy-sol-types' own
        // abi_decode_params, and confirm every field survives the round
        // trip. This validates every field was assigned to the position
        // intended, using the same encode/decode pair, so a
        // self-consistent-but-wrong encoding (e.g. the 2026-08-24
        // abi_encode()-vs-abi_encode_params() bug — see this file's top
        // doc comment) would NOT have been caught by this test alone,
        // since both sides would have shared the same bug. It was only
        // caught by comparing against an independent solc/EVM oracle,
        // which is what the golden-vector test above does and this test
        // does not.
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
        // abi_encode_params — see this file's top doc comment, "BUG
        // FOUND AND FIXED (2026-08-24)". Using the mismatched
        // Blueprint::abi_decode (the single-dynamic-value form) here
        // would decode the flat-tuple bytes against the wrong head
        // shape rather than catching an encode/decode mismatch.
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
        // abi_decode_params — see the round-trip test above and this
        // file's top doc comment, "BUG FOUND AND FIXED (2026-08-24)".
        let decoded = Blueprint::abi_decode_params(&encoded, true).unwrap();
        let expected =
            U256::from(bp.max_base_fee_gwei).saturating_mul(U256::from(1_000_000_000u64));
        assert_eq!(decoded.max_base_fee, expected);
    }

    // ── compute_bp_hash — domain separation ─────────────────────────────────

    #[test]
    fn compute_bp_hash_matches_solc_golden_vector() {
        // Fixture MUST match contracts/test/DomainSeparatedBlueprintHash.t.sol's
        // ORCHESTRATOR / CHAIN_ID / BLUEPRINT_CALLDATA constants. Closes
        // the gap this file's top doc comment, "BUG FOUND AND FIXED
        // (2026-08-24)", flagged in its CAVEAT paragraph: until this
        // test existed, `compute_bp_hash`'s `abi_encode_params()` fix was
        // inspection-resolved by analogy to `build_blueprint_calldata`'s
        // confirmed bug, not independently verified against a real
        // solc/EVM oracle the way `build_blueprint_calldata_matches_
        // solc_golden_vector` verifies `build_blueprint_calldata`.
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
        // contracts/test/DomainSeparatedBlueprintHash.t.sol, which
        // printed this exact value — an EXACT match to the value
        // originally seeded here via an independent Python (eth_abi)
        // computation, so no update was needed once forge confirmed it.
        // Closes the CAVEAT this file's top doc comment previously
        // tracked for compute_bp_hash's abi_encode_params() fix.
        let solc_golden: [u8; 32] = hex::decode(
            "74d1c22598dab6e5f1cb1a1809d3b7255728a0c10c971a9f05257cd6758b356b",
        )
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
        // base + 2*priority > 500, with priority still <= 50
        // e.g. base=401, priority=50 -> max_fee_gwei = 501
        let err = envelope_fees_wei(401, 50).unwrap_err();
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
        // priority == 50, max_fee_gwei == 500
        let (tip, max_fee) = envelope_fees_wei(400, 50).unwrap();
        assert_eq!(tip, U256::from(50u64) * U256::from(GWEI_TO_WEI));
        assert_eq!(max_fee, U256::from(500u64) * U256::from(GWEI_TO_WEI));
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
        // transaction, not a synthetic case. Found via web search (a
        // github.com/ethers-io/ethers.js discussion where someone pasted
        // this raw tx while debugging RLP decoding — it calls USDT's
        // transfer(), selector 0xa9059cbb). Its fields were decoded
        // independently via Python's `rlp` library, then fed through a
        // standalone extraction of THIS FILE's own encode_eip1559_signed
        // (same function body, compiled and run for real, outside this
        // crate's own alloy-primitives version constraints — see this
        // file's top doc comment's "EIP-1559 RLP encoding helpers"
        // section for the verification history), and confirmed to
        // reproduce these exact bytes.
        //
        // This closes the specific gap flagged in this file's own module
        // doc comment history: the RLP encoder had only ever been
        // checked against the EIP-1559 spec's prose (structural checks
        // like "starts with 0x02," "empty list is 0xc0") — never against
        // bytes a real Ethereum node actually accepted. This is that
        // check: not "matches spec text," not "matches another library's
        // synthetic construction," but "matches bytes mined into a real
        // Ethereum block."
        //
        // Also independently exercises the LONG-FORM RLP paths (see
        // rlp_bytes'/rlp_list's `else` branches) via this transaction's
        // 68-byte calldata — the most bug-prone part of a hand-written
        // RLP encoder, and NOT exercised by any of this file's other
        // encode_eip1559_* tests, which all use short (<56-byte) data.
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
        let real_r = hex::decode("236084da36000fb2c7373cfa78e8f1bc9d8eb081dc240630c8024aa06fc39f96")
            .expect("valid real tx r hex");
        let real_s = hex::decode("30bdc5cd4e1f5f6abbb36c3b004270b68724cc46c56ad5847c99f8ced9c4112d")
            .expect("valid real tx s hex");

        let signed = encode_eip1559_signed(
            1,      // chain_id — Ethereum mainnet
            86964,  // nonce
            U256::from(1_000_000_000u64),  // max_priority_fee_per_gas
            U256::from(34_154_125_362u64), // max_fee_per_gas
            120_000,                        // gas_limit
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
        // long-form branch (0xf7+len_of_len). Verified this session
        // (standalone extraction) against Python's independent `rlp`
        // library — byte-for-byte match, including both long-form
        // prefixes (confirmed present via the 0xf8/0xb8 bytes in the
        // expected hex below). Kept here as a permanent regression test
        // for the same case, rather than only a one-off check.
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