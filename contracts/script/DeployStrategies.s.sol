// contracts/script/DeployStrategies.s.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Script.sol";
import {SimpleArb}      from "../src/strategies/SimpleArb.sol";
import {CanaryArb}      from "../src/strategies/CanaryArb.sol";
import {LiquidationArb} from "../src/strategies/LiquidationArb.sol";
import {MultiStepArb}   from "../src/strategies/MultiStepArb.sol";
import {MevOfa}         from "../src/strategies/MevOfa.sol";

/// @title DeployStrategies
/// @notice Deploys SimpleArb, CanaryArb, LiquidationArb, MultiStepArb, and MevOfa, and logs
///         the values needed to fill in config/deployment/<network>.toml.
///
/// WHAT THIS SCRIPT IS BASED ON:
///   All five strategy contracts have now been provided directly and every constructor
///   below is CONFIRMED against real source, not inferred from test mocks:
///     SimpleArb(orchestrator)
///     CanaryArb(orchestrator)
///     LiquidationArb(orchestrator, aavePool, compoundComet, morphoBlue, eulerV2, swapRouter)
///     MultiStepArb(orchestrator)
///     MevOfa(orchestrator, minPriceImpactBps)
///   eulerV2 = address(0) for a Phase 3.0 deploy, per LiquidationArb.sol's own constructor
///   comment. minPriceImpactBps maps to MevOfa's immutable MIN_PRICE_IMPACT_BPS, confirmed
///   by that contract's own constructor.
///
///   Unlike LiquidationArb, SimpleArb/CanaryArb/MultiStepArb/MevOfa take NO protocol/pool
///   addresses at construction time -- those are supplied per-call in strategyCalldata when
///   a blueprint is built, not baked in here. That's why only LiquidationArb needs the
///   AAVE_POOL / COMPOUND_COMPTROLLER / MORPHO_BLUE / EULER_V2 / ROUTER env vars below.
///
///   KNOWN ISSUE IN LiquidationArb.sol (flagged, not fixed here): its activateEuler() is
///   gated `onlyOrchestrator` (msg.sender == the OmegaOrchestrator contract address), but
///   OmegaOrchestrator.sol has no function anywhere that calls out to a strategy's
///   activateEuler() -- so as currently written, nothing can ever call it. Not a blocker for
///   this Phase 3.0 deploy (eulerV2 is address(0) here regardless), but real work needed
///   before Phase 3.1 activation.
///
///   SEPARATE NOTE, NOT A DEPLOY-TIME BLOCKER: MultiStepArb.sol's own docstring flags that
///   its UniV3ExactInputSingleParams struct assumes the classic ISwapRouter (with a
///   `deadline` field), not SwapRouter02. This doesn't affect deployment of the contract
///   itself, but will make every UniV3 hop revert at blueprint-execution time if the router
///   you actually point hops at is SwapRouter02 -- confirm which router before building any
///   MultiStepArb blueprint that uses UniV3 hops.
///
/// WHAT I WILL NOT DO: fill in real Aave/Compound/Morpho/router/Euler addresses for you.
///   Those are protocol deployment addresses that vary by chain and change over time --
///   guessing one from training data and having it silently accepted is a real fund-safety
///   risk on a contract that flashloans and moves real capital. Every one of them is a
///   required env var below; the script reverts with a clear message if any is missing
///   rather than silently deploying against address(0) or a stale guess.
///
/// USAGE:
///   ORCHESTRATOR=0x... \
///   AAVE_POOL=0x... COMPOUND_COMPTROLLER=0x... MORPHO_BLUE=0x... EULER_V2=0x0 ROUTER=0x... \
///   MEV_OFA_MIN_PRICE_IMPACT=<value, confirm units with the real contract> \
///     forge script script/DeployStrategies.s.sol --rpc-url <arbitrum_rpc> --broadcast --account <deployer_key_alias>
///
///   EULER_V2 is intentionally allowed to be address(0) -- matching the test's own use of
///   address(0) to represent "not yet activated" -- but every other address below is
///   required and the script reverts if unset.
contract DeployStrategies is Script {
    function run() external {
        address orchestrator = vm.envAddress("ORCHESTRATOR");
        require(orchestrator != address(0), "ORCHESTRATOR env var not set");

        address aavePool   = vm.envAddress("AAVE_POOL");
        address compound   = vm.envAddress("COMPOUND_COMPTROLLER");
        address morphoBlue = vm.envAddress("MORPHO_BLUE");
        address eulerV2    = vm.envOr("EULER_V2", address(0)); // allowed to be unset/zero -- see note above
        address router     = vm.envAddress("ROUTER");

        require(aavePool   != address(0), "AAVE_POOL env var not set");
        require(compound   != address(0), "COMPOUND_COMPTROLLER env var not set");
        require(morphoBlue != address(0), "MORPHO_BLUE env var not set");
        require(router     != address(0), "ROUTER env var not set");

        uint256 mevMinPriceImpact = vm.envUint("MEV_OFA_MIN_PRICE_IMPACT");

        vm.startBroadcast();

        SimpleArb simpleArb = new SimpleArb(orchestrator);
        console2.log("SimpleArb deployed:", address(simpleArb));

        CanaryArb canaryArb = new CanaryArb(orchestrator);
        console2.log("CanaryArb deployed:", address(canaryArb));

        LiquidationArb liquidationArb = new LiquidationArb(
            orchestrator,
            aavePool,
            compound,
            morphoBlue,
            eulerV2,
            router
        );
        console2.log("LiquidationArb deployed:", address(liquidationArb));

        MultiStepArb multiStepArb = new MultiStepArb(orchestrator);
        console2.log("MultiStepArb deployed:", address(multiStepArb));

        MevOfa mevOfa = new MevOfa(orchestrator, mevMinPriceImpact);
        console2.log("MevOfa deployed:", address(mevOfa));

        vm.stopBroadcast();

        // Log codehash for each -- copy these directly into arbitrum.toml's bytecode_hash
        // fields rather than re-deriving with a separate `cast code | cast keccak` call,
        // though re-deriving independently as a cross-check costs nothing and is good
        // practice before broadcasting RegisterStrategies.s.sol against these.
        console2.log("--- bytecode_hash values for arbitrum.toml ---");
        console2.log("SA  bytecode_hash:", vm.toString(address(simpleArb).codehash));
        console2.log("LA  bytecode_hash:", vm.toString(address(liquidationArb).codehash));
        console2.log("MSA bytecode_hash:", vm.toString(address(multiStepArb).codehash));
        console2.log("MEV bytecode_hash:", vm.toString(address(mevOfa).codehash));
        console2.log("CNRY bytecode_hash:", vm.toString(address(canaryArb).codehash));
    }
}