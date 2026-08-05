// crates/omega-core/src/types/blueprint.rs
//
// ExecutionBlueprint — the central data structure of the Omega Engine.
//
// A blueprint is an immutable, fully-specified execution intent.  Once
// constructed by a strategy scorer (§StrategyTrait::build_blueprint) it
// flows unchanged through simulation, ZK commitment, relay submission
// and loss attribution.  No field is mutated after construction; any
// re-pricing or re-scoring produces a *new* blueprint with a fresh hash.
//
// ## This revision
//
// Added `max_base_fee_gwei` (gas model group) — the off-chain half of
// OmegaOrchestrator's new on-chain `maxBaseFee` guard (Solidity
// `BaseFeeTooHigh` revert in `execute()`). Set via
// `derive_max_base_fee_gwei()` from `base_fee_at_creation` plus a
// buffer factor, same shape as `l2_buffer_factor`/`l1_data_buffer_factor`.
// Included in `compute_hash()`'s input set (fixed at construction, like
// its sibling gas-model fields); excluded from `compute_idempotency_key()`
// (a risk parameter, not economic trade identity — same treatment as
// `dynamic_min_profit`). See `max_base_fee_wei()` for the required
// gwei→wei conversion before ABI-encoding into `blueprintCalldata` — the
// Solidity side compares directly against `block.basefee`, which is
// wei-denominated.
//
// NOTE ON THIS FILE'S PRIOR AUDIT-FINDINGS HISTORY: this revision only
// shows the diff for the change above; the numbered "Audit findings
// fixed in this pass" history from earlier revisions of this file
// (boundary-disagreement fix, gas-budget rounding, hash-integrity
// helpers, idempotency-key fields, dead-code removal, idempotency-key/
// client_order_id fix) still applies and isn't restated here — nothing
// in this change touches or reverts any of it.
//
// Spec references:
//   §1.1  — phases and StrategyId mapping
//   §7    — Arbitrum dual-component gas model fields
//   §8    — OFA compliance flag
//   §11   — LA-specific fields (flashloan, confirmation depth)
//   §12   — Gas War Engine fields (priority_fee_gwei, relay_targets)
//   §13   — Loss Attribution hook (blueprint_hash is the join key)
//   §15   — Vault ZK gate (zk_proof_commitment)

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::lane::{Lane, Simulator};

// ─────────────────────────────────────────────────────────────────────────────
// StrategyId
// ─────────────────────────────────────────────────────────────────────────────

/// Identifies which strategy produced a blueprint (§1.1).
///
/// Priority ordering (lower number = higher precedence in slot competition):
///   MEV(0) > LA(1) > MSA(2) > SA(3) > CNRY(255)
///
/// CNRY (Canary) never competes for execution slots — it is a signal
/// validator with zero capital deployment (Phase 0.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StrategyId {
    /// Simple Arbitrage — Phase 1.
    Sa,
    /// Canary — Phase 0.5.  Signal validator, no capital deployed.
    Cnry,
    /// Multi-Step Arbitrage — Phase 2.
    Msa,
    /// Liquidation Arbitrage — Phase 3 (Aave/Compound/Morpho/Euler v2).
    La,
    /// MEV-OFA / Backrun — Phase 4.
    Mev,
}

impl StrategyId {
    /// Slot competition priority.  Lower = higher precedence.
    /// CNRY is 255 — it never displaces a live strategy.
    #[inline]
    pub fn priority(self) -> u8 {
        match self {
            StrategyId::Mev => 0,
            StrategyId::La => 1,
            StrategyId::Msa => 2,
            StrategyId::Sa => 3,
            StrategyId::Cnry => 255,
        }
    }

    /// Minimum system phase required before this strategy may activate
    /// (§1.1, §20).
    #[inline]
    pub fn phase_required(self) -> u8 {
        match self {
            StrategyId::Cnry => 0,
            StrategyId::Sa => 1,
            StrategyId::Msa => 2,
            StrategyId::La => 3,
            StrategyId::Mev => 4,
        }
    }

    /// Returns `true` for the Canary strategy — used as a gate in
    /// several hot-path checks to avoid canary blueprints reaching
    /// relay submission.
    #[inline]
    pub fn is_canary(self) -> bool {
        self == StrategyId::Cnry
    }
}

impl std::fmt::Display for StrategyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StrategyId::Sa => f.write_str("SA"),
            StrategyId::Cnry => f.write_str("CNRY"),
            StrategyId::Msa => f.write_str("MSA"),
            StrategyId::La => f.write_str("LA"),
            StrategyId::Mev => f.write_str("MEV"),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ExecutionBlueprint
// ─────────────────────────────────────────────────────────────────────────────

/// Fully-specified, immutable execution intent.
///
/// Produced by [`crate::types::strategy::StrategyTrait::build_blueprint`]
/// and consumed — without mutation — by simulation, relay submission,
/// the ZK vault gate, and loss attribution.
///
/// ## Field groups
///
/// | Group          | Fields                                              |
/// |----------------|-----------------------------------------------------|
/// | Identity       | blueprint_hash, chain_id, strategy_id, lane, simulator |
/// | Signal binding | signal_state_hash, state_version, signal_id         |
/// | Execution      | flashloan_*, calldata, strategy_bytecode_hash      |
/// | Gas model (§7) | l2_exec_gas_estimate, l1_data_gas_estimate, …      |
/// | Economics      | expected_profit_net, dynamic_min_profit, slippage_bps |
/// | Timing         | expiry_block, nonce, confirmation_depth, client_order_id |
/// | Submission     | idempotency_key                                     |
/// | Relay          | relay_targets                                       |
/// | ZK             | zk_proof_commitment                                 |
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionBlueprint {
    // ── Identity ─────────────────────────────────────────────────────────
    /// keccak256 of the canonical serialised blueprint fields (all fields
    /// except `blueprint_hash` itself).  Acts as the join key in Loss
    /// Attribution (§13) and the ZK vault proof input (§15).
    ///
    /// Nothing in the type system currently prevents this from
    /// desyncing from the blueprint's actual field values if a field is
    /// mutated after construction (every field here is `pub`). Call
    /// `verify_hash()` (or `verify_hash_detailed()`) at any trust
    /// boundary to catch that.
    pub blueprint_hash: B256,

    /// EIP-155 chain ID.  1 = Ethereum mainnet, 42161 = Arbitrum One.
    pub chain_id: u64,

    /// Strategy that produced this blueprint (§1.1).
    pub strategy_id: StrategyId,

    /// Execution lane — determines simulation backend (§4, §11.1).
    pub lane: Lane,

    /// Simulation backend used when this blueprint was constructed.
    /// Recorded for loss attribution (§13.4).
    pub simulator: Simulator,

    // ── Signal binding ───────────────────────────────────────────────────
    /// keccak256 of the signal/oracle state snapshot this blueprint was
    /// built against.  Validated by the EIL double-buffer before
    /// simulation (§6).  Mismatch → SIMULATION_STATE_MISMATCH (§13.4).
    pub signal_state_hash: B256,

    /// Monotonically increasing snapshot version from the oracle layer.
    /// Used alongside `signal_state_hash` to detect stale state (§6).
    pub state_version: u64,

    /// Unique identifier for this specific signal generation / scorer
    /// invocation that produced this blueprint. Distinct from
    /// `blueprint_hash` (a content hash of the trade itself):
    /// `signal_id` identifies WHICH run of the scorer produced it, so
    /// two blueprints with identical economic content but generated by
    /// two separate scorer invocations (e.g. a duplicated task) are
    /// still distinguishable for provenance/debugging, even though
    /// their `idempotency_key` correctly treats them as the same trade
    /// (see `compute_idempotency_key()`, and finding 6 in this file's
    /// module doc comment for why `signal_id` must NOT leak into that
    /// key via `client_order_id`).
    pub signal_id: Uuid,

    // ── Execution ────────────────────────────────────────────────────────
    /// Flashloan provider contract address (Aave v3, Balancer, etc.).
    /// Zero address signals no flashloan — capital sourced from PIL (§7).
    pub flashloan_provider: Address,

    /// Requested flashloan principal in wei.  Must be ≤ flashloan_available.
    pub flashloan_amount: U256,

    /// Maximum flashloan liquidity available from the provider at
    /// signal snapshot time.  Used to gate blueprint construction —
    /// never submit a blueprint where flashloan_amount > flashloan_available.
    pub flashloan_available: U256,

    /// Fully-encoded call to the strategy contract.  Includes flashloan
    /// callback data, swap routing, and profit extraction.
    pub calldata: Bytes,

    /// keccak256 of the deployed strategy contract's runtime bytecode.
    /// Validated by the StrategyRegistry before simulation to detect
    /// stale or incorrect deployments (§8).
    pub strategy_bytecode_hash: B256,

    // ── Gas model — Arbitrum dual-component (§7) ─────────────────────────
    /// Estimated L2 execution gas units.  Does NOT include L1 data cost.
    /// Multiplied by l2_buffer_factor before fee calculation.
    pub l2_exec_gas_estimate: u64,

    /// Estimated L1 data gas units (calldata bytes × 16).  Priced
    /// separately against the L1 data fee oracle.
    pub l1_data_gas_estimate: u64,

    /// Gas reserved for profit-extraction transfer (Vault safeTransfer).
    /// Added to l2_exec_gas_estimate for total L2 gas budget.
    pub extraction_gas: u64,

    /// Net profit expected after ALL costs: L2 gas, L1 data fee, flashloan
    /// premium, and slippage.  Blueprint is rejected if this is below
    /// `dynamic_min_profit`.
    pub expected_profit_net: U256,

    /// Dynamic minimum profit threshold at blueprint construction time
    /// (§7).  Accounts for current base fee volatility and competition
    /// probability.  Emergency bundle check uses this field (§12.1).
    pub dynamic_min_profit: U256,

    /// Multiplier applied to l2_exec_gas_estimate (e.g. 1.15 = 15% buffer).
    /// Tuned by the Gas War Engine ML model (§13).
    pub l2_buffer_factor: f64,

    /// Multiplier applied to l1_data_gas_estimate (§7).
    pub l1_data_buffer_factor: f64,

    /// Maximum acceptable slippage in basis points (100 bps = 1%).
    pub slippage_bps: u16,

    /// EIP-1559 base fee (gwei) at blueprint construction time.
    /// Used by Loss Attribution to correlate fee conditions with
    /// LOST_GAS_LOW events (§13).
    pub base_fee_at_creation: u64,

    /// L1 data fee oracle reading (gwei) at blueprint construction time.
    pub l1_data_fee_at_creation: u64,

    /// Priority fee (tip) in gwei submitted to the Arbitrum sequencer.
    /// Bounded by the Gas War Engine cap (§12.2).
    /// NOTE: On Arbitrum this is near-zero ETH cost despite large gwei
    /// values — see §12.2 for the ETH cost analysis.
    pub priority_fee_gwei: u64,

    /// Signer's ceiling on `block.basefee` (gwei), enforced on-chain by
    /// `OmegaOrchestrator.execute()` as a last-line-of-defense against
    /// this blueprint landing after network fee conditions have spiked
    /// past what was priced in when it was signed. Set from
    /// `base_fee_at_creation` plus a margin at construction time — see
    /// `derive_max_base_fee_gwei()`.
    ///
    /// UNITS: stored here in gwei, same as `base_fee_at_creation` and
    /// `priority_fee_gwei`, for consistency with the rest of this gas
    /// model group. The Solidity side compares against `block.basefee`,
    /// which is denominated in WEI — whatever code ABI-encodes this
    /// blueprint into `blueprintCalldata` for `execute()` MUST convert
    /// via `max_base_fee_wei()` below, not pass this field's raw value
    /// directly. Passing the gwei value straight through would silently
    /// set an on-chain ceiling roughly 1e9× too low, making the guard
    /// revert on every single execution.
    ///
    /// NOT a substitute for `dynamic_min_profit` — this bounds only the
    /// base fee component, not priority fee/tips, L1 data fees, or blob
    /// fees, none of which are observable on-chain the same simple way
    /// across every target chain. The real gas/profitability engine
    /// (this field's own buffer factor, `dynamic_min_profit`, and the
    /// `omega-gas-war` simulation checks) is still what's responsible
    /// for pricing blueprints correctly before signing; this field is
    /// only a safety net against drift between signing and inclusion.
    pub max_base_fee_gwei: u64,

    /// Price impact of the required swaps in basis points.  None when
    /// the strategy does not perform AMM swaps (e.g. pure liquidations
    /// with external collateral).
    pub price_impact_bps: Option<u16>,

    /// OFA (Order Flow Agreement) compliance flag (§8).
    /// When true the bundle must be routed only through OFA-compliant
    /// relays and must not be included in non-OFA blocks.
    pub ofa_compliant: bool,

    // ── Timing ───────────────────────────────────────────────────────────
    /// Block number after which this blueprint MUST NOT be submitted.
    /// The relay layer enforces this hard deadline.
    pub expiry_block: u64,

    /// Per-strategy monotonic nonce.  Prevents replay of expired
    /// blueprints.  See `nonce_key()` for the derivation.
    pub nonce: u64,

    /// Required on-chain confirmation depth before the Vault releases
    /// profit (§15).  Minimum 12 (Vault contract enforces this).
    pub confirmation_depth: u8,

    /// Exchange/relay-side client order ID for reconciliation — a
    /// human-and-log-readable identifier distinct from the raw
    /// `blueprint_hash`, intended for cross-referencing this blueprint
    /// against relay/exchange-side records after submission. See
    /// `derive_client_order_id()` for the canonical derivation; this
    /// field is set once at construction and never recomputed.
    ///
    /// NOTE: this is included in `compute_hash()`'s input set (it's a
    /// fixed, construction-time identity field), but deliberately
    /// EXCLUDED from `compute_idempotency_key()` — it embeds
    /// `signal_id`, which varies between independent scorer
    /// invocations of the same trade; see finding 6 in this file's
    /// module doc comment.
    pub client_order_id: String,

    // ── Submission ───────────────────────────────────────────────────────
    /// Cryptographic idempotency key used by the submission layer to
    /// detect a duplicate blueprint before it is ever signed — distinct
    /// from (and complementary to) `omega_rpc::client::SubmissionTracker`,
    /// which dedups already-SIGNED transaction hashes. See
    /// `compute_idempotency_key()` for the exact field set and finding 6
    /// in this file's module doc comment for why `client_order_id` is
    /// NOT part of it. Call `verify_idempotency_key()` (or
    /// `verify_idempotency_key_detailed()`) at the submission boundary.
    pub idempotency_key: B256,

    // ── Relay ────────────────────────────────────────────────────────────
    /// Ordered list of relay endpoint identifiers.  Populated by the
    /// Gas War Engine using LA-inclusion-rate ranking (§11.2).
    /// Must have at least one entry; may have up to 4 (cascade mode).
    ///
    /// NOTE: populated AFTER blueprint_hash is committed — deliberately
    /// excluded from `compute_hash()`'s input set (see that method).
    pub relay_targets: Vec<String>,

    // ── ZK ───────────────────────────────────────────────────────────────
    /// Commitment to the StarkProof that gates Vault profit release (§15).
    /// None during blueprint construction; set by the ZK layer before
    /// relay submission when ZK is required for this strategy.
    ///
    /// NOTE: populated AFTER blueprint_hash is committed — deliberately
    /// excluded from `compute_hash()`'s input set (see that method).
    pub zk_proof_commitment: Option<B256>,
}

/// Diagnostic detail for a failed `verify_hash()` check — carries the
/// blueprint's stored hash alongside what `compute_hash()` derives from
/// its current field values, so a caller can log both instead of just
/// a bare `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HashMismatch {
    /// The blueprint's stored `blueprint_hash`.
    pub expected: B256,
    /// What `compute_hash()` derives from the current field values.
    pub actual: B256,
}

impl std::fmt::Display for HashMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "blueprint_hash mismatch: stored {:#x}, recomputed {:#x}",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for HashMismatch {}

/// Diagnostic detail for a failed `verify_idempotency_key()` check —
/// same shape as `HashMismatch`, for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdempotencyKeyMismatch {
    /// The blueprint's stored `idempotency_key`.
    pub expected: B256,
    /// What `compute_idempotency_key()` derives from the current field values.
    pub actual: B256,
}

impl std::fmt::Display for IdempotencyKeyMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "idempotency_key mismatch: stored {:#x}, recomputed {:#x}",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for IdempotencyKeyMismatch {}

impl ExecutionBlueprint {
    /// Derives the nonce namespace key for a (strategy, chain) pair.
    ///
    /// Used by nonce managers in omega-strategies to ensure each
    /// (strategy_id, chain_id) combination has an independent nonce
    /// sequence — preventing cross-strategy replay.
    pub fn nonce_key(strategy_id: StrategyId, chain_id: u64) -> B256 {
        // 32-byte hash + 8-byte chain_id = 40 bytes (previously
        // under-allocated at 36; harmless — Vec reallocates
        // automatically — but corrected since it's a one-line fix).
        let mut buf = Vec::with_capacity(40);
        // Stable strategy discriminant — format!("{strategy_id}") is
        // used rather than the enum index to survive enum reordering.
        buf.extend_from_slice(keccak256(strategy_id.to_string().as_bytes()).as_slice());
        buf.extend_from_slice(&chain_id.to_be_bytes());
        keccak256(&buf)
    }

    /// Derives the canonical `client_order_id` for a blueprint from its
    /// identity fields. Deterministic and human-readable, distinct from
    /// `blueprint_hash` (a raw content hash) — intended for grep-ability
    /// in logs and for cross-referencing against relay/exchange-side
    /// records.
    ///
    /// Includes `chain_id` alongside `strategy_id`/`nonce`/`signal_id`
    /// because nonces are scoped per (strategy_id, chain_id) pair (see
    /// `nonce_key`) — omitting chain_id here would let two blueprints on
    /// different chains with the same strategy+nonce collide on the
    /// same client_order_id string.
    ///
    /// NOTE: this ID embeds `signal_id`, which is exactly why it must
    /// NOT be fed into `compute_idempotency_key()` — see finding 6 in
    /// this file's module doc comment.
    pub fn derive_client_order_id(
        strategy_id: StrategyId,
        chain_id: u64,
        nonce: u64,
        signal_id: Uuid,
    ) -> String {
        format!("omega-{strategy_id}-{chain_id}-{nonce}-{signal_id}")
    }

    /// Returns `true` when this is a Canary blueprint.
    ///
    /// Canary blueprints MUST NOT be submitted to relays or interact
    /// with the Vault.  Call sites use this guard before any submission
    /// path.
    #[inline]
    pub fn is_canary(&self) -> bool {
        self.strategy_id.is_canary()
    }

    /// Selects the simulation backend for a given (lane, gas_estimate)
    /// pair (§4, §11).
    ///
    /// Rule: Microtx lane + gas < 200,000 → revm (in-process, zero-copy).
    /// All other combinations → Anvil (full fork).
    #[inline]
    pub fn select_simulator(lane: Lane, gas_estimate: u64) -> Simulator {
        if lane == Lane::Microtx && gas_estimate < 200_000 {
            Simulator::Revm
        } else {
            Simulator::Anvil
        }
    }

    /// Returns the total L2 gas budget including extraction overhead.
    ///
    /// total = ceil(l2_exec_gas_estimate × l2_buffer_factor) + extraction_gas
    ///
    /// Used by the Gas War Engine fee cap calculation (§12).
    ///
    /// Rounds UP (`.ceil()`), not truncates: this is a gas BUDGET (a cap
    /// the downstream fee calculation must not underestimate), so any
    /// fractional gas from the buffer multiplication must round up, not
    /// down — same reasoning as
    /// `omega_risk::gas_model::dynamic_min_profit`'s cost-floor rounding
    /// elsewhere in this codebase. Truncating here would silently
    /// under-budget gas by up to one unit's worth of fractional
    /// remainder on every single blueprint.
    #[inline]
    pub fn total_l2_gas_budget(&self) -> u64 {
        let buffered = (self.l2_exec_gas_estimate as f64 * self.l2_buffer_factor).ceil() as u64;
        buffered.saturating_add(self.extraction_gas)
    }

    /// Derives `max_base_fee_gwei` from a base-fee observation at
    /// blueprint-construction time, using the same
    /// margin-above-observed-value shape as `l2_buffer_factor` /
    /// `l1_data_buffer_factor` elsewhere in this gas model.
    ///
    /// `buffer_factor` is a multiplier, e.g. `1.5` for 50% headroom
    /// above `base_fee_at_creation` — chosen by the caller (typically
    /// the same Gas War Engine tuning that sets the other buffer
    /// factors) based on how much base-fee drift the strategy's
    /// expected time-to-inclusion can tolerate before profitability is
    /// actually at risk. Rounds UP (`.ceil()`), same reasoning as
    /// `total_l2_gas_budget()`: this produces a CEILING the on-chain
    /// guard must not underestimate, so any fractional gwei from the
    /// multiplication must round up, not truncate down.
    ///
    /// Does not itself set the field — callers assign the result to
    /// `max_base_fee_gwei` when constructing the blueprint.
    #[inline]
    pub fn derive_max_base_fee_gwei(base_fee_at_creation: u64, buffer_factor: f64) -> u64 {
        (base_fee_at_creation as f64 * buffer_factor).ceil() as u64
    }

    /// Converts `max_base_fee_gwei` to wei, for encoding into the
    /// `maxBaseFee` field of `blueprintCalldata` before calling
    /// `OmegaOrchestrator.execute()`.
    ///
    /// REQUIRED at the ABI-encoding boundary: the Solidity contract
    /// compares this value directly against `block.basefee`, which is
    /// wei-denominated. Encoding `max_base_fee_gwei` unconverted would
    /// set an on-chain ceiling ~1e9× too low and cause `execute()` to
    /// revert with `BaseFeeTooHigh` on every call, not just ones that
    /// are actually experiencing a fee spike.
    #[inline]
    pub fn max_base_fee_wei(&self) -> U256 {
        U256::from(self.max_base_fee_gwei) * U256::from(1_000_000_000u64)
    }

    /// Returns `true` when `expected_profit_net` meets or exceeds
    /// `dynamic_min_profit`.
    ///
    /// This is the primary profitability gate checked before relay
    /// submission and before emergency bundle emission (§12.1).
    ///
    /// CONSISTENCY NOTE: uses `>=`, matching the authoritative pre-trade
    /// gate `omega_risk::checks::check_dynamic_profit` (check 5 of 13),
    /// which rejects only `expected_profit_net_wei < dynamic_min_profit_wei`
    /// — i.e. treats an exact tie as profitable. This method previously
    /// used strict `>`, which meant a blueprint sitting exactly at its
    /// minimum profit threshold would read as "not profitable" here
    /// while simultaneously PASSING the actual enforced check in
    /// omega-risk. This is a convenience method, not a substitute for
    /// the full 13-check omega-risk pipeline — it's aligned to that
    /// pipeline's boundary semantics specifically so the two can never
    /// silently disagree again.
    #[inline]
    pub fn is_profitable(&self) -> bool {
        self.expected_profit_net >= self.dynamic_min_profit
    }

    /// Returns `true` when this blueprint has reached or passed its
    /// expiry block.
    ///
    /// CONSISTENCY NOTE: uses `>=`, matching the authoritative pre-trade
    /// gate `omega_risk::checks::check_expiry` (check 2 of 13), which
    /// rejects when `current_block >= bp.expiry_block`. This method
    /// previously used strict `>`, meaning a blueprint sitting exactly
    /// AT its expiry block would read as "not expired" here while the
    /// actually-enforced check would already reject it — a blueprint
    /// could pass this convenience check while the real gate silently
    /// drops it, or vice versa if some path trusted only this method.
    #[inline]
    pub fn is_expired(&self, current_block: u64) -> bool {
        current_block >= self.expiry_block
    }

    /// Returns `true` when `flashloan_amount` is within available
    /// liquidity, as of the flashloan snapshot recorded on this
    /// blueprint.
    ///
    /// IMPORTANT: this is a raw feasibility check only — `amount <=
    /// available`, no safety margin. It is NOT a substitute for the
    /// full pre-trade `omega_risk::checks::check_flashloan_liquidity`,
    /// which additionally requires `available >= amount × 1.20` (a 20%
    /// safety margin, since a razor-thin margin can be consumed by a
    /// competing transaction in the same block) and enforces the
    /// no-self-flash rule (flashloan provider ≠ liquidation target
    /// protocol). Do not treat `flashloan_feasible() == true` as
    /// sufficient grounds to proceed with execution on its own.
    #[inline]
    pub fn flashloan_feasible(&self) -> bool {
        self.flashloan_amount <= self.flashloan_available
    }

    /// Deterministically recomputes what `blueprint_hash` should be
    /// from every field fixed at construction time — everything except
    /// `blueprint_hash` itself (self-referential), `idempotency_key`
    /// (a separately-derived value for a distinct consumer — see this
    /// file's finding 4 module doc comment), and the two fields
    /// populated in later pipeline stages, `relay_targets` (Gas War
    /// Engine, §12) and `zk_proof_commitment` (ZK layer, §15).
    ///
    /// This exists because every field on this struct is `pub`, so
    /// nothing in the type system stops a caller from mutating a field
    /// after construction — silently desyncing `blueprint_hash` from
    /// the blueprint's actual contents. Call `verify_hash()` at any
    /// trust boundary (before simulation, before relay submission) to
    /// catch that.
    ///
    /// ## Fields covered
    ///
    /// `chain_id`, `strategy_id`, `lane`, `simulator`,
    /// `signal_state_hash`, `state_version`, `signal_id`,
    /// `flashloan_provider`, `flashloan_amount`, `flashloan_available`,
    /// `calldata`, `strategy_bytecode_hash`, `l2_exec_gas_estimate`,
    /// `l1_data_gas_estimate`, `extraction_gas`, `expected_profit_net`,
    /// `dynamic_min_profit`, `l2_buffer_factor`, `l1_data_buffer_factor`,
    /// `slippage_bps`, `base_fee_at_creation`, `l1_data_fee_at_creation`,
    /// `priority_fee_gwei`, `max_base_fee_gwei`, `price_impact_bps`,
    /// `ofa_compliant`, `expiry_block`, `nonce`, `confirmation_depth`,
    /// `client_order_id`.
    ///
    /// ## Encoding
    ///
    /// Fields are concatenated in a fixed, explicitly-specified byte
    /// layout (big-endian integers, raw fixed-width bytes for
    /// B256/Address/U256, IEEE-754 bit patterns for the two f64 buffer
    /// factors, raw UTF-8 bytes for String/Uuid-as-string) and hashed
    /// with keccak256 — deliberately NOT delegated to `bincode` or
    /// another general-purpose serializer, since this value is also
    /// used as a ZK vault proof input (§15) and a hash commitment's
    /// byte layout needs to be self-specified and stable, not dependent
    /// on a third-party crate's internal wire format (which is not a
    /// byte-for-byte stability guarantee the way a commitment scheme
    /// needs).
    ///
    /// ## Integration requirement
    ///
    /// Whatever code originally computes and sets `blueprint_hash` in
    /// `StrategyTrait::build_blueprint` implementations (omega-strategies
    /// — not visible from omega-core) MUST use this exact same field
    /// set and encoding, or `verify_hash()` will report every
    /// legitimately-constructed blueprint as tampered. This is the
    /// reference encoding; align the real construction path to it (or
    /// this method to the real one) as a follow-up — omega-core cannot
    /// see that code to confirm which direction the alignment needs to
    /// go.
    pub fn compute_hash(&self) -> B256 {
        let mut buf = Vec::with_capacity(640);
        buf.extend_from_slice(&self.chain_id.to_be_bytes());
        buf.extend_from_slice(self.strategy_id.to_string().as_bytes());
        buf.push(match self.lane {
            Lane::Microtx => 0u8,
            Lane::Normal => 1u8,
        });
        buf.push(match self.simulator {
            Simulator::Revm => 0u8,
            Simulator::Anvil => 1u8,
        });
        buf.extend_from_slice(self.signal_state_hash.as_slice());
        buf.extend_from_slice(&self.state_version.to_be_bytes());
        buf.extend_from_slice(self.signal_id.as_bytes());
        buf.extend_from_slice(self.flashloan_provider.as_slice());
        buf.extend_from_slice(&self.flashloan_amount.to_be_bytes::<32>());
        buf.extend_from_slice(&self.flashloan_available.to_be_bytes::<32>());
        buf.extend_from_slice(&self.calldata);
        buf.extend_from_slice(self.strategy_bytecode_hash.as_slice());
        buf.extend_from_slice(&self.l2_exec_gas_estimate.to_be_bytes());
        buf.extend_from_slice(&self.l1_data_gas_estimate.to_be_bytes());
        buf.extend_from_slice(&self.extraction_gas.to_be_bytes());
        buf.extend_from_slice(&self.expected_profit_net.to_be_bytes::<32>());
        buf.extend_from_slice(&self.dynamic_min_profit.to_be_bytes::<32>());
        // f64 bit pattern, not the float value itself — deterministic,
        // avoids any float-formatting ambiguity in a hash input.
        buf.extend_from_slice(&self.l2_buffer_factor.to_bits().to_be_bytes());
        buf.extend_from_slice(&self.l1_data_buffer_factor.to_bits().to_be_bytes());
        buf.extend_from_slice(&self.slippage_bps.to_be_bytes());
        buf.extend_from_slice(&self.base_fee_at_creation.to_be_bytes());
        buf.extend_from_slice(&self.l1_data_fee_at_creation.to_be_bytes());
        buf.extend_from_slice(&self.priority_fee_gwei.to_be_bytes());
        buf.extend_from_slice(&self.max_base_fee_gwei.to_be_bytes());
        // Option<u16>: explicit discriminant byte + value, so None and
        // Some(0) remain distinguishable in the hash input.
        match self.price_impact_bps {
            Some(v) => {
                buf.push(1);
                buf.extend_from_slice(&v.to_be_bytes());
            }
            None => {
                buf.push(0);
                buf.extend_from_slice(&0u16.to_be_bytes());
            }
        }
        buf.push(self.ofa_compliant as u8);
        buf.extend_from_slice(&self.expiry_block.to_be_bytes());
        buf.extend_from_slice(&self.nonce.to_be_bytes());
        buf.push(self.confirmation_depth);
        // Length-prefixed so a client_order_id boundary can't be
        // ambiguous with adjacent bytes (unlike the fixed-width fields
        // above, a String has variable length).
        buf.extend_from_slice(&(self.client_order_id.len() as u32).to_be_bytes());
        buf.extend_from_slice(self.client_order_id.as_bytes());
        keccak256(&buf)
    }

    /// True if `blueprint_hash` matches what `compute_hash()` derives
    /// from the blueprint's current field values, right now.
    ///
    /// Call this at any trust boundary — right before simulation, right
    /// before relay submission — as a cheap integrity check that
    /// nothing mutated a field after construction without recomputing
    /// the hash. A blueprint that fails this check should be treated
    /// the same as a SIMULATION_STATE_MISMATCH: discard it, never
    /// submit it.
    #[inline]
    pub fn verify_hash(&self) -> bool {
        self.blueprint_hash == self.compute_hash()
    }

    /// Same check as `verify_hash()`, but on failure returns the stored
    /// and recomputed hashes instead of a bare `false`, so callers can
    /// log what actually diverged.
    pub fn verify_hash_detailed(&self) -> Result<(), HashMismatch> {
        let actual = self.compute_hash();
        if self.blueprint_hash == actual {
            Ok(())
        } else {
            Err(HashMismatch {
                expected: self.blueprint_hash,
                actual,
            })
        }
    }

    /// Deterministically recomputes what `idempotency_key` should be
    /// from this blueprint's economic-identity fields: `strategy_id`,
    /// `chain_id`, `nonce`, `strategy_bytecode_hash`,
    /// `flashloan_provider`, `flashloan_amount`, `expected_profit_net`.
    ///
    /// Deliberately does NOT use a `target_contract_address` field —
    /// this struct has no such field; `strategy_bytecode_hash` already
    /// serves as this blueprint's contract-identity anchor (see this
    /// file's finding 4 module doc comment).
    ///
    /// Deliberately does NOT use `client_order_id` — `client_order_id`
    /// embeds `signal_id` (see `derive_client_order_id`), which is
    /// expected to differ across independent scorer invocations of the
    /// identical trade. Hashing it in would give two such blueprints
    /// two different idempotency keys, defeating the entire point of
    /// this key. This was a real bug in an earlier revision; see
    /// finding 6 in this file's module doc comment.
    ///
    /// `strategy_id` IS included (it was not, previously): `nonce` is
    /// only unique per `(strategy_id, chain_id)` (see `nonce_key`), so
    /// omitting `strategy_id` here would let two different strategies
    /// that happen to share a nonce and coincidentally-identical
    /// financial terms collide on the same idempotency key.
    ///
    /// Including `nonce` is deliberate: it ties this key specifically
    /// to a same-nonce resubmission of otherwise-identical parameters —
    /// the caller-side-retry-bug scenario this key exists to catch (see
    /// `omega_rpc::client::SubmissionTracker`'s doc comment for the
    /// analogous RPC-layer guard against the same class of bug one
    /// stage later, after signing). A blueprint re-derived with a
    /// genuinely new nonce is a new logical submission and is correctly
    /// NOT caught by this key.
    ///
    /// `max_base_fee_gwei` (like `dynamic_min_profit` and the buffer
    /// factors) is a risk/gas-pricing parameter, not part of the
    /// trade's economic identity, so it's excluded here too — the same
    /// trade re-derived with a different fee ceiling is still the same
    /// submission for dedup purposes.
    pub fn compute_idempotency_key(&self) -> B256 {
        let mut buf = Vec::with_capacity(128);
        buf.extend_from_slice(&self.chain_id.to_be_bytes());
        // Length-prefixed even though StrategyId's Display output is
        // short and fixed-set today — cheap insurance against a future
        // variant whose name is a prefix of another's.
        let strategy_bytes = self.strategy_id.to_string();
        buf.extend_from_slice(&(strategy_bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(strategy_bytes.as_bytes());
        buf.extend_from_slice(&self.nonce.to_be_bytes());
        buf.extend_from_slice(self.strategy_bytecode_hash.as_slice());
        buf.extend_from_slice(self.flashloan_provider.as_slice());
        buf.extend_from_slice(&self.flashloan_amount.to_be_bytes::<32>());
        buf.extend_from_slice(&self.expected_profit_net.to_be_bytes::<32>());
        keccak256(&buf)
    }

    /// True if `idempotency_key` matches what `compute_idempotency_key()`
    /// derives from the blueprint's current field values, right now.
    /// Call this at the submission boundary, alongside `verify_hash()`.
    #[inline]
    pub fn verify_idempotency_key(&self) -> bool {
        self.idempotency_key == self.compute_idempotency_key()
    }

    /// Same check as `verify_idempotency_key()`, but on failure returns
    /// the stored and recomputed keys instead of a bare `false`.
    pub fn verify_idempotency_key_detailed(&self) -> Result<(), IdempotencyKeyMismatch> {
        let actual = self.compute_idempotency_key();
        if self.idempotency_key == actual {
            Ok(())
        } else {
            Err(IdempotencyKeyMismatch {
                expected: self.idempotency_key,
                actual,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_blueprint() -> ExecutionBlueprint {
        let signal_id = Uuid::from_bytes([0xAAu8; 16]);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(StrategyId::Sa, 42161, 1, signal_id);

        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO, // placeholder; overwritten below
            chain_id: 42161,
            strategy_id: StrategyId::Sa,
            lane: Lane::Microtx,
            simulator: Simulator::Revm,
            signal_state_hash: B256::from([0xABu8; 32]),
            state_version: 7,
            signal_id,
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::from(1_000_000u64),
            flashloan_available: U256::from(2_000_000u64),
            calldata: Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]),
            strategy_bytecode_hash: B256::from([0xCDu8; 32]),
            l2_exec_gas_estimate: 100_000,
            l1_data_gas_estimate: 5_000,
            extraction_gas: 45_000,
            expected_profit_net: U256::from(1_000_000_000_000_000_000u128),
            dynamic_min_profit: U256::from(100_000_000_000_000_000u128),
            l2_buffer_factor: 1.15,
            l1_data_buffer_factor: 1.10,
            slippage_bps: 20,
            base_fee_at_creation: 1,
            l1_data_fee_at_creation: 40,
            priority_fee_gwei: 10,
            max_base_fee_gwei: ExecutionBlueprint::derive_max_base_fee_gwei(1, 3.0),
            price_impact_bps: Some(15),
            ofa_compliant: true,
            expiry_block: 1_000,
            nonce: 1,
            confirmation_depth: 12,
            client_order_id,
            idempotency_key: B256::ZERO, // placeholder; overwritten below
            relay_targets: vec!["flashbots".to_string()],
            zk_proof_commitment: None,
        };
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();
        bp
    }

    #[test]
    fn compute_hash_is_deterministic() {
        let bp = sample_blueprint();
        assert_eq!(bp.compute_hash(), bp.compute_hash());
    }

    #[test]
    fn verify_hash_passes_for_freshly_constructed_blueprint() {
        let bp = sample_blueprint();
        assert!(bp.verify_hash());
        assert!(bp.verify_hash_detailed().is_ok());
    }

    #[test]
    fn verify_hash_detailed_reports_expected_and_actual_on_mismatch() {
        let mut bp = sample_blueprint();
        let original_hash = bp.blueprint_hash;
        bp.expected_profit_net = U256::from(999u64);
        let err = bp
            .verify_hash_detailed()
            .expect_err("mutated field must fail verification");
        assert_eq!(err.expected, original_hash);
        assert_eq!(err.actual, bp.compute_hash());
        assert_ne!(err.expected, err.actual);
    }

    #[test]
    fn verify_hash_fails_after_mutating_a_hashed_field() {
        let mut bp = sample_blueprint();
        bp.expected_profit_net = U256::from(999u64);
        assert!(
            !bp.verify_hash(),
            "mutating a hashed field must desync blueprint_hash"
        );
    }

    #[test]
    fn verify_hash_fails_after_mutating_client_order_id() {
        let mut bp = sample_blueprint();
        bp.client_order_id = "tampered".to_string();
        assert!(
            !bp.verify_hash(),
            "client_order_id is a construction-time identity field and must be hashed \
             into blueprint_hash (it is NOT hashed into idempotency_key — see the \
             idempotency-key tests below)"
        );
    }

    #[test]
    fn verify_hash_fails_after_mutating_signal_id() {
        let mut bp = sample_blueprint();
        bp.signal_id = Uuid::from_bytes([0xFFu8; 16]);
        assert!(
            !bp.verify_hash(),
            "signal_id must be part of the hashed field set"
        );
    }

    #[test]
    fn verify_hash_unaffected_by_post_construction_fields() {
        // relay_targets and zk_proof_commitment are populated in LATER
        // pipeline stages (Gas War Engine, ZK layer) — mutating them
        // after construction must NOT break verify_hash(), since they
        // were never part of the original commitment.
        let mut bp = sample_blueprint();
        assert!(bp.verify_hash());
        bp.relay_targets = vec!["bloxroute".to_string(), "titan".to_string()];
        bp.zk_proof_commitment = Some(B256::from([0xEFu8; 32]));
        assert!(
            bp.verify_hash(),
            "relay_targets/zk_proof_commitment are intentionally excluded from the hash"
        );
    }

    #[test]
    fn verify_hash_unaffected_by_idempotency_key_mutation() {
        // idempotency_key is a separately-derived value for a distinct
        // consumer (submission-layer dedup) — mutating it must not
        // affect blueprint_hash, which serves the LA/ZK role instead.
        let mut bp = sample_blueprint();
        assert!(bp.verify_hash());
        bp.idempotency_key = B256::from([0x11u8; 32]);
        assert!(
            bp.verify_hash(),
            "idempotency_key is intentionally excluded from blueprint_hash's input set"
        );
    }

    #[test]
    fn verify_hash_fails_after_mutating_max_base_fee_gwei() {
        let mut bp = sample_blueprint();
        assert!(bp.verify_hash());
        bp.max_base_fee_gwei += 1;
        assert!(
            !bp.verify_hash(),
            "max_base_fee_gwei is a construction-time field and must be hashed \
             into blueprint_hash"
        );
    }

    #[test]
    fn idempotency_key_unaffected_by_max_base_fee_gwei_mutation() {
        // Risk/gas-pricing parameter, not economic trade identity — see
        // compute_idempotency_key()'s doc comment.
        let mut bp = sample_blueprint();
        let key_before = bp.compute_idempotency_key();
        bp.max_base_fee_gwei += 1;
        assert_eq!(
            bp.compute_idempotency_key(),
            key_before,
            "max_base_fee_gwei must not influence compute_idempotency_key()"
        );
    }

    #[test]
    fn derive_max_base_fee_gwei_rounds_up_not_down() {
        // 1 gwei * 3.0 = 3 exactly, no rounding ambiguity — pick a case
        // that actually exercises the ceiling.
        assert_eq!(ExecutionBlueprint::derive_max_base_fee_gwei(1, 3.0), 3);
        // 3 gwei * 1.10 = 3.3 -> must ceil to 4, not truncate to 3,
        // mirroring total_l2_gas_budget_rounds_up_not_down.
        assert_eq!(ExecutionBlueprint::derive_max_base_fee_gwei(3, 1.10), 4);
    }

    #[test]
    fn max_base_fee_wei_converts_gwei_to_wei() {
        let mut bp = sample_blueprint();
        bp.max_base_fee_gwei = 42;
        assert_eq!(bp.max_base_fee_wei(), U256::from(42_000_000_000u64));
    }

    #[test]
    fn is_expired_boundary_matches_omega_risk_check_expiry() {
        let bp = sample_blueprint(); // expiry_block = 1_000
        assert!(!bp.is_expired(999), "before expiry: not expired");
        assert!(
            bp.is_expired(1_000),
            "AT expiry_block: must already be expired (matches check_expiry's >=)"
        );
        assert!(bp.is_expired(1_001), "past expiry: expired");
    }

    #[test]
    fn is_profitable_boundary_matches_omega_risk_check_dynamic_profit() {
        let mut bp = sample_blueprint();
        bp.expected_profit_net = bp.dynamic_min_profit; // exact tie
        assert!(
            bp.is_profitable(),
            "exact tie must be profitable, matching check_dynamic_profit's strict-< rejection"
        );

        bp.expected_profit_net = bp.dynamic_min_profit - U256::from(1u64);
        assert!(!bp.is_profitable());
    }

    #[test]
    fn total_l2_gas_budget_rounds_up_not_down() {
        let mut bp = sample_blueprint();
        // 3 gas * 1.10 buffer = 3.3 -> must ceil to 4, not truncate to 3.
        bp.l2_exec_gas_estimate = 3;
        bp.l2_buffer_factor = 1.10;
        bp.extraction_gas = 0;
        assert_eq!(bp.total_l2_gas_budget(), 4);
    }

    #[test]
    fn total_l2_gas_budget_includes_extraction_gas() {
        let mut bp = sample_blueprint();
        bp.l2_exec_gas_estimate = 100_000;
        bp.l2_buffer_factor = 1.0; // no rounding ambiguity
        bp.extraction_gas = 45_000;
        assert_eq!(bp.total_l2_gas_budget(), 145_000);
    }

    #[test]
    fn flashloan_feasible_boundary() {
        let mut bp = sample_blueprint();
        bp.flashloan_amount = U256::from(500u64);
        bp.flashloan_available = U256::from(500u64);
        assert!(
            bp.flashloan_feasible(),
            "amount == available is feasible (raw check only, no safety margin)"
        );
        bp.flashloan_amount = U256::from(501u64);
        assert!(!bp.flashloan_feasible());
    }

    #[test]
    fn select_simulator_rule() {
        assert_eq!(
            ExecutionBlueprint::select_simulator(Lane::Microtx, 199_999),
            Simulator::Revm
        );
        assert_eq!(
            ExecutionBlueprint::select_simulator(Lane::Microtx, 200_000),
            Simulator::Anvil
        );
        assert_eq!(
            ExecutionBlueprint::select_simulator(Lane::Normal, 1_000),
            Simulator::Anvil
        );
    }

    #[test]
    fn nonce_key_is_stable_and_distinguishes_chains() {
        let k1 = ExecutionBlueprint::nonce_key(StrategyId::Sa, 42161);
        let k2 = ExecutionBlueprint::nonce_key(StrategyId::Sa, 42161);
        let k3 = ExecutionBlueprint::nonce_key(StrategyId::Sa, 1);
        let k4 = ExecutionBlueprint::nonce_key(StrategyId::La, 42161);
        assert_eq!(k1, k2, "same inputs must produce the same key");
        assert_ne!(k1, k3, "different chain_id must produce a different key");
        assert_ne!(k1, k4, "different strategy_id must produce a different key");
    }

    #[test]
    fn is_canary_delegates_to_strategy_id() {
        let mut bp = sample_blueprint();
        bp.strategy_id = StrategyId::Cnry;
        assert!(bp.is_canary());
        bp.strategy_id = StrategyId::Sa;
        assert!(!bp.is_canary());
    }

    // ── Idempotency key / client order ID ────────────────────────────────────

    #[test]
    fn compute_idempotency_key_is_deterministic() {
        let bp = sample_blueprint();
        assert_eq!(bp.compute_idempotency_key(), bp.compute_idempotency_key());
    }

    #[test]
    fn verify_idempotency_key_passes_for_freshly_constructed_blueprint() {
        let bp = sample_blueprint();
        assert!(bp.verify_idempotency_key());
        assert!(bp.verify_idempotency_key_detailed().is_ok());
    }

    #[test]
    fn verify_idempotency_key_detailed_reports_expected_and_actual_on_mismatch() {
        let mut bp = sample_blueprint();
        let original_key = bp.idempotency_key;
        bp.flashloan_amount = U256::from(999_999u64);
        let err = bp
            .verify_idempotency_key_detailed()
            .expect_err("mutated economic field must fail verification");
        assert_eq!(err.expected, original_key);
        assert_eq!(err.actual, bp.compute_idempotency_key());
        assert_ne!(err.expected, err.actual);
    }

    #[test]
    fn verify_idempotency_key_fails_after_mutating_flashloan_amount() {
        let mut bp = sample_blueprint();
        bp.flashloan_amount = U256::from(999_999u64);
        assert!(
            !bp.verify_idempotency_key(),
            "mutating flashloan principal must desync idempotency_key"
        );
    }

    #[test]
    fn idempotency_key_unaffected_by_client_order_id_mutation() {
        // client_order_id embeds signal_id and is deliberately NOT part
        // of the idempotency key's input set (see finding 6 in the
        // module doc comment) — mutating it must not change the key.
        let mut bp = sample_blueprint();
        let key_before = bp.compute_idempotency_key();
        bp.client_order_id = "some-other-client-order-id".to_string();
        assert_eq!(
            bp.compute_idempotency_key(),
            key_before,
            "client_order_id must not influence compute_idempotency_key()"
        );
    }

    #[test]
    fn same_nonce_same_params_same_idempotency_key() {
        // The exact scenario this key exists to catch: two separately
        // constructed blueprints representing the identical trade at
        // the identical nonce must produce the IDENTICAL
        // idempotency_key, even though they come from different scorer
        // invocations (different signal_id / client_order_id).
        let bp1 = sample_blueprint();
        let mut bp2 = sample_blueprint();
        bp2.signal_id = Uuid::from_bytes([0x99u8; 16]);
        bp2.client_order_id = ExecutionBlueprint::derive_client_order_id(
            bp2.strategy_id,
            bp2.chain_id,
            bp2.nonce,
            bp2.signal_id,
        );
        bp2.idempotency_key = bp2.compute_idempotency_key();

        assert_ne!(
            bp1.client_order_id, bp2.client_order_id,
            "sanity check: the two client_order_ids must actually differ, or this \
             test wouldn't be exercising the bug it's meant to catch"
        );
        assert_eq!(
            bp1.compute_idempotency_key(),
            bp2.compute_idempotency_key(),
            "two blueprints for the identical trade at the identical nonce, from \
             two different scorer invocations, must share one idempotency_key"
        );
    }

    #[test]
    fn duplicate_scorer_invocation_yields_same_idempotency_key() {
        // Same scenario as above, phrased as the real-world case this
        // key exists for: a duplicated scorer task re-derives the
        // identical trade into a fresh blueprint before either is ever
        // signed. Both must be recognized as the same submission.
        let bp1 = sample_blueprint();
        let mut bp2 = sample_blueprint();
        bp2.signal_id = Uuid::new_v4();
        bp2.client_order_id = ExecutionBlueprint::derive_client_order_id(
            bp2.strategy_id,
            bp2.chain_id,
            bp2.nonce,
            bp2.signal_id,
        );
        bp2.idempotency_key = bp2.compute_idempotency_key();

        assert_eq!(bp1.idempotency_key, bp2.idempotency_key);
        assert!(bp2.verify_idempotency_key());
    }

    #[test]
    fn different_nonce_different_idempotency_key() {
        let bp1 = sample_blueprint();
        let mut bp2 = sample_blueprint();
        bp2.nonce = bp1.nonce + 1;
        bp2.client_order_id = ExecutionBlueprint::derive_client_order_id(
            bp2.strategy_id,
            bp2.chain_id,
            bp2.nonce,
            bp2.signal_id,
        );
        assert_ne!(
            bp1.compute_idempotency_key(),
            bp2.compute_idempotency_key(),
            "a genuinely new nonce must produce a different idempotency key, \
             since it represents a new logical submission, not a retry"
        );
    }

    #[test]
    fn different_strategy_id_different_idempotency_key() {
        // Regression guard for the gap the strategy_id addition closes:
        // nonces are only unique per (strategy_id, chain_id), so two
        // different strategies must not collide on idempotency_key even
        // if every other economic field happens to match.
        let bp1 = sample_blueprint();
        let mut bp2 = sample_blueprint();
        bp2.strategy_id = StrategyId::La;
        assert_ne!(
            bp1.compute_idempotency_key(),
            bp2.compute_idempotency_key(),
            "different strategy_id must produce a different idempotency key"
        );
    }

    #[test]
    fn derive_client_order_id_is_stable() {
        let id = Uuid::from_bytes([0x01u8; 16]);
        let a = ExecutionBlueprint::derive_client_order_id(StrategyId::La, 42161, 5, id);
        let b = ExecutionBlueprint::derive_client_order_id(StrategyId::La, 42161, 5, id);
        assert_eq!(a, b);
    }

    #[test]
    fn derive_client_order_id_distinguishes_chain_id() {
        let id = Uuid::from_bytes([0x01u8; 16]);
        let a = ExecutionBlueprint::derive_client_order_id(StrategyId::Sa, 42161, 5, id);
        let b = ExecutionBlueprint::derive_client_order_id(StrategyId::Sa, 1, 5, id);
        assert_ne!(
            a, b,
            "nonces are scoped per (strategy_id, chain_id) — chain_id must be part of the ID"
        );
    }

    #[test]
    fn derive_client_order_id_distinguishes_nonce() {
        let id = Uuid::from_bytes([0x01u8; 16]);
        let a = ExecutionBlueprint::derive_client_order_id(StrategyId::Sa, 42161, 5, id);
        let b = ExecutionBlueprint::derive_client_order_id(StrategyId::Sa, 42161, 6, id);
        assert_ne!(a, b);
    }
}
