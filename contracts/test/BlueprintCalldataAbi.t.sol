// contracts/test/BlueprintCalldataAbi.t.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import {OmegaOrchestrator} from "../src/OmegaOrchestrator.sol";
import {StrategyIds} from "../src/StrategyIds.sol";

/// @notice solc/EVM oracle for blueprintCalldata layout.
/// @dev    Mirrors OmegaOrchestrator.execute()'s abi.decode tuple exactly.
///         Golden bytes are the authoritative cross-check for
///         crates/omega-execution KeyManagerTransactionSigner::build_blueprint_calldata.
contract BlueprintCalldataAbiTest is Test {
    // Fixed fixture — must match the Rust golden test vectors bit-for-bit.
    uint64  internal constant EXPIRY_BLOCK = 1_100;
    uint64  internal constant NONCE = 0;
    bytes32 internal constant STRATEGY_ID = StrategyIds.SIMPLE_ARB;
    OmegaOrchestrator.FlashloanProviderType internal constant PROVIDER_TYPE =
        OmegaOrchestrator.FlashloanProviderType.Balancer; // uint8(0)
    address internal constant FLASHLOAN_TOKEN = address(0x9999999999999999999999999999999999999999);
    address internal constant PROVIDER_CONTRACT = address(0);
    bytes   internal constant STRATEGY_CALLDATA = hex"deadbeef";
    uint256 internal constant FLASHLOAN_AMOUNT = 1_000_000;
    uint256 internal constant MIN_NET_PROFIT = 100_000;
    // sample_bp uses max_base_fee_gwei from derive_max_base_fee_gwei(10, 3.0);
    // for this oracle we pin an explicit wei value so the vector is deterministic.
    uint256 internal constant MAX_BASE_FEE_WEI = 30_000_000_000; // 30 gwei in wei

    function _encodeBlueprint() internal pure returns (bytes memory) {
        // EXACT tuple order as OmegaOrchestrator.execute() step 2.
        return abi.encode(
            EXPIRY_BLOCK,
            NONCE,
            STRATEGY_ID,
            PROVIDER_TYPE,
            FLASHLOAN_TOKEN,
            PROVIDER_CONTRACT,
            STRATEGY_CALLDATA,
            FLASHLOAN_AMOUNT,
            MIN_NET_PROFIT,
            MAX_BASE_FEE_WEI
        );
    }

    function test_solc_encode_round_trips_execute_decode_tuple() public pure {
        bytes memory encoded = _encodeBlueprint();

        (
            uint64 expiry_block,
            uint64 nonce,
            bytes32 strategyId,
            OmegaOrchestrator.FlashloanProviderType providerType,
            address flashloanToken,
            address providerContract,
            bytes memory strategyCalldata,
            uint256 flashloanAmount,
            uint256 minNetProfit,
            uint256 maxBaseFee
        ) = abi.decode(
            encoded,
            (
                uint64,
                uint64,
                bytes32,
                OmegaOrchestrator.FlashloanProviderType,
                address,
                address,
                bytes,
                uint256,
                uint256,
                uint256
            )
        );

        assertEq(expiry_block, EXPIRY_BLOCK);
        assertEq(nonce, NONCE);
        assertEq(strategyId, STRATEGY_ID);
        assertTrue(providerType == PROVIDER_TYPE);
        assertEq(flashloanToken, FLASHLOAN_TOKEN);
        assertEq(providerContract, PROVIDER_CONTRACT);
        assertEq(strategyCalldata, STRATEGY_CALLDATA);
        assertEq(flashloanAmount, FLASHLOAN_AMOUNT);
        assertEq(minNetProfit, MIN_NET_PROFIT);
        assertEq(maxBaseFee, MAX_BASE_FEE_WEI);
    }

    function test_solc_encode_is_multiple_of_32_and_nonempty() public pure {
        bytes memory encoded = _encodeBlueprint();
        assertTrue(encoded.length > 0);
        assertEq(encoded.length % 32, 0);
    }

    /// @dev Print golden hex once; lock the same bytes in Rust.
    ///      forge test --match-test test_print_golden_blueprint_calldata -vv
    function test_print_golden_blueprint_calldata() public pure {
        bytes memory encoded = _encodeBlueprint();
        console.logBytes(encoded);
        assertTrue(encoded.length >= 320); // head slots + dynamic tail
    }

    function test_strategy_id_is_canonical_sa() public pure {
        assertEq(STRATEGY_ID, StrategyIds.SIMPLE_ARB);
        assertEq(
            STRATEGY_ID,
            bytes32(0xc4bb1c851b1c74593f61f8d1f99ec07e2960d847a94d4a736e321ba387d4d2d7)
        );
    }

    function test_provider_type_ordinal_matches_enum() public pure {
        assertEq(uint8(OmegaOrchestrator.FlashloanProviderType.Balancer), 0);
        assertEq(uint8(OmegaOrchestrator.FlashloanProviderType.AaveV3), 1);
        assertEq(uint8(OmegaOrchestrator.FlashloanProviderType.UniswapV3), 2);
        assertEq(uint8(PROVIDER_TYPE), 0);
    }
}