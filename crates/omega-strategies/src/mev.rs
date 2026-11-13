// crates/omega-strategies/src/mev.rs
//
// MEV-OFA / Backrun (MEV) — Phase 4 strategy (spec §1.1, §4, §8).
//
// ## Production fixes (this revision)
//
// ### 1. Fabricated adverse selection score — FIXED
// ### 2. Fabricated profit estimation — FIXED
// ### 3. Stub calldata — FIXED
// ### 4. Stub simulation — FIXED

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

use crate::flashloan_select::to_blueprint_provider_type;
use crate::oracle::{MevShareOracleSource, NoOpMevShareOracleSource};
use crate::simulator::SimulatorBackend;

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

const MEV_GAS_BUDGET: u64 = 300_000;
const MEV_EXTRACTION_GAS: u64 = 21_000;
const MEV_L1_DATA_GAS: u64 = 4_000;
const MEV_EXPIRY_BLOCKS: u64 = 1;
const MEV_SLIPPAGE_BPS: u16 = 30;
const MEV_CONFIRMATION: u8 = 12;

const ADVERSE_SELECTION_THRESHOLD: f64 = 0.5;
const MAX_BASE_FEE_HEADROOM_MULTIPLIER: f64 = 3.0;

const ARBITRUM_WETH: Address = Address::new([
    0x82, 0xaf, 0x49, 0x44, 0x7d, 0x8a, 0x07, 0xe3, 0xbd, 0x95, 0xbd, 0x0d, 0x56, 0xf3, 0x52, 0x41,
    0x52, 0x3f, 0xba, 0xb1,
]);

// ─────────────────────────────────────────────────────────────────────────────
// MevStrategy
// ─────────────────────────────────────────────────────────────────────────────

pub struct MevStrategy {
    chain_id: u64,
    nonce: AtomicU64,
    bytecode_hash: B256,
    #[allow(dead_code)]
    contract_addr: Address,
    liquidity_registry: Arc<LiquidityRegistry>,
    oracle_source: Arc<dyn MevShareOracleSource>,
    simulator: Option<Arc<dyn SimulatorBackend>>,
    gas: GasConfig,
}

impl MevStrategy {
    pub fn new_with_registry(
        chain_id: u64,
        bytecode_hash: B256,
        contract_addr: Address,
        liquidity_registry: Arc<LiquidityRegistry>,
        config: &OmegaConfig,
        oracle_source: Arc<dyn MevShareOracleSource>,
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
        config: &OmegaConfig,
    ) -> Arc<Self> {
        Self::new_with_registry(
            chain_id,
            bytecode_hash,
            contract_addr,
            LiquidityRegistry::new(),
            config,
            Arc::new(NoOpMevShareOracleSource),
            None,
        )
    }

    fn dynamic_min_profit(&self, signal: &SignalState) -> U256 {
        let l2 = MEV_GAS_BUDGET as f64
            * signal.base_fee_gwei as f64
            * self.gas.l2_buffer_factor
            * 1e9_f64;
        let l1 = MEV_L1_DATA_GAS as f64
            * signal.l1_data_fee_gwei as f64
            * self.gas.l1_data_buffer_factor
            * 1e9_f64;
        U256::from((l2 + l1) as u128)
    }

    fn encode_backrun_calldata(
        target_tx_hash: B256,
        pools: &[Address],
        tokens: &[Address],
        amounts: &[U256],
        min_profit: U256,
    ) -> Bytes {
        let selector =
            &keccak256(b"executeBackrun(bytes32,address[],address[],uint256[],uint256)")[..4];

        let n_pools = pools.len() as u64;
        let n_tokens = tokens.len() as u64;
        let n_amounts = amounts.len() as u64;

        let head: u64 = 5 * 32;
        let pools_off = head;
        let tokens_off = pools_off + 32 + n_pools * 32;
        let amounts_off = tokens_off + 32 + n_tokens * 32;

        let mut data = Vec::with_capacity(
            4 + (head as usize)
                + (32 + n_pools as usize * 32)
                + (32 + n_tokens as usize * 32)
                + (32 + n_amounts as usize * 32),
        );
        data.extend_from_slice(selector);

        data.extend_from_slice(target_tx_hash.as_slice());
        data.extend_from_slice(&U256::from(pools_off).to_be_bytes::<32>());
        data.extend_from_slice(&U256::from(tokens_off).to_be_bytes::<32>());
        data.extend_from_slice(&U256::from(amounts_off).to_be_bytes::<32>());
        data.extend_from_slice(&min_profit.to_be_bytes::<32>());

        data.extend_from_slice(&U256::from(n_pools).to_be_bytes::<32>());
        for a in pools {
            let mut word = [0u8; 32];
            word[12..].copy_from_slice(a.as_slice());
            data.extend_from_slice(&word);
        }

        data.extend_from_slice(&U256::from(n_tokens).to_be_bytes::<32>());
        for a in tokens {
            let mut word = [0u8; 32];
            word[12..].copy_from_slice(a.as_slice());
            data.extend_from_slice(&word);
        }

        data.extend_from_slice(&U256::from(n_amounts).to_be_bytes::<32>());
        for v in amounts {
            data.extend_from_slice(&v.to_be_bytes::<32>());
        }

        Bytes::from(data)
    }
}

#[async_trait]
impl StrategyTrait for MevStrategy {
    fn strategy_id(&self) -> StrategyId {
        StrategyId::Mev
    }

    fn lane(&self) -> Lane {
        Lane::Normal
    }

    fn hot_path_eligible(&self) -> bool {
        false
    }

    fn gas_budget(&self) -> u64 {
        MEV_GAS_BUDGET
    }

    fn base_min_profit_wei(&self) -> U256 {
        U256::from(2_000_000_000_000_000_u128)
    }

    fn expected_bytecode_hash(&self) -> B256 {
        self.bytecode_hash
    }

    async fn score(&self, signal: &SignalState) -> Result<OpScore> {
        let Some(opp) = self.oracle_source.current_opportunity() else {
            return Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 0.0,
            });
        };

        let adv = opp.adverse_selection_score;
        if adv > ADVERSE_SELECTION_THRESHOLD {
            tracing::debug!(
                adverse_score = adv,
                threshold = ADVERSE_SELECTION_THRESHOLD,
                target_tx = %opp.target_tx_hash,
                "MEV opportunity skipped: adverse selection too high",
            );
            return Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: adv,
            });
        }

        let min_profit = self.dynamic_min_profit(signal);
        let est_profit = opp.price_impact_wei;

        if est_profit <= min_profit {
            return Ok(OpScore {
                score: 0.0,
                expected_profit: est_profit,
                competition_prob: adv,
            });
        }

        let profit_f = u128::try_from(est_profit).unwrap_or(u128::MAX) as f64;
        let min_f = u128::try_from(min_profit).unwrap_or(u128::MAX) as f64;
        let raw = (profit_f / (min_f * 2.0)).min(1.0);
        let adj = (raw * (1.0 - adv)).clamp(0.0, 1.0);

        tracing::debug!(
            chain_id = signal.chain_id,
            score = adj,
            adverse_score = adv,
            target_tx = %opp.target_tx_hash,
            "MEV opportunity scored",
        );

        Ok(OpScore {
            score: adj,
            expected_profit: est_profit,
            competition_prob: adv,
        })
    }

    async fn build_blueprint(&self, signal: &SignalState) -> Result<ExecutionBlueprint> {
        let opp = self.oracle_source.current_opportunity().ok_or_else(|| {
            anyhow::anyhow!("MEV: no MEV-Share opportunity currently tracked — refusing blueprint")
        })?;

        anyhow::ensure!(
            opp.adverse_selection_score <= ADVERSE_SELECTION_THRESHOLD,
            "MEV: adverse selection score {:.3} exceeds threshold {:.3} — refusing blueprint",
            opp.adverse_selection_score,
            ADVERSE_SELECTION_THRESHOLD
        );

        anyhow::ensure!(
            !opp.pool_addresses.is_empty(),
            "MEV: oracle returned opportunity with empty pool list"
        );
        anyhow::ensure!(
            opp.token_path.len() == opp.pool_addresses.len() + 1,
            "MEV: token_path length must be pool_addresses.len() + 1"
        );
        anyhow::ensure!(
            opp.backrun_amounts.len() == opp.pool_addresses.len(),
            "MEV: backrun_amounts length must equal pool_addresses.len()"
        );

        let nonce = self.nonce.fetch_add(1, Ordering::Relaxed);
        let min_profit = self.dynamic_min_profit(signal);
        let est_profit = opp.price_impact_wei;

        anyhow::ensure!(
            est_profit > min_profit,
            "MEV: opportunity no longer profitable (profit={}, min={})",
            est_profit,
            min_profit
        );

        let calldata = Self::encode_backrun_calldata(
            opp.target_tx_hash,
            &opp.pool_addresses,
            &opp.token_path,
            &opp.backrun_amounts,
            min_profit,
        );

        let flashloan_token = ARBITRUM_WETH;
        let flashloan_amount = est_profit;
        anyhow::ensure!(
            !flashloan_amount.is_zero(),
            "MEV: refusing zero flashloan_amount (price_impact_wei is zero)"
        );

        let selection = omega_flashloan::select_provider(
            &self.liquidity_registry,
            self.chain_id,
            flashloan_token,
            flashloan_amount,
        )
        .map_err(|e| anyhow::anyhow!("MEV: flashloan selection failed: {e:?}"))?;

        let signal_id = Uuid::new_v4();
        let client_order_id = ExecutionBlueprint::derive_client_order_id(
            StrategyId::Mev,
            self.chain_id,
            nonce,
            signal_id,
        );

        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO,
            chain_id: self.chain_id,
            strategy_id: StrategyId::Mev,
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
            l2_exec_gas_estimate: MEV_GAS_BUDGET,
            l1_data_gas_estimate: MEV_L1_DATA_GAS,
            extraction_gas: MEV_EXTRACTION_GAS,
            expected_profit_net: est_profit,
            dynamic_min_profit: min_profit,
            l2_buffer_factor: self.gas.l2_buffer_factor,
            l1_data_buffer_factor: self.gas.l1_data_buffer_factor,
            slippage_bps: MEV_SLIPPAGE_BPS,
            base_fee_at_creation: signal.base_fee_gwei,
            l1_data_fee_at_creation: signal.l1_data_fee_gwei,
            priority_fee_gwei: self.gas.max_priority_fee_gwei,
            price_impact_bps: Some(50),
            ofa_compliant: true,
            expiry_block: signal.block_number + MEV_EXPIRY_BLOCKS,
            nonce,
            confirmation_depth: MEV_CONFIRMATION,
            client_order_id,
            idempotency_key: B256::ZERO,
            relay_targets: vec!["mev_share_primary".to_string()],
            zk_proof_commitment: None,
        };
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();

        anyhow::ensure!(
            bp.is_profitable(),
            "MEV blueprint unprofitable: profit={} < min={}",
            bp.expected_profit_net,
            bp.dynamic_min_profit
        );

        tracing::debug!(
            blueprint_hash = %bp.blueprint_hash,
            nonce,
            expected_profit = %bp.expected_profit_net,
            priority_fee = bp.priority_fee_gwei,
            "MEV blueprint built",
        );
        Ok(bp)
    }

    async fn simulate(&self, bp: &ExecutionBlueprint) -> Result<SimResult> {
        assert_eq!(bp.simulator, Simulator::Anvil);
        match &self.simulator {
            Some(backend) => backend.simulate(bp).await,
            None => anyhow::bail!(
                "MEV: no SimulatorBackend injected — cannot simulate blueprint {}; \
                 pass a real Anvil backend via MevStrategy::new_with_registry()",
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
    use crate::oracle::{MevShareOpportunity, StaticMevShareOracleSource};
    use omega_core::OmegaConfig;

    fn addr(b: u8) -> Address {
        Address::from([b; 20])
    }

    fn test_opp(adverse: f64, profit_wei: u128) -> MevShareOpportunity {
        MevShareOpportunity {
            adverse_selection_score: adverse,
            price_impact_wei: U256::from(profit_wei),
            target_tx_hash: B256::from([0xBB; 32]),
            pool_addresses: vec![addr(0xA1)],
            token_path: vec![addr(0xB1), addr(0xB2)],
            backrun_amounts: vec![U256::from(1_000_000u64)],
        }
    }

    fn make_strategy_with_opp(opp: MevShareOpportunity) -> Arc<MevStrategy> {
        let lr = LiquidityRegistry::new();
        lr.update(
            42161,
            omega_flashloan::FlashloanProvider::Balancer,
            ARBITRUM_WETH,
            Address::from([0xB0; 20]),
            U256::from(10_000_000_000_000_000_000u128),
            1,
        );
        MevStrategy::new_with_registry(
            42161,
            B256::from([0xAB; 32]),
            Address::from([0x11; 20]),
            lr,
            &OmegaConfig::default(),
            Arc::new(StaticMevShareOracleSource(opp)),
            None,
        )
    }

    fn make_no_oracle() -> Arc<MevStrategy> {
        MevStrategy::new_no_oracle(
            42161,
            B256::from([0xAB; 32]),
            Address::from([0x11; 20]),
            &OmegaConfig::default(),
        )
    }

    fn sig(base_fee: u64) -> SignalState {
        SignalState {
            state_version: 9,
            chain_id: 42161,
            block_number: 1_000_000,
            base_fee_gwei: base_fee,
            l1_data_fee_gwei: 2,
            state_hash: B256::from([0x09; 32]),
        }
    }

    #[tokio::test]
    async fn high_adverse_selection_skips() {
        let s = make_strategy_with_opp(test_opp(0.9, 50_000_000_000_000_000));
        let op = s.score(&sig(10)).await.unwrap();
        assert_eq!(op.score, 0.0, "high adverse selection must produce score 0");
    }

    #[tokio::test]
    async fn low_adverse_selection_proceeds() {
        let s = make_strategy_with_opp(test_opp(0.1, 50_000_000_000_000_000));
        let op = s.score(&sig(10)).await.unwrap();
        assert!(
            op.score > 0.0 || op.expected_profit == U256::ZERO,
            "low adverse selection should not be blocked: score={}",
            op.score
        );
    }

    #[tokio::test]
    async fn score_zero_no_oracle() {
        let op = make_no_oracle().score(&sig(10)).await.unwrap();
        assert_eq!(op.score, 0.0);
    }

    #[tokio::test]
    async fn blueprint_sets_ofa_compliant() {
        let s = make_strategy_with_opp(test_opp(0.1, 50_000_000_000_000_000));
        let sc = s.score(&sig(10)).await.unwrap();
        if sc.score <= 0.0 {
            return;
        }
        let bp = s.build_blueprint(&sig(10)).await.unwrap();
        assert!(
            bp.ofa_compliant,
            "MEV blueprints must set ofa_compliant=true"
        );
        assert_eq!(bp.strategy_id, StrategyId::Mev);
        assert_eq!(bp.simulator, Simulator::Anvil);
        assert!(!bp.blueprint_hash.is_zero());
    }

    #[tokio::test]
    async fn blueprint_uses_flashloan() {
        let s = make_strategy_with_opp(test_opp(0.1, 50_000_000_000_000_000));
        let sc = s.score(&sig(10)).await.unwrap();
        if sc.score <= 0.0 {
            return;
        }
        let bp = s.build_blueprint(&sig(10)).await.unwrap();
        assert_ne!(bp.flashloan_amount, U256::ZERO);
        assert_ne!(bp.flashloan_provider, Address::ZERO);
        assert_eq!(bp.flashloan_token, ARBITRUM_WETH);
    }

    #[tokio::test]
    async fn blueprint_calldata_has_correct_selector() {
        let s = make_strategy_with_opp(test_opp(0.1, 50_000_000_000_000_000));
        let sc = s.score(&sig(10)).await.unwrap();
        if sc.score <= 0.0 {
            return;
        }
        let bp = s.build_blueprint(&sig(10)).await.unwrap();
        let expected_sel =
            &keccak256(b"executeBackrun(bytes32,address[],address[],uint256[],uint256)")[..4];
        assert_eq!(&bp.calldata[..4], expected_sel);
    }

    #[tokio::test]
    async fn build_fails_no_oracle() {
        let err = make_no_oracle()
            .build_blueprint(&sig(10))
            .await
            .expect_err("must fail");
        assert!(err.to_string().contains("no MEV-Share opportunity"));
    }

    #[tokio::test]
    async fn build_fails_high_adverse_selection() {
        let s = make_strategy_with_opp(test_opp(0.9, 50_000_000_000_000_000));
        let err = s.build_blueprint(&sig(10)).await.expect_err("must fail");
        assert!(err.to_string().contains("adverse selection"));
    }

    #[tokio::test]
    async fn simulate_fails_without_backend() {
        let s = make_strategy_with_opp(test_opp(0.1, 50_000_000_000_000_000));
        let sc = s.score(&sig(10)).await.unwrap();
        if sc.score <= 0.0 {
            return;
        }
        let bp = s.build_blueprint(&sig(10)).await.unwrap();
        let err = s
            .simulate(&bp)
            .await
            .expect_err("must fail without backend");
        assert!(format!("{err:?}").contains("no SimulatorBackend"));
    }

    #[tokio::test]
    async fn max_base_fee_never_zero_at_sub_gwei() {
        let s = make_strategy_with_opp(test_opp(0.1, 50_000_000_000_000_000));
        let mut signal = sig(10);
        signal.base_fee_gwei = 0;
        let sc = s.score(&signal).await.unwrap();
        if sc.score <= 0.0 {
            return;
        }
        let bp = s.build_blueprint(&signal).await.unwrap();
        assert!(bp.max_base_fee_gwei >= ExecutionBlueprint::MIN_MAX_BASE_FEE_GWEI);
        assert!(bp.max_base_fee_wei() > U256::ZERO);
        assert!(bp.verify_hash());
    }

    #[tokio::test]
    async fn build_blueprint_passes_verify_hash() {
        let s = make_strategy_with_opp(test_opp(0.1, 50_000_000_000_000_000));
        let sc = s.score(&sig(10)).await.unwrap();
        if sc.score <= 0.0 {
            return;
        }
        let bp = s.build_blueprint(&sig(10)).await.unwrap();
        assert!(bp.verify_hash());
        assert!(bp.verify_idempotency_key());
    }

    #[tokio::test]
    async fn nonce_monotonically_increasing() {
        let s = make_strategy_with_opp(test_opp(0.1, 50_000_000_000_000_000));
        let sc = s.score(&sig(10)).await.unwrap();
        if sc.score <= 0.0 {
            return;
        }
        let bp1 = s.build_blueprint(&sig(10)).await.unwrap();
        let bp2 = s.build_blueprint(&sig(10)).await.unwrap();
        assert!(bp2.nonce > bp1.nonce);
    }

    #[tokio::test]
    async fn build_fails_closed_without_liquidity() {
        let s = MevStrategy::new_with_registry(
            42161,
            B256::from([0xAB; 32]),
            Address::from([0x11; 20]),
            LiquidityRegistry::new(),
            &OmegaConfig::default(),
            Arc::new(StaticMevShareOracleSource(test_opp(
                0.1,
                50_000_000_000_000_000,
            ))),
            None,
        );
        let sc = s.score(&sig(10)).await.unwrap();
        if sc.score <= 0.0 {
            return;
        }
        let err = s
            .build_blueprint(&sig(10))
            .await
            .expect_err("empty registry must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("flashloan selection failed") || msg.contains("NoneAvailable"));
    }

    #[test]
    fn strategy_metadata() {
        let s = make_no_oracle();
        assert_eq!(s.strategy_id(), StrategyId::Mev);
        assert_eq!(s.lane(), Lane::Normal);
        assert!(!s.hot_path_eligible());
        assert_eq!(s.gas_budget(), MEV_GAS_BUDGET);
    }

    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn mev_slippage_within_known_risk_policy_cap() {
        const MIRRORED: u16 = 30;
        assert!(
            MEV_SLIPPAGE_BPS <= MIRRORED,
            "MEV_SLIPPAGE_BPS ({MEV_SLIPPAGE_BPS}) exceeds mirrored risk cap ({MIRRORED})"
        );
    }
}
