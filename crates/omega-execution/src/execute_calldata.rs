// crates/omega-execution/src/execute_calldata.rs
//! Public execute-calldata path for `OmegaOrchestrator.execute(bytes, bytes)`.
//!
//! Extracted so alternate outer-envelope signers (e.g. AWS KMS) can build the
//! same on-chain authorization calldata without holding an in-memory
//! `SecretKey` or depending on `KeyManager`.
//!
//! Behavioral contract matches the former private methods on
//! `KeyManagerTransactionSigner`.

use std::collections::HashMap;
use std::sync::Arc;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolCall, SolValue};
use omega_core::ExecutionBlueprint;
use omega_security::BlueprintSigner;

use crate::error::ExecutionError;

sol! {
    /// Mirrors OmegaOrchestrator.sol's `blueprintCalldata` layout.
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

    /// `keccak256(abi.encode(orchestrator, chainId, blueprintCalldata))`
    #[derive(Debug)]
    struct DomainSeparatedBlueprint {
        address orchestrator;
        uint64 chain_id;
        bytes blueprint_calldata;
    }

    function execute(bytes blueprint_calldata, bytes sig) external;
}

/// Hard ceiling on `priority_fee_gwei` (docs/fee-policy.md).
pub const MAX_PRIORITY_FEE_GWEI_CAP: u64 = 50;

/// Hard ceiling on `base_fee_at_creation + 2 * priority_fee_gwei` in gwei.
pub const MAX_FEE_GWEI_CAP: u64 = 500;

pub const GWEI_TO_WEI: u64 = 1_000_000_000;

/// Stateless builder for `execute(blueprintCalldata, sig)` outer calldata.
///
/// Does **not** hold the gas-paying key. Only blueprint authorization
/// (`BlueprintSigner`) is required.
#[derive(Clone)]
pub struct ExecuteCalldataBuilder {
    orchestrator: Address,
    strategy_onchain_ids: HashMap<String, [u8; 32]>,
    blueprint_signer: Arc<BlueprintSigner>,
}

impl ExecuteCalldataBuilder {
    /// # Panics
    /// Panics if `orchestrator == Address::ZERO`.
    pub fn new(
        orchestrator: Address,
        strategy_onchain_ids: HashMap<String, [u8; 32]>,
        blueprint_signer: Arc<BlueprintSigner>,
    ) -> Self {
        assert_ne!(
            orchestrator,
            Address::ZERO,
            "ExecuteCalldataBuilder requires a real deployed OmegaOrchestrator address"
        );
        Self {
            orchestrator,
            strategy_onchain_ids,
            blueprint_signer,
        }
    }

    pub fn orchestrator(&self) -> Address {
        self.orchestrator
    }

    /// C6 fail-closed pre-flight before any key material is used.
    pub fn validate_blueprint_for_signing(
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
    pub fn resolve_strategy_id(&self, bp: &ExecutionBlueprint) -> Result<[u8; 32], ExecutionError> {
        let strategy_key = bp.strategy_id.to_string();
        let strategy_id_bytes: [u8; 32] = *self
            .strategy_onchain_ids
            .get(&strategy_key)
            .ok_or_else(|| ExecutionError::SigningFailed {
                detail: format!(
                    "no on-chain strategyId configured for strategy_id {strategy_key:?} — this \
                     value is real deployment configuration (whatever bytes32 was passed to \
                     OmegaOrchestrator.registerStrategy() for this strategy on-chain), not \
                     something derivable from code. Wire it into strategy_onchain_ids, sourced \
                     from real deployment records, never guessed at."
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

    /// ABI-encoded `blueprintCalldata` for `bp`.
    pub fn build_blueprint_calldata(
        &self,
        bp: &ExecutionBlueprint,
    ) -> Result<Vec<u8>, ExecutionError> {
        let strategy_id_bytes = self.resolve_strategy_id(bp)?;
        let provider_type: u8 = bp.flashloan_provider_type as u8;
        let max_base_fee_wei =
            U256::from(bp.max_base_fee_gwei).saturating_mul(U256::from(GWEI_TO_WEI));

        let blueprint = Blueprint {
            expiry_block: bp.expiry_block,
            nonce: bp.nonce,
            strategy_id: B256::from(strategy_id_bytes),
            provider_type,
            flashloan_token: bp.flashloan_token,
            provider_contract: bp.provider_contract,
            strategy_calldata: bp.calldata.clone(),
            flashloan_amount: bp.flashloan_amount,
            min_net_profit: bp.dynamic_min_profit,
            max_base_fee: max_base_fee_wei,
        };

        // abi_encode_params — NOT abi_encode (flat tuple vs offset word).
        Ok(blueprint.abi_encode_params())
    }

    /// Domain-separated `bpHash` matching `OmegaOrchestrator.execute()`.
    pub fn compute_bp_hash(&self, blueprint_calldata: &[u8], chain_id: u64) -> [u8; 32] {
        let domain = DomainSeparatedBlueprint {
            orchestrator: self.orchestrator,
            chain_id,
            blueprint_calldata: blueprint_calldata.to_vec().into(),
        };
        omega_security::keccak256(&domain.abi_encode_params())
    }

    /// Full outer calldata: `execute(blueprintCalldata, sig)`.
    pub fn build_execute_calldata(
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

/// EIP-1559 envelope fees in wei. Fail closed on policy caps.
pub fn envelope_fees_wei(
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

/// Assemble `execute(bytes,bytes)` selector + args.
pub fn encode_execute_call(blueprint_calldata: Vec<u8>, sig: Vec<u8>) -> Vec<u8> {
    executeCall {
        blueprint_calldata: blueprint_calldata.into(),
        sig: sig.into(),
    }
    .abi_encode()
}
