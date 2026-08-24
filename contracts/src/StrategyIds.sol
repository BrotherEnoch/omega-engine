// contracts/src/StrategyIds.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// @title StrategyIds
/// @notice Canonical strategyId constants for OmegaOrchestrator.registerStrategy().
library StrategyIds {
    /// @dev keccak256("OMEGA_STRATEGY_SA")
    bytes32 internal constant SIMPLE_ARB =
        0xc4bb1c851b1c74593f61f8d1f99ec07e2960d847a94d4a736e321ba387d4d2d7;

    /// @dev keccak256("OMEGA_STRATEGY_LA")
    bytes32 internal constant LIQUIDATION_ARB =
        0x77b0296a1c4dae896ee0ffe05246d8b3e8ecd44a1d4a0c6591b183fb2390a698;

    /// @dev keccak256("OMEGA_STRATEGY_MSA")
    bytes32 internal constant MULTI_STEP_ARB =
        0xbfd7e8e9c54a6762cb6ff399dc8bdefe2226a32400ed6001e1bee533bbaa25d2;

    /// @dev keccak256("OMEGA_STRATEGY_MEV")
    bytes32 internal constant MEV_OFA =
        0x892be743cfc8880f51726a84ab1d0d0fc05336d49927c5a9eaaf926a84db319a;

    /// @dev keccak256("OMEGA_STRATEGY_CNRY")
    bytes32 internal constant CANARY_ARB =
        0x93879ddf9ec0b01c066594680539ea61eaab23f806b410fda1c18659efcc7725;
}