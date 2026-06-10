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
/// | Signal binding | signal_state_hash, state_version                   |
/// | Execution      | flashloan_*, calldata, strategy_bytecode_hash      |
/// | Gas model (§7) | l2_exec_gas_estimate, l1_data_gas_estimate, …      |
/// | Economics      | expected_profit_net, dynamic_min_profit, slippage_bps |
/// | Timing         | expiry_block, nonce, confirmation_depth             |
/// | Relay          | relay_targets                                       |
/// | ZK             | zk_proof_commitment                                 |
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionBlueprint {
    // ── Identity ─────────────────────────────────────────────────────────
    /// keccak256 of the canonical serialised blueprint fields (all fields
    /// except `blueprint_hash` itself).  Acts as the join key in Loss
    /// Attribution (§13) and the ZK vault proof input (§15).
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

    // ── Relay ────────────────────────────────────────────────────────────
    /// Ordered list of relay endpoint identifiers.  Populated by the
    /// Gas War Engine using LA-inclusion-rate ranking (§11.2).
    /// Must have at least one entry; may have up to 4 (cascade mode).
    pub relay_targets: Vec<String>,

    // ── ZK ───────────────────────────────────────────────────────────────
    /// Commitment to the StarkProof that gates Vault profit release (§15).
    /// None during blueprint construction; set by the ZK layer before
    /// relay submission when ZK is required for this strategy.
    pub zk_proof_commitment: Option<B256>,
}

impl ExecutionBlueprint {
    /// Derives the nonce namespace key for a (strategy, chain) pair.
    ///
    /// Used by nonce managers in omega-strategies to ensure each
    /// (strategy_id, chain_id) combination has an independent nonce
    /// sequence — preventing cross-strategy replay.
    pub fn nonce_key(strategy_id: StrategyId, chain_id: u64) -> B256 {
        let mut buf = Vec::with_capacity(36);
        // Stable strategy discriminant — format!("{strategy_id}") is
        // used rather than the enum index to survive enum reordering.
        buf.extend_from_slice(keccak256(strategy_id.to_string().as_bytes()).as_slice());
        buf.extend_from_slice(&chain_id.to_be_bytes());
        keccak256(&buf)
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
    /// total = (l2_exec_gas_estimate × l2_buffer_factor) + extraction_gas
    ///
    /// Used by the Gas War Engine fee cap calculation (§12).
    #[inline]
    pub fn total_l2_gas_budget(&self) -> u64 {
        let buffered = (self.l2_exec_gas_estimate as f64 * self.l2_buffer_factor) as u64;
        buffered.saturating_add(self.extraction_gas)
    }

    /// Returns `true` when `expected_profit_net` exceeds `dynamic_min_profit`.
    ///
    /// This is the primary profitability gate checked before relay
    /// submission and before emergency bundle emission (§12.1).
    #[inline]
    pub fn is_profitable(&self) -> bool {
        self.expected_profit_net > self.dynamic_min_profit
    }

    /// Returns `true` when this blueprint has exceeded its expiry block.
    #[inline]
    pub fn is_expired(&self, current_block: u64) -> bool {
        current_block > self.expiry_block
    }

    /// Returns `true` when `flashloan_amount` is within available
    /// liquidity.  Checked during blueprint construction (§11).
    #[inline]
    pub fn flashloan_feasible(&self) -> bool {
        self.flashloan_amount <= self.flashloan_available
    }
}
