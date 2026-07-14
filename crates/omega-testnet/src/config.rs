// omega-engine\crates\omega-testnet\src\config.rs
//! Configuration schema for a Phase 0.75 testnet dry-run.
//!
//! Deliberately holds no secret material. `relay_auth_key_env_var` and
//! `execution_signer_env_var` are the *names* of environment variables the
//! relay/signing layer (outside this crate) should read from — this crate
//! never reads, stores, or logs the values themselves.

use crate::error::{Result, TestnetError};
use serde::{Deserialize, Serialize};

/// Chain IDs treated as "looks like mainnet" for the safety guard in
/// `TestnetConfig::validate`. Not exhaustive — a denylist here is
/// defense-in-depth on top of the person's own judgment, not a substitute
/// for it.
const KNOWN_MAINNET_CHAIN_IDS: &[u64] = &[
    1,     // Ethereum mainnet
    42161, // Arbitrum One
    10,    // OP Mainnet
    137,   // Polygon PoS
    8453,  // Base
    56,    // BNB Smart Chain
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestnetConfig {
    /// Human-readable label for this run, used in report filenames and logs.
    pub run_label: String,

    /// Testnet chain ID (e.g. 11155111 for Sepolia).
    pub chain_id: u64,

    /// Escape hatch: set true only if `chain_id` is intentionally a
    /// mainnet chain (e.g. a mainnet canary run per the phase-gate plan,
    /// not a testnet run). Defaults to false; `validate()` rejects known
    /// mainnet chain IDs unless this is explicitly set.
    #[serde(default)]
    pub allow_mainnet_chain_id: bool,

    /// Read-only RPC endpoint for the testnet chain.
    pub rpc_url: String,

    /// Name of the environment variable holding the relay auth key. The
    /// key itself is never stored in this struct or serialized with it.
    pub relay_auth_key_env_var: String,

    /// Name of the environment variable holding the execution signer's
    /// key material (or a reference to it, e.g. an HSM key ID). Never
    /// stored or serialized here.
    pub execution_signer_env_var: String,

    /// Public address of the burner wallet used for this run. Public by
    /// definition — safe to store and log.
    pub burner_wallet_address: String,

    /// Hard cap on total value (wei) this run is permitted to risk,
    /// enforced by the caller before every submission — this crate only
    /// carries the number, it doesn't enforce it by itself.
    pub max_position_wei: u128,

    /// Planned soak duration for this run, in hours. The phase-gate plan
    /// calls for days, not hours, of continuous operation before treating
    /// results as meaningful — see `gate.rs`.
    pub planned_soak_hours: u32,

    /// Minimum number of cycles to run regardless of elapsed time.
    pub min_cycles: u32,
}

impl TestnetConfig {
    /// Validates internal consistency and applies the mainnet-chain-id
    /// guard. Call before starting any run.
    pub fn validate(&self) -> Result<()> {
        if self.run_label.trim().is_empty() {
            return Err(TestnetError::InvalidConfig("run_label must not be empty".into()));
        }
        if self.rpc_url.trim().is_empty() {
            return Err(TestnetError::InvalidConfig("rpc_url must not be empty".into()));
        }
        if self.relay_auth_key_env_var.trim().is_empty() {
            return Err(TestnetError::InvalidConfig(
                "relay_auth_key_env_var must name an environment variable".into(),
            ));
        }
        if self.execution_signer_env_var.trim().is_empty() {
            return Err(TestnetError::InvalidConfig(
                "execution_signer_env_var must name an environment variable".into(),
            ));
        }
        if self.burner_wallet_address.trim().is_empty() {
            return Err(TestnetError::InvalidConfig("burner_wallet_address must not be empty".into()));
        }
        if self.max_position_wei == 0 {
            return Err(TestnetError::InvalidConfig(
                "max_position_wei must be > 0 (0 would mean the run can never submit anything)".into(),
            ));
        }
        if self.min_cycles == 0 {
            return Err(TestnetError::InvalidConfig("min_cycles must be >= 1".into()));
        }

        if KNOWN_MAINNET_CHAIN_IDS.contains(&self.chain_id) && !self.allow_mainnet_chain_id {
            return Err(TestnetError::MainnetChainIdRejected(self.chain_id));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_cfg() -> TestnetConfig {
        TestnetConfig {
            run_label: "sepolia-soak-1".into(),
            chain_id: 11155111, // Sepolia
            allow_mainnet_chain_id: false,
            rpc_url: "https://sepolia.example.com".into(),
            relay_auth_key_env_var: "FLASHBOTS_AUTH_KEY_TESTNET".into(),
            execution_signer_env_var: "TESTNET_SIGNER_KEY".into(),
            burner_wallet_address: "0x000000000000000000000000000000000000dEaD".into(),
            max_position_wei: 10_000_000_000_000_000, // 0.01 ETH
            planned_soak_hours: 72,
            min_cycles: 500,
        }
    }

    #[test]
    fn valid_config_passes() {
        assert!(valid_cfg().validate().is_ok());
    }

    #[test]
    fn rejects_mainnet_chain_id_by_default() {
        let mut cfg = valid_cfg();
        cfg.chain_id = 42161; // Arbitrum One
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, TestnetError::MainnetChainIdRejected(42161)));
    }

    #[test]
    fn allows_mainnet_chain_id_when_explicit() {
        let mut cfg = valid_cfg();
        cfg.chain_id = 42161;
        cfg.allow_mainnet_chain_id = true;
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn rejects_zero_position_cap() {
        let mut cfg = valid_cfg();
        cfg.max_position_wei = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_empty_env_var_names() {
        let mut cfg = valid_cfg();
        cfg.relay_auth_key_env_var = "".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn config_serializes_without_secret_fields() {
        // No field on TestnetConfig holds a secret value, so a full
        // round-trip serialization is safe to write to disk/logs.
        let cfg = valid_cfg();
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("PRIVATE"));
        let back: TestnetConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.chain_id, cfg.chain_id);
    }
}