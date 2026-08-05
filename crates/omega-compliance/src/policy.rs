// crates/omega-compliance/src/policy.rs
//
// ## Fix (this revision): asset_symbol()/notional_value() don't exist on
// ExecutionBlueprint, and can't be implemented on it
//
// `validate_blueprint` previously called `bp.asset_symbol()` and
// `bp.notional_value()` — neither method exists on
// `omega_core::types::blueprint::ExecutionBlueprint`
// (`error[E0599]: no method named ... found`). The original code's own
// comments ("Implement or adapt to your blueprint fields", "Implement
// helper if needed") mark these as unfinished stubs, not a real
// implementation that just needs wiring up.
//
// They can't be added to `ExecutionBlueprint` itself, either: that
// struct's actual fields are `flashloan_provider: Address`,
// `flashloan_amount: U256` (raw token units), and
// `expected_profit_net: U256` (wei) — there is no human-readable token
// symbol anywhere on it, and no USD-denominated price. Deriving an
// "asset symbol" from a raw contract address, or a "notional value" in
// USD without a price oracle, would mean fabricating exactly the data
// this compliance check exists to verify — an allowlist/position-size
// gate that silently guesses at the asset and dollar value it's
// checking is worse than one that fails to compile, since a wrong guess
// here fails *open* (a disallowed asset or oversized position reads as
// compliant) rather than failing loudly.
//
// This crate's own dependency list (see imports below: `omega_core`,
// `chrono`, `serde`, `thiserror` — no `omega_oracle`) confirms it has no
// price-feed access of its own to compute either value correctly.
//
// Fixed by making both values explicit, caller-supplied parameters to
// `validate_blueprint` instead of methods on the blueprint. The caller
// — whatever code sits between the oracle/pricing layer and this
// compliance gate — already has to resolve "what token does this
// blueprint touch, and what's it worth in USD" for other reasons (gas
// cost accounting, profit reporting); this makes that resolution an
// explicit, visible input to the compliance decision rather than an
// implicit method call that silently returns nothing meaningful. This
// is a breaking signature change for any existing caller of
// `validate_blueprint`; there was no way to fix the underlying missing
// data without one.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

use omega_core::types::blueprint::ExecutionBlueprint; // Adjust import as needed in your core

#[derive(Debug, Clone, Error)]
pub enum ComplianceError {
    #[error("Policy violation: {0}")]
    Violation(String),
    #[error("Configuration error: {0}")]
    Config(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TimeWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompliancePolicy {
    pub allowed_assets: Vec<String>,
    pub allowed_chains: Vec<u64>,
    pub max_position_size_usd: f64,
    pub max_leverage_bps: u16,
    pub trading_windows: Vec<TimeWindow>,
    pub cooldown_period_secs: u64,
    pub allowed_strategies: Vec<String>,
}

impl Default for CompliancePolicy {
    fn default() -> Self {
        Self {
            allowed_assets: vec!["ETH".into(), "BTC".into(), "USDC".into()],
            allowed_chains: vec![42161], // Arbitrum mainnet
            max_position_size_usd: 100_000.0,
            max_leverage_bps: 5000, // 50x example
            trading_windows: vec![],
            cooldown_period_secs: 300,
            allowed_strategies: vec!["mev".into(), "flashloan".into()],
        }
    }
}

#[derive(Debug)]
pub struct ComplianceChecker {
    policy: Arc<CompliancePolicy>,
}

impl ComplianceChecker {
    pub fn new(policy: CompliancePolicy) -> Self {
        Self {
            policy: Arc::new(policy),
        }
    }

    /// Validate a blueprint against the configured compliance policy.
    ///
    /// `asset_symbol` and `notional_value_usd` are supplied by the
    /// caller rather than read off `bp` — see this file's module-level
    /// "Fix" note for why: `ExecutionBlueprint` carries a raw
    /// `flashloan_provider` address and wei-denominated amounts, not a
    /// human-readable symbol or a USD price, so resolving either
    /// requires a price/token-metadata lookup this crate has no access
    /// to. The caller (sitting closer to the oracle/pricing layer) is
    /// expected to resolve both before calling this.
    pub fn validate_blueprint(
        &self,
        bp: &ExecutionBlueprint,
        asset_symbol: &str,
        notional_value_usd: f64,
        now: DateTime<Utc>,
    ) -> Result<(), ComplianceError> {
        // Asset permission
        if !self.policy.allowed_assets.iter().any(|a| a == asset_symbol) {
            return Err(ComplianceError::Violation(format!(
                "Asset {asset_symbol} not allowed"
            )));
        }

        // Chain permission
        if !self.policy.allowed_chains.contains(&bp.chain_id) {
            return Err(ComplianceError::Violation(format!(
                "Chain {} not allowed",
                bp.chain_id
            )));
        }

        // Position size
        if notional_value_usd > self.policy.max_position_size_usd {
            return Err(ComplianceError::Violation(
                "Exceeds max position size".into(),
            ));
        }

        // Time window
        if !self.is_in_trading_window(now) {
            return Err(ComplianceError::Violation(
                "Outside allowed trading window".into(),
            ));
        }

        // Strategy
        if !self
            .policy
            .allowed_strategies
            .contains(&bp.strategy_id.to_string())
        {
            return Err(ComplianceError::Violation("Strategy not allowed".into()));
        }

        Ok(())
    }

    fn is_in_trading_window(&self, now: DateTime<Utc>) -> bool {
        if self.policy.trading_windows.is_empty() {
            return true; // No restriction
        }
        self.policy
            .trading_windows
            .iter()
            .any(|w| w.start <= now && now <= w.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, Bytes, B256, U256};
    use omega_core::types::blueprint::StrategyId;
    use omega_core::types::lane::{Lane, Simulator};
    use uuid::Uuid;

    fn sample_blueprint(chain_id: u64) -> ExecutionBlueprint {
        let signal_id = Uuid::from_bytes([0xAAu8; 16]);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(StrategyId::Sa, chain_id, 1, signal_id);
        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO,
            chain_id,
            strategy_id: StrategyId::Sa,
            lane: Lane::Microtx,
            simulator: Simulator::Revm,
            signal_state_hash: B256::from([0xABu8; 32]),
            state_version: 1,
            signal_id,
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::from(1_000_000u64),
            flashloan_available: U256::from(2_000_000u64),
            calldata: Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]),
            strategy_bytecode_hash: B256::from([0xCDu8; 32]),
            l2_exec_gas_estimate: 100_000,
            l1_data_gas_estimate: 5_000,
            extraction_gas: 45_000,
            expected_profit_net: U256::from(1u64),
            dynamic_min_profit: U256::from(1u64),
            l2_buffer_factor: 1.15,
            l1_data_buffer_factor: 1.10,
            slippage_bps: 20,
            base_fee_at_creation: 1,
            l1_data_fee_at_creation: 40,
            priority_fee_gwei: 10,
            price_impact_bps: None,
            ofa_compliant: true,
            expiry_block: 1_000,
            nonce: 1,
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

    #[test]
    fn allowed_asset_and_chain_and_size_passes() {
        let checker = ComplianceChecker::new(CompliancePolicy::default());
        let bp = sample_blueprint(42161);
        let now = Utc::now();
        assert!(checker
            .validate_blueprint(&bp, "ETH", 50_000.0, now)
            .is_ok());
    }

    #[test]
    fn disallowed_asset_is_rejected() {
        let checker = ComplianceChecker::new(CompliancePolicy::default());
        let bp = sample_blueprint(42161);
        let now = Utc::now();
        let err = checker
            .validate_blueprint(&bp, "DOGE", 1_000.0, now)
            .unwrap_err();
        assert!(matches!(err, ComplianceError::Violation(msg) if msg.contains("DOGE")));
    }

    #[test]
    fn disallowed_chain_is_rejected() {
        let checker = ComplianceChecker::new(CompliancePolicy::default());
        let bp = sample_blueprint(1); // Ethereum mainnet, not in default allowed_chains
        let now = Utc::now();
        let err = checker
            .validate_blueprint(&bp, "ETH", 1_000.0, now)
            .unwrap_err();
        assert!(matches!(err, ComplianceError::Violation(_)));
    }

    #[test]
    fn oversized_position_is_rejected() {
        let checker = ComplianceChecker::new(CompliancePolicy::default());
        let bp = sample_blueprint(42161);
        let now = Utc::now();
        let err = checker
            .validate_blueprint(&bp, "ETH", 1_000_000.0, now) // > 100_000 default cap
            .unwrap_err();
        assert!(matches!(err, ComplianceError::Violation(msg) if msg.contains("position size")));
    }

    #[test]
    fn empty_trading_windows_means_unrestricted() {
        let checker = ComplianceChecker::new(CompliancePolicy::default());
        assert!(checker.is_in_trading_window(Utc::now()));
    }

    #[test]
    fn outside_configured_trading_window_is_rejected() {
        let mut policy = CompliancePolicy::default();
        let now = Utc::now();
        policy.trading_windows = vec![TimeWindow {
            start: now - chrono::Duration::hours(2),
            end: now - chrono::Duration::hours(1),
        }];
        let checker = ComplianceChecker::new(policy);
        let bp = sample_blueprint(42161);
        let err = checker
            .validate_blueprint(&bp, "ETH", 1_000.0, now)
            .unwrap_err();
        assert!(matches!(err, ComplianceError::Violation(msg) if msg.contains("trading window")));
    }
}
