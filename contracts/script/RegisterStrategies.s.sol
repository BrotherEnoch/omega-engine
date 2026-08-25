// contracts/script/RegisterStrategies.s.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Script.sol";
import {OmegaOrchestrator} from "../src/OmegaOrchestrator.sol";
import {StrategyIds}       from "../src/StrategyIds.sol";

/// @title RegisterStrategies
/// @notice Reads config/deployment/<network>.toml and calls
///         OmegaOrchestrator.registerStrategy() for each of the four strategies.
///
/// DEPENDENCIES / ASSUMPTIONS FLAGGED:
///   - Requires forge-std's TOML cheatcodes (vm.parseTomlAddress / vm.parseTomlBytes32).
///     These are present in reasonably recent forge-std; if your pinned version predates
///     them, `forge update` forge-std first or tell me and I'll rewrite this against
///     env-var inputs instead.
///   - `registerStrategy` is `onlyRole(DEFAULT_ADMIN_ROLE)` on OmegaOrchestrator (see that
///     contract's own gating) -- whatever key/account this script broadcasts from
///     (--private-key / --account / --ledger, your choice at invocation time) MUST hold
///     that role, or every call here reverts. I'm not assuming which signing method you use.
///   - `min_phase` from the manifest is deliberately NOT read or used anywhere in this
///     script. As flagged when the manifest was written, nothing on-chain in
///     OmegaOrchestrator currently enforces phase gating, so there is nothing for this
///     script to correctly do with that field yet -- reading it here without a defined
///     use would be exactly the kind of unstated assumption I was told not to make.
///   - Path defaults to config/deployment/arbitrum.toml; override with
///     `MANIFEST_PATH=config/deployment/<other>.toml forge script ...` for other networks.
///
/// USAGE:
///   MANIFEST_PATH=config/deployment/arbitrum.toml \
///     forge script script/RegisterStrategies.s.sol --rpc-url <arbitrum_rpc> --broadcast --account <admin_key_alias>
contract RegisterStrategies is Script {
    struct StrategyEntry {
        string  key;                     // manifest section name, e.g. "SA" -- logging only
        bytes32 strategyId;              // from StrategyIds, NOT from the manifest -- the
                                          // manifest's onchain_id field is treated as a
                                          // human-readable cross-check, not the source of
                                          // truth (StrategyIds.sol is), so a typo'd manifest
                                          // value can't silently register under the wrong id
        address implementation;
        bytes32 expectedBytecodeHash;
    }

    function run() external {
        string memory manifestPath = vm.envOr(
            "MANIFEST_PATH",
            string("config/deployment/arbitrum.toml")
        );
        string memory toml = vm.readFile(manifestPath);

        address orchestratorAddr = vm.parseTomlAddress(toml, ".orchestrator.address");
        OmegaOrchestrator orch = OmegaOrchestrator(orchestratorAddr);

        StrategyEntry[5] memory entries = [
            StrategyEntry(
                "SA",
                StrategyIds.SIMPLE_ARB,
                vm.parseTomlAddress(toml, ".strategies.SA.implementation"),
                vm.parseTomlBytes32(toml, ".strategies.SA.bytecode_hash")
            ),
            StrategyEntry(
                "LA",
                StrategyIds.LIQUIDATION_ARB,
                vm.parseTomlAddress(toml, ".strategies.LA.implementation"),
                vm.parseTomlBytes32(toml, ".strategies.LA.bytecode_hash")
            ),
            StrategyEntry(
                "MSA",
                StrategyIds.MULTI_STEP_ARB,
                vm.parseTomlAddress(toml, ".strategies.MSA.implementation"),
                vm.parseTomlBytes32(toml, ".strategies.MSA.bytecode_hash")
            ),
            StrategyEntry(
                "MEV",
                StrategyIds.MEV_OFA,
                vm.parseTomlAddress(toml, ".strategies.MEV.implementation"),
                vm.parseTomlBytes32(toml, ".strategies.MEV.bytecode_hash")
            ),
            StrategyEntry(
                "CNRY",
                StrategyIds.CANARY_ARB,
                vm.parseTomlAddress(toml, ".strategies.CNRY.implementation"),
                vm.parseTomlBytes32(toml, ".strategies.CNRY.bytecode_hash")
            )
        ];

        // Cross-check manifest's onchain_id against StrategyIds BEFORE broadcasting anything --
        // catches a manifest that's drifted from StrategyIds.sol (e.g. someone hand-edited one
        // and not the other) with a clear revert reason, rather than silently registering
        // whichever one happens to be "right" and leaving the mismatch undetected.
        _checkManifestIdMatches(toml, ".strategies.SA.onchain_id",  entries[0]);
        _checkManifestIdMatches(toml, ".strategies.LA.onchain_id",  entries[1]);
        _checkManifestIdMatches(toml, ".strategies.MSA.onchain_id", entries[2]);
        _checkManifestIdMatches(toml, ".strategies.MEV.onchain_id", entries[3]);
        _checkManifestIdMatches(toml, ".strategies.CNRY.onchain_id", entries[4]);

        vm.startBroadcast();

        for (uint256 i = 0; i < entries.length; i++) {
            StrategyEntry memory e = entries[i];

            // Pre-flight bytecode check, mirroring OmegaOrchestrator.execute()'s own step 8
            // (BytecodeMismatch) -- confirms the manifest's recorded hash still matches the
            // implementation's LIVE codehash right now, before spending gas on a tx that
            // registers a strategy whose bytecode_hash is about to be wrong from block one.
            bytes32 actualCodehash = e.implementation.codehash;
            require(
                actualCodehash == e.expectedBytecodeHash,
                string.concat(
                    "bytecode_hash mismatch for ", e.key,
                    " -- manifest is stale or implementation address is wrong"
                )
            );

            orch.registerStrategy(e.strategyId, e.implementation);
            console2.log("Registered strategy:", e.key);
            console2.log("  strategyId:     ", vm.toString(e.strategyId));
            console2.log("  implementation: ", e.implementation);
        }

        vm.stopBroadcast();
    }

    function _checkManifestIdMatches(
        string memory toml,
        string memory tomlKey,
        StrategyEntry memory entry
    ) internal view {
        bytes32 manifestId = vm.parseTomlBytes32(toml, tomlKey);
        require(
            manifestId == entry.strategyId,
            string.concat(
                "manifest onchain_id does not match StrategyIds.sol for ", entry.key
            )
        );
    }
}