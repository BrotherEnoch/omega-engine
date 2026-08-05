// crates/omega-flashloan/src/tests.rs
// FIX: module_inception — rename inner mod to avoid same name as containing module
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod flashloan_tests {
    use crate::encoding::encode_flashloan_call;
    use crate::{
        premium_wei, repayment_wei, select_provider, FlashloanError, FlashloanProvider,
        LiquidityRegistry, AAVE_V3_PREMIUM_BPS, BALANCER_PREMIUM_BPS, UNISWAP_V3_PREMIUM_BPS,
    };
    use alloy_primitives::{keccak256, Address, U256};
    use omega_core::errors::{DropCode, OmegaError};
    use std::sync::Arc;

    fn eth(n: u128) -> U256 {
        U256::from(n * 1_000_000_000_000_000_000_u128)
    }

    fn test_registry() -> Arc<LiquidityRegistry> {
        LiquidityRegistry::new()
    }

    fn addr(b: u8) -> Address {
        Address::from([b; 20])
    }

    #[test]
    fn aave_premium_9bps() {
        let amount = eth(10);
        let prem = premium_wei(FlashloanProvider::AaveV3, amount);
        assert_eq!(prem, U256::from(9_000_000_000_000_000_u128));
    }

    #[test]
    fn balancer_premium_zero() {
        let amount = eth(100);
        assert_eq!(premium_wei(FlashloanProvider::Balancer, amount), U256::ZERO);
    }

    #[test]
    fn uniswap_premium_30bps() {
        let amount = eth(10);
        let prem = premium_wei(FlashloanProvider::UniswapV3, amount);
        assert_eq!(prem, U256::from(30_000_000_000_000_000_u128));
    }

    #[test]
    fn repayment_is_principal_plus_premium() {
        let amount = eth(5);
        let prem = premium_wei(FlashloanProvider::AaveV3, amount);
        let repay = repayment_wei(FlashloanProvider::AaveV3, amount);
        assert_eq!(repay, amount + prem);
    }

    #[test]
    fn zero_amount_has_zero_premium() {
        assert_eq!(
            premium_wei(FlashloanProvider::AaveV3, U256::ZERO),
            U256::ZERO
        );
    }

    #[test]
    fn update_and_snapshot_fresh() {
        let reg = test_registry();
        reg.update(42161, FlashloanProvider::AaveV3, addr(0xAA), eth(50), 1_000);
        let snap = reg.snapshot(42161, FlashloanProvider::AaveV3, addr(0xAA));
        assert!(snap.is_some());
        assert_eq!(snap.unwrap().available_wei, eth(50));
    }

    #[test]
    fn unknown_provider_returns_none() {
        let reg = test_registry();
        let snap = reg.snapshot(42161, FlashloanProvider::Balancer, addr(0xBB));
        assert!(snap.is_none());
    }

    #[test]
    fn available_contracts_sorted_by_liquidity() {
        let reg = test_registry();
        reg.update(42161, FlashloanProvider::AaveV3, addr(0x01), eth(10), 1);
        reg.update(42161, FlashloanProvider::AaveV3, addr(0x02), eth(50), 1);
        reg.update(42161, FlashloanProvider::AaveV3, addr(0x03), eth(30), 1);
        let contracts = reg.available_contracts(42161, FlashloanProvider::AaveV3);
        assert_eq!(contracts.len(), 3);
        assert_eq!(contracts[0].1.available_wei, eth(50));
        assert_eq!(contracts[1].1.available_wei, eth(30));
        assert_eq!(contracts[2].1.available_wei, eth(10));
    }

    #[test]
    fn selects_balancer_over_aave_when_both_available() {
        let reg = test_registry();
        reg.update(42161, FlashloanProvider::Balancer, addr(0xB0), eth(100), 1);
        reg.update(42161, FlashloanProvider::AaveV3, addr(0xA0), eth(100), 1);
        let result = select_provider(&reg, 42161, eth(50)).unwrap();
        assert_eq!(
            result.provider,
            FlashloanProvider::Balancer,
            "Balancer (0 bps) must be preferred over Aave v3 (9 bps)"
        );
        assert_eq!(result.premium_wei, U256::ZERO);
    }

    #[test]
    fn falls_back_to_aave_when_balancer_insufficient() {
        let reg = test_registry();
        reg.update(42161, FlashloanProvider::Balancer, addr(0xB0), eth(10), 1);
        reg.update(42161, FlashloanProvider::AaveV3, addr(0xA0), eth(100), 1);
        let result = select_provider(&reg, 42161, eth(50)).unwrap();
        assert_eq!(result.provider, FlashloanProvider::AaveV3);
    }

    #[test]
    fn returns_none_available_when_all_insufficient() {
        let reg = test_registry();
        reg.update(42161, FlashloanProvider::AaveV3, addr(0xA0), eth(5), 1);
        let err = select_provider(&reg, 42161, eth(50)).unwrap_err();
        assert!(matches!(err, FlashloanError::NoneAvailable { .. }));
        if let FlashloanError::NoneAvailable {
            best_available_wei, ..
        } = err
        {
            assert_eq!(
                best_available_wei,
                eth(5),
                "best_available_wei must report the closest we got"
            );
        }
    }

    #[test]
    fn empty_registry_returns_none_available() {
        let reg = test_registry();
        let err = select_provider(&reg, 42161, eth(1)).unwrap_err();
        assert!(
            matches!(err, FlashloanError::NoneAvailable { best_available_wei: w, .. }
            if w == U256::ZERO)
        );
    }

    #[test]
    fn selection_result_contract_addr_matches_registry() {
        let reg = test_registry();
        let provider = addr(0xCC);
        reg.update(42161, FlashloanProvider::AaveV3, provider, eth(100), 1);
        let result = select_provider(&reg, 42161, eth(50)).unwrap();
        assert_eq!(result.contract_addr, provider);
    }

    #[test]
    fn error_maps_to_miss_flashloan_drop_code() {
        let err = FlashloanError::NoneAvailable {
            amount_wei: eth(1),
            chain_id: 42161,
            best_available_wei: U256::ZERO,
        };
        assert!(matches!(
            err.to_omega_error(),
            OmegaError::Dropped {
                code: DropCode::MissFlashloan
            }
        ));
    }

    #[test]
    fn aave_calldata_has_correct_selector() {
        let calldata = encode_flashloan_call(
            FlashloanProvider::AaveV3,
            addr(0xAA),
            addr(0xBB),
            addr(0xCC),
            eth(10),
            true,
            b"callback",
        );
        let expected_selector =
            &keccak256(b"flashLoanSimple(address,address,uint256,bytes,uint16)")[..4];
        assert_eq!(&calldata[..4], expected_selector);
    }

    #[test]
    fn balancer_calldata_has_correct_selector() {
        let calldata = encode_flashloan_call(
            FlashloanProvider::Balancer,
            addr(0xAA),
            addr(0xBB),
            addr(0xCC),
            eth(10),
            true,
            b"callback",
        );
        let expected_selector = &keccak256(b"flashLoan(address,address[],uint256[],bytes)")[..4];
        assert_eq!(&calldata[..4], expected_selector);
    }

    #[test]
    fn uniswap_calldata_has_correct_selector() {
        let calldata = encode_flashloan_call(
            FlashloanProvider::UniswapV3,
            addr(0xAA),
            addr(0xBB),
            addr(0xCC),
            eth(10),
            true,
            b"callback",
        );
        let expected_selector = &keccak256(b"flash(address,uint256,uint256,bytes)")[..4];
        assert_eq!(&calldata[..4], expected_selector);
    }

    /// Regression test for the token0/token1 bug: when the borrowed asset is the pool's
    /// token1, amount_wei must land in the amount1 slot (bytes 68..100, right after the
    /// 4-byte selector + 32-byte recipient + 32-byte amount0), and amount0 must be zero.
    /// Before the fix, this would have silently put amount_wei in amount0 regardless.
    #[test]
    fn uniswap_asset_is_token1_places_amount_in_amount1_slot() {
        let calldata = encode_flashloan_call(
            FlashloanProvider::UniswapV3,
            addr(0xAA),
            addr(0xBB),
            addr(0xCC),
            eth(10),
            false,
            b"callback",
        );
        let amount0_bytes = &calldata[4 + 32..4 + 64];
        let amount1_bytes = &calldata[4 + 64..4 + 96];
        let zero = [0u8; 32];
        assert_eq!(
            amount0_bytes,
            &zero[..],
            "amount0 must be zero when asset is token1"
        );
        assert_ne!(
            amount1_bytes,
            &zero[..],
            "amount1 must carry amount_wei when asset is token1"
        );
    }

    #[test]
    fn uniswap_asset_is_token0_places_amount_in_amount0_slot() {
        let calldata = encode_flashloan_call(
            FlashloanProvider::UniswapV3,
            addr(0xAA),
            addr(0xBB),
            addr(0xCC),
            eth(10),
            true,
            b"callback",
        );
        let amount0_bytes = &calldata[4 + 32..4 + 64];
        let amount1_bytes = &calldata[4 + 64..4 + 96];
        let zero = [0u8; 32];
        assert_ne!(
            amount0_bytes,
            &zero[..],
            "amount0 must carry amount_wei when asset is token0"
        );
        assert_eq!(
            amount1_bytes,
            &zero[..],
            "amount1 must be zero when asset is token0"
        );
    }

    #[test]
    fn calldata_length_is_multiple_of_32_plus_4() {
        for provider in [
            FlashloanProvider::AaveV3,
            FlashloanProvider::Balancer,
            FlashloanProvider::UniswapV3,
        ] {
            let cd = encode_flashloan_call(
                provider,
                addr(0x01),
                addr(0x02),
                addr(0x03),
                eth(1),
                true,
                b"test_data",
            );
            assert_eq!(
                (cd.len() - 4) % 32,
                0,
                "provider {provider}: calldata tail must be 32-byte aligned"
            );
        }
    }

    #[test]
    fn balancer_has_lowest_priority_number() {
        assert!(FlashloanProvider::Balancer.priority() < FlashloanProvider::AaveV3.priority());
        assert!(FlashloanProvider::AaveV3.priority() < FlashloanProvider::UniswapV3.priority());
    }

    #[test]
    fn premium_bps_match_constants() {
        assert_eq!(FlashloanProvider::AaveV3.premium_bps(), AAVE_V3_PREMIUM_BPS);
        assert_eq!(
            FlashloanProvider::Balancer.premium_bps(),
            BALANCER_PREMIUM_BPS
        );
        assert_eq!(
            FlashloanProvider::UniswapV3.premium_bps(),
            UNISWAP_V3_PREMIUM_BPS
        );
    }
}
