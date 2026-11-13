// crates/omega-strategies/src/msa.rs
//
// Multi-hop Sandwich Arbitrage (MSA) — Phase 2 strategy (spec §1.1, §3, §9).
//
// ## Production fixes (this revision)
//
// ### 1. Fabricated profit estimation — FIXED
// ### 2. Stub calldata — FIXED
// ### 3. Stub simulation — FIXED
// ### 4. max_base_fee floor
// ### 5. Test expectation corrected (2026-11-13)
//
// `competition_uses_base_rate` asserted 0.35, which is the SA base rate in
// omega-risk. The SandwichArbitrage class base rate is 0.70
// (omega-risk `msa_base_competition_is_70pct`). The test now expects 0.70.
// No production-code change.
//
// ## Competition model
//
// `SignalState` no longer carries `mev_share_events_in_window`. The event count
// is passed as 0 (SandwichArbitrage base rate 0.70, below the 0.75 skip
// threshold). The skip branch (competition > 0.75) is retained for future use
// when a replacement side-channel is wired in.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
use omega_core::types::lane::{Lane, Simulator};
use omega_core::types::strategy::{OpScore, SignalState, SimResult, StrategyTrait};
use omega_core::{GasConfig, OmegaConfig};
use omega_flashloan::LiquidityRegistry;
use omega_risk::competition::{self, StrategyCompetitionClass};

use crate::flashloan_select::to_blueprint_provider_type;
use crate::oracle::{NoOpSandwichOracleSource, SandwichOracleSource};
use crate::simulator::SimulatorBackend;

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

const MSA_GAS_BUDGET: u64 = 400_000;
const MSA_EXTRACTION_GAS: u64 = 21_000;
const MSA_L1_DATA_GAS: u64 = 8_000;
const MSA_EXPIRY_BLOCKS: u64 = 1;
const MSA_SLIPPAGE_BPS: u16 = 50;
const MSA_CONFIRMATION: u8 = 12;
const COMPETITION_SKIP_THRESHOLD: f64 = 0.75;
const MAX_BASE_FEE_HEADROOM_MULTIPLIER: f64 = 3.0;

const ARBITRUM_WETH: Address = Address::new([
    0x82, 0xaf, 0x49, 0x44, 0x7d, 0x8a, 0x07, 0xe3, 0xbd, 0x95, 0xbd, 0x0d, 0x56, 0xf3, 0x52, 0x41,
    0x52, 0x3f, 0xba, 0xb1,
]);

// ─────────────────────────────────────────────────────────────────────────────
// MsaStrategy
// ─────────────────────────────────────────────────────────────────────────────

pub struct MsaStrategy {
    chain_id: u64,
    nonce: AtomicU64,
    bytecode_hash: B256,
    #[allow(dead_code)]
    contract_addr: Address,
    liquidity_registry: Arc<LiquidityRegistry>,
    oracle_source: Arc<dyn SandwichOracleSource>,
    simulator: Option<Arc<dyn SimulatorBackend>>,
    gas: GasConfig,
}

impl MsaStrategy {
    pub fn new(
        chain_id: u64,
        bytecode_hash: B256,
        contract_addr: Address,
        liquidity_registry: Arc<LiquidityRegistry>,
        config: &OmegaConfig,
        oracle_source: Arc<dyn SandwichOracleSource>,
        simulator: Option<Arc<dyn SimulatorBackend>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            nonce: AtomicU64::new(1),
            bytecode_hash,
            contract_addr,
            liquidity_registry,
            oracle_source,
            simulator,
            gas: config.gas.clone(),
        })
    }

    pub fn new_no_oracle(
        chain_id: u64,
        bytecode_hash: B256,
        contract_addr: Address,
        liquidity_registry: Arc<LiquidityRegistry>,
        config: &OmegaConfig,
    ) -> Arc<Self> {
        Self::new(
            chain_id,
            bytecode_hash,
            contract_addr,
            liquidity_registry,
            config,
            Arc::new(NoOpSandwichOracleSource),
            None,
        )
    }

    fn dynamic_min_profit(&self, signal: &SignalState) -> U256 {
        let l2 = MSA_GAS_BUDGET as f64
            * signal.base_fee_gwei as f64
            * self.gas.l2_buffer_factor
            * 1e9_f64;
        let l1 = MSA_L1_DATA_GAS as f64
            * signal.l1_data_fee_gwei as f64
            * self.gas.l1_data_buffer_factor
            * 1e9_f64;
        U256::from((l2 + l1) as u128)
    }

    fn encode_sandwich_calldata(
        pools: &[Address],
        tokens: &[Address],
        front_amounts: &[U256],
        back_amounts: &[U256],
        min_profit: U256,
        target_tx_hash: B256,
    ) -> Bytes {
        let selector =
            &keccak256(b"executeSandwich(address[],address[],uint256[],uint256[],uint256,bytes32)")
                [..4];

        let n_pools = pools.len() as u64;
        let n_tokens = tokens.len() as u64;
        let n_front = front_amounts.len() as u64;
        let n_back = back_amounts.len() as u64;

        let head: u64 = 6 * 32;
        let pools_off = head;
        let tok_off = pools_off + 32 + n_pools * 32;
        let front_off = tok_off + 32 + n_tokens * 32;
        let back_off = front_off + 32 + n_front * 32;

        let mut data = Vec::with_capacity(
            4 + (head as usize)
                + (32 + n_pools as usize * 32)
                + (32 + n_tokens as usize * 32)
                + (32 + n_front as usize * 32)
                + (32 + n_back as usize * 32),
        );
        data.extend_from_slice(selector);

        data.extend_from_slice(&U256::from(pools_off).to_be_bytes::<32>());
        data.extend_from_slice(&U256::from(tok_off).to_be_bytes::<32>());
        data.extend_from_slice(&U256::from(front_off).to_be_bytes::<32>());
        data.extend_from_slice(&U256::from(back_off).to_be_bytes::<32>());
        data.extend_from_slice(&min_profit.to_be_bytes::<32>());
        data.extend_from_slice(target_tx_hash.as_slice());

        data.extend_from_slice(&U256::from(n_pools).to_be_bytes::<32>());
        for a in pools {
            let mut w = [0u8; 32];
            w[12..].copy_from_slice(a.as_slice());
            data.extend_from_slice(&w);
        }

        data.extend_from_slice(&U256::from(n_tokens).to_be_bytes::<32>());
        for a in tokens {
            let mut w = [0u8; 32];
            w[12..].copy_from_slice(a.as_slice());
            data.extend_from_slice(&w);
        }

        data.extend_from_slice(&U256::from(n_front).to_be_bytes::<32>());
        for v in front_amounts {
            data.extend_from_slice(&v.to_be_bytes::<32>());
        }

        data.extend_from_slice(&U256::from(n_back).to_be_bytes::<32>());
        for v in back_amounts {
            data.extend_from_slice(&v.to_be_bytes::<32>());
        }

        Bytes::from(data)
    }
}

#[async_trait]
impl StrategyTrait for MsaStrategy {
    fn strategy_id(&self) -> StrategyId {
        StrategyId::Msa
    }

    fn lane(&self) -> Lane {
        Lane::Normal
    }

    fn hot_path_eligible(&self) -> bool {
        false
    }

    fn gas_budget(&self) -> u64 {
        MSA_GAS_BUDGET
    }

    fn base_min_profit_wei(&self) -> U256 {
        U256::from(3_000_000_000_000_000_u128)
    }

    fn expected_bytecode_hash(&self) -> B256 {
        self.bytecode_hash
    }

    async fn score(&self, signal: &SignalState) -> Result<OpScore> {
        let competition_prob = competition::non_la_competition_probability(
            StrategyCompetitionClass::SandwichArbitrage,
            0,
        );
        if competition_prob > COMPETITION_SKIP_THRESHOLD {
            return Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob,
            });
        }

        let Some(opp) = self.oracle_source.current_opportunity() else {
            return Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob,
            });
        };

        let min_profit = self.dynamic_min_profit(signal);
        if opp.gross_profit_wei <= min_profit {
            return Ok(OpScore {
                score: 0.0,
                expected_profit: opp.gross_profit_wei,
                competition_prob,
            });
        }

        let profit_f = u128::try_from(opp.gross_profit_wei).unwrap_or(u128::MAX) as f64;
        let min_f = u128::try_from(min_profit).unwrap_or(u128::MAX) as f64;
        let raw = (profit_f / (min_f * 2.0)).min(1.0);
        let adj = (raw * (1.0 - competition_prob)).clamp(0.0, 1.0);

        tracing::debug!(
            chain_id = signal.chain_id,
            score = adj,
            competition_prob,
            target_tx = %opp.target_tx_hash,
            "MSA opportunity scored",
        );

        Ok(OpScore {
            score: adj,
            expected_profit: opp.gross_profit_wei,
            competition_prob,
        })
    }

    async fn build_blueprint(&self, signal: &SignalState) -> Result<ExecutionBlueprint> {
        let opp = self.oracle_source.current_opportunity().ok_or_else(|| {
            anyhow::anyhow!("MSA: no sandwich opportunity currently tracked — refusing blueprint")
        })?;

        anyhow::ensure!(
            !opp.pool_addresses.is_empty(),
            "MSA: oracle returned opportunity with empty pool list"
        );
        anyhow::ensure!(
            opp.token_path.len() == opp.pool_addresses.len() + 1,
            "MSA: token_path length must be pool_addresses.len() + 1"
        );
        anyhow::ensure!(
            opp.front_run_amounts.len() == opp.pool_addresses.len(),
            "MSA: front_run_amounts length must equal pool_addresses.len()"
        );
        anyhow::ensure!(
            opp.back_run_amounts.len() == opp.pool_addresses.len(),
            "MSA: back_run_amounts length must equal pool_addresses.len()"
        );

        let nonce = self.nonce.fetch_add(1, Ordering::Relaxed);
        let min_profit = self.dynamic_min_profit(signal);

        anyhow::ensure!(
            opp.gross_profit_wei > min_profit,
            "MSA: opportunity no longer profitable (gross={}, min={})",
            opp.gross_profit_wei,
            min_profit
        );

        let calldata = Self::encode_sandwich_calldata(
            &opp.pool_addresses,
            &opp.token_path,
            &opp.front_run_amounts,
            &opp.back_run_amounts,
            min_profit,
            opp.target_tx_hash,
        );

        let flashloan_token = ARBITRUM_WETH;
        let flashloan_amount = opp.gross_profit_wei;
        anyhow::ensure!(
            !flashloan_amount.is_zero(),
            "MSA: refusing zero flashloan_amount"
        );

        let selection = omega_flashloan::select_provider(
            &self.liquidity_registry,
            self.chain_id,
            flashloan_token,
            flashloan_amount,
        )
        .map_err(|e| anyhow::anyhow!("MSA: flashloan selection failed: {e:?}"))?;

        let signal_id = Uuid::new_v4();
        let client_order_id = ExecutionBlueprint::derive_client_order_id(
            StrategyId::Msa,
            self.chain_id,
            nonce,
            signal_id,
        );

        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO,
            chain_id: self.chain_id,
            strategy_id: StrategyId::Msa,
            lane: Lane::Normal,
            simulator: Simulator::Anvil,
            signal_state_hash: signal.state_hash,
            state_version: signal.state_version,
            signal_id,
            flashloan_provider: selection.contract_addr,
            flashloan_amount,
            flashloan_available: selection.available_wei,
            flashloan_provider_type: to_blueprint_provider_type(selection.provider),
            provider_contract: selection.contract_addr,
            flashloan_token,
            max_base_fee_gwei: ExecutionBlueprint::derive_max_base_fee_gwei(
                signal.base_fee_gwei,
                MAX_BASE_FEE_HEADROOM_MULTIPLIER,
            ),
            calldata,
            strategy_bytecode_hash: self.bytecode_hash,
            l2_exec_gas_estimate: MSA_GAS_BUDGET,
            l1_data_gas_estimate: MSA_L1_DATA_GAS,
            extraction_gas: MSA_EXTRACTION_GAS,
            expected_profit_net: opp.gross_profit_wei,
            dynamic_min_profit: min_profit,
            l2_buffer_factor: self.gas.l2_buffer_factor,
            l1_data_buffer_factor: self.gas.l1_data_buffer_factor,
            slippage_bps: MSA_SLIPPAGE_BPS,
            base_fee_at_creation: signal.base_fee_gwei,
            l1_data_fee_at_creation: signal.l1_data_fee_gwei,
            priority_fee_gwei: self.gas.max_priority_fee_gwei,
            price_impact_bps: Some(100),
            ofa_compliant: false,
            expiry_block: signal.block_number + MSA_EXPIRY_BLOCKS,
            nonce,
            confirmation_depth: MSA_CONFIRMATION,
            client_order_id,
            idempotency_key: B256::ZERO,
            relay_targets: vec!["default_relay".to_string()],
            zk_proof_commitment: None,
        };
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();

        anyhow::ensure!(
            bp.is_profitable(),
            "MSA blueprint unprofitable: profit={} < min={}",
            bp.expected_profit_net,
            bp.dynamic_min_profit
        );

        tracing::debug!(
            blueprint_hash = %bp.blueprint_hash,
            nonce,
            expected_profit = %bp.expected_profit_net,
            "MSA blueprint built",
        );
        Ok(bp)
    }

    async fn simulate(&self, bp: &ExecutionBlueprint) -> Result<SimResult> {
        assert_eq!(bp.simulator, Simulator::Anvil);
        match &self.simulator {
            Some(backend) => backend.simulate(bp).await,
            None => anyhow::bail!(
                "MSA: no SimulatorBackend injected — cannot simulate blueprint {}; \
                 pass a real Anvil backend via MsaStrategy::new()",
                bp.blueprint_hash
            ),
        }
    }

    fn encode_calldata(&self, bp: &ExecutionBlueprint) -> Bytes {
        bp.calldata.clone()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oracle::{SandwichOpportunity, StaticSandwichOracleSource};
    use omega_core::OmegaConfig;

    fn addr(b: u8) -> Address {
        Address::from([b; 20])
    }

    fn test_opp(profit_wei: u128) -> SandwichOpportunity {
        SandwichOpportunity {
            gross_profit_wei: U256::from(profit_wei),
            target_tx_hash: B256::from([0xAA; 32]),
            pool_addresses: vec![addr(0xA1), addr(0xA2)],
            token_path: vec![addr(0xB1), addr(0xB2), addr(0xB3)],
            front_run_amounts: vec![U256::from(1_000_000u64), U256::from(900_000u64)],
            back_run_amounts: vec![U256::from(950_000u64), U256::from(1_050_000u64)],
        }
    }

    fn make_strategy_with_opp(opp: SandwichOpportunity) -> Arc<MsaStrategy> {
        let lr = LiquidityRegistry::new();
        lr.update(
            42161,
            omega_flashloan::FlashloanProvider::Balancer,
            ARBITRUM_WETH,
            Address::from([0xB0; 20]),
            U256::from(10_000_000_000_000_000_000u128),
            1,
        );
        MsaStrategy::new(
            42161,
            B256::from([0xCC; 32]),
            Address::from([0x22; 20]),
            lr,
            &OmegaConfig::default(),
            Arc::new(StaticSandwichOracleSource(opp)),
            None,
        )
    }

    fn make_no_oracle() -> Arc<MsaStrategy> {
        let lr = LiquidityRegistry::new();
        lr.update(
            42161,
            omega_flashloan::FlashloanProvider::Balancer,
            ARBITRUM_WETH,
            Address::from([0xB0; 20]),
            U256::from(10_000_000_000_000_000_000u128),
            1,
        );
        MsaStrategy::new_no_oracle(
            42161,
            B256::from([0xCC; 32]),
            Address::from([0x22; 20]),
            lr,
            &OmegaConfig::default(),
        )
    }

    fn sig(base_fee: u64) -> SignalState {
        SignalState {
            state_version: 7,
            chain_id: 42161,
            block_number: 2_500_000,
            base_fee_gwei: base_fee,
            l1_data_fee_gwei: 2,
            state_hash: B256::from([0x07; 32]),
        }
    }

    #[tokio::test]
    async fn score_zero_no_oracle_opportunity() {
        let op = make_no_oracle().score(&sig(10)).await.unwrap();
        assert_eq!(op.score, 0.0);
    }

    #[tokio::test]
    async fn score_nonzero_with_profitable_opportunity() {
        let s = make_strategy_with_opp(test_opp(50_000_000_000_000_000));
        let op = s.score(&sig(10)).await.unwrap();
        assert!(
            op.score > 0.0,
            "profitable opportunity must produce nonzero score"
        );
    }

    #[tokio::test]
    async fn score_zero_when_profit_below_min() {
        let s = make_strategy_with_opp(test_opp(1));
        let op = s.score(&sig(10)).await.unwrap();
        assert_eq!(op.score, 0.0);
    }

    #[tokio::test]
    async fn competition_uses_base_rate() {
        let s = make_strategy_with_opp(test_opp(50_000_000_000_000_000));
        let op = s.score(&sig(10)).await.unwrap();
        // SandwichArbitrage base rate in omega-risk is 0.70 (0.35 is SA's).
        assert!(
            (op.competition_prob - 0.70).abs() < 1e-9,
            "expected base rate 0.70, got {}",
            op.competition_prob
        );
        assert!(
            op.competition_prob <= COMPETITION_SKIP_THRESHOLD,
            "base rate must stay under the skip threshold or MSA could never score"
        );
    }

    #[tokio::test]
    async fn build_blueprint_fails_no_oracle() {
        let err = make_no_oracle()
            .build_blueprint(&sig(10))
            .await
            .expect_err("must fail");
        assert!(err.to_string().contains("no sandwich opportunity"));
    }

    #[tokio::test]
    async fn build_blueprint_produces_valid_blueprint() {
        let s = make_strategy_with_opp(test_opp(50_000_000_000_000_000));
        let bp = s.build_blueprint(&sig(10)).await.unwrap();
        assert_eq!(bp.strategy_id, StrategyId::Msa);
        assert_eq!(bp.simulator, Simulator::Anvil);
        assert!(!bp.ofa_compliant);
        assert!(!bp.blueprint_hash.is_zero());
        assert!(bp.verify_hash());
        assert!(bp.verify_idempotency_key());
        assert_ne!(bp.flashloan_token, Address::ZERO);
        assert!(!bp.flashloan_amount.is_zero());
    }

    #[tokio::test]
    async fn build_blueprint_calldata_has_correct_selector() {
        let s = make_strategy_with_opp(test_opp(50_000_000_000_000_000));
        let bp = s.build_blueprint(&sig(10)).await.unwrap();
        let expected_sel =
            &keccak256(b"executeSandwich(address[],address[],uint256[],uint256[],uint256,bytes32)")
                [..4];
        assert_eq!(&bp.calldata[..4], expected_sel);
    }

    #[tokio::test]
    async fn simulate_fails_without_backend() {
        let s = make_strategy_with_opp(test_opp(50_000_000_000_000_000));
        let bp = s.build_blueprint(&sig(10)).await.unwrap();
        let err = s
            .simulate(&bp)
            .await
            .expect_err("must fail without backend");
        assert!(format!("{err:?}").contains("no SimulatorBackend"));
    }

    #[tokio::test]
    async fn build_fails_closed_without_liquidity() {
        let empty_lr = LiquidityRegistry::new();
        let s = MsaStrategy::new(
            42161,
            B256::from([0xCC; 32]),
            Address::from([0x22; 20]),
            empty_lr,
            &OmegaConfig::default(),
            Arc::new(StaticSandwichOracleSource(test_opp(50_000_000_000_000_000))),
            None,
        );
        let err = s
            .build_blueprint(&sig(10))
            .await
            .expect_err("empty registry must fail closed");
        let msg = format!("{err:?}");
        assert!(msg.contains("flashloan selection failed") || msg.contains("NoneAvailable"));
    }

    #[tokio::test]
    async fn nonce_is_monotonically_increasing() {
        let s = make_strategy_with_opp(test_opp(50_000_000_000_000_000));
        let bp1 = s.build_blueprint(&sig(10)).await.unwrap();
        let bp2 = s.build_blueprint(&sig(10)).await.unwrap();
        assert!(bp2.nonce > bp1.nonce);
    }

    #[tokio::test]
    async fn max_base_fee_never_zero_at_sub_gwei() {
        let s = make_strategy_with_opp(test_opp(50_000_000_000_000_000));
        let mut signal = sig(10);
        signal.base_fee_gwei = 0;
        let bp = s.build_blueprint(&signal).await.unwrap();
        assert!(bp.max_base_fee_gwei >= ExecutionBlueprint::MIN_MAX_BASE_FEE_GWEI);
        assert!(bp.verify_hash());
    }

    #[test]
    fn strategy_metadata() {
        let s = make_no_oracle();
        assert_eq!(s.strategy_id(), StrategyId::Msa);
        assert_eq!(s.lane(), Lane::Normal);
        assert!(!s.hot_path_eligible());
        assert_eq!(s.gas_budget(), MSA_GAS_BUDGET);
    }

    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn msa_slippage_within_known_risk_policy_cap() {
        const MIRRORED: u16 = 50;
        assert!(
            MSA_SLIPPAGE_BPS <= MIRRORED,
            "MSA_SLIPPAGE_BPS ({MSA_SLIPPAGE_BPS}) exceeds mirrored risk cap ({MIRRORED})"
        );
    }
}
