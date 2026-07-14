// omega-engine\crates\omega-simulation\src\traits.rs
//! These types/traits are defined here for now so this crate is
//! self-contained, but they are meant to be identical to whatever
//! `omega-core` and `omega-execution` use. In the real workspace, delete
//! this file and depend on `omega_core::{Bundle, BundleSubmitter, ...}`
//! instead — the whole point of Phase 0.5 is that sim and live share one
//! definition of "what a bundle is" and diverge only in "where it goes."

use crate::error::Result;
use async_trait::async_trait;
use ethers::types::{Address, Bytes, U256};
use serde::{Deserialize, Serialize};

/// A candidate opportunity surfaced by the engine's detection logic
/// (arbitrage path, liquidation target, etc.) — independent of whether it
/// will be executed live or in simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opportunity {
    pub id: String,
    pub kind: OpportunityKind,
    pub target_pool: Address,
    pub flash_loan_asset: Address,
    pub flash_loan_amount: U256,
    pub calldata: Bytes,
    pub expected_profit_wei: U256,
    pub gas_estimate: U256,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OpportunityKind {
    Arbitrage,
    Liquidation,
}

/// A bundle of one or more transactions meant to land atomically.
/// In live mode this maps to a Flashbots/bloXroute/Titan/Eden bundle.
/// In simulation mode it's just sent to the local fork sequentially/atomically
/// via `eth_sendTransaction` against the dev-funded anvil accounts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub opportunity_id: String,
    pub target_contract: Address,
    pub calldata: Bytes,
    pub value: U256,
    pub gas_limit: U256,
    pub max_fee_per_gas: U256,
    pub max_priority_fee_per_gas: U256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub opportunity_id: String,
    pub tx_hash: String,
    pub success: bool,
    pub gas_used: U256,
    pub realized_profit_wei: Option<i128>,
    pub revert_reason: Option<String>,
}

/// Implemented once for live relays (omega-execution) and once for the
/// local fork (this crate, `SimulationSubmitter`). The engine's core loop
/// depends only on this trait, never on a concrete transport.
#[async_trait]
pub trait BundleSubmitter: Send + Sync {
    async fn submit(&self, bundle: Bundle) -> Result<Receipt>;
}

/// Whatever the engine's real detection logic implements. Left generic here
/// so the harness can be driven either by the real detector or by a fixture
/// detector in tests, without the harness caring which.
#[async_trait]
pub trait OpportunityDetector: Send + Sync {
    async fn next_opportunities(&mut self, block_number: u64) -> Result<Vec<Opportunity>>;
}