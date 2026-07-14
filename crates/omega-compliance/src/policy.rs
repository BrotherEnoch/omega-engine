// crates/omega-compliance/src/policy.rs
use std::sync::Arc;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use omega_core::types::blueprint::ExecutionBlueprint;  // Adjust import as needed in your core

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
        Self { policy: Arc::new(policy) }
    }

    pub fn validate_blueprint(
        &self,
        bp: &ExecutionBlueprint,
        now: DateTime<Utc>,
    ) -> Result<(), ComplianceError> {
        // Asset permission
        let asset = bp.asset_symbol(); // Implement or adapt to your blueprint fields
        if !self.policy.allowed_assets.contains(&asset) {
            return Err(ComplianceError::Violation(format!("Asset {} not allowed", asset)));
        }

        // Chain permission
        if !self.policy.allowed_chains.contains(&bp.chain_id) {
            return Err(ComplianceError::Violation(format!("Chain {} not allowed", bp.chain_id)));
        }

        // Position size (adapt field names)
        if bp.notional_value() > self.policy.max_position_size_usd {  // Implement helper if needed
            return Err(ComplianceError::Violation("Exceeds max position size".into()));
        }

        // Time window
        if !self.is_in_trading_window(now) {
            return Err(ComplianceError::Violation("Outside allowed trading window".into()));
        }

        // Strategy
        if !self.policy.allowed_strategies.contains(&bp.strategy_id.to_string()) {
            return Err(ComplianceError::Violation("Strategy not allowed".into()));
        }

        Ok(())
    }

    fn is_in_trading_window(&self, now: DateTime<Utc>) -> bool {
        if self.policy.trading_windows.is_empty() {
            return true; // No restriction
        }
        self.policy.trading_windows.iter().any(|w| w.start <= now && now <= w.end)
    }
}