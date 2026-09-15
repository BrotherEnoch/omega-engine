// crates/aws-kms-omega-adapter/src/signer.rs
use crate::envelope::sign_eip1559_envelope;
use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use aws_kms_signer::AwsKmsSigner;
use omega_core::ExecutionBlueprint;
use omega_execution::{
    envelope_fees_wei, ExecuteCalldataBuilder, ExecutionError, SignedTransaction,
    TransactionSigner,
};
use omega_security::BlueprintSigner;
use std::collections::HashMap;
use std::sync::Arc;

pub struct AwsKmsTransactionSigner {
    kms: AwsKmsSigner,
    execute: ExecuteCalldataBuilder,
}

impl AwsKmsTransactionSigner {
    /// # Panics
    /// Panics if `orchestrator == Address::ZERO`.
    pub fn new(
        kms: AwsKmsSigner,
        orchestrator: Address,
        strategy_onchain_ids: HashMap<String, [u8; 32]>,
        blueprint_signer: Arc<BlueprintSigner>,
    ) -> Self {
        Self {
            kms,
            execute: ExecuteCalldataBuilder::new(
                orchestrator,
                strategy_onchain_ids,
                blueprint_signer,
            ),
        }
    }

    pub fn active_address(&self) -> [u8; 20] {
        self.kms.address()
    }

    pub fn orchestrator(&self) -> Address {
        self.execute.orchestrator()
    }

    pub async fn sign_raw_envelope(
        &self,
        chain_id: u64,
        nonce: u64,
        max_priority_fee_per_gas: U256,
        max_fee_per_gas: U256,
        gas_limit: u64,
        to: Address,
        value: U256,
        data: &[u8],
    ) -> Result<SignedTransaction, ExecutionError> {
        let raw = sign_eip1559_envelope(
            &self.kms,
            chain_id,
            nonce,
            max_priority_fee_per_gas,
            max_fee_per_gas,
            gas_limit,
            to,
            value,
            data,
        )
        .await
        .map_err(|e| ExecutionError::SigningFailed {
            detail: e.to_string(),
        })?;

        Ok(SignedTransaction {
            raw_tx_hex: format!("0x{}", hex::encode(raw)),
        })
    }
}

#[async_trait]
impl TransactionSigner for AwsKmsTransactionSigner {
    async fn sign_transaction(
        &self,
        bp: &ExecutionBlueprint,
        chain_id: u64,
    ) -> Result<SignedTransaction, ExecutionError> {
        let _ = self
            .execute
            .validate_blueprint_for_signing(bp, chain_id)?;

        let data = self.execute.build_execute_calldata(bp, chain_id)?;
        let gas_limit = bp.total_l2_gas_budget();
        let (max_priority_fee_per_gas, max_fee_per_gas) =
            envelope_fees_wei(bp.base_fee_at_creation, bp.priority_fee_gwei)?;

        self.sign_raw_envelope(
            chain_id,
            bp.nonce,
            max_priority_fee_per_gas,
            max_fee_per_gas,
            gas_limit,
            self.execute.orchestrator(),
            U256::ZERO,
            data.as_ref(),
        )
        .await
    }
}