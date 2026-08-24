// contracts/script/DeployCore.s.sol
// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Script.sol";
import {OmegaOrchestrator}       from "../src/OmegaOrchestrator.sol";
import {OmegaVault}              from "../src/OmegaVault.sol";
import {OpilToken}               from "../src/OpilToken.sol";
import {PilTreasury}             from "../src/PilTreasury.sol";
import {SP1StarkVerifierAdapter} from "../src/verifiers/SP1StarkVerifierAdapter.sol";

/// @title DeployCore
/// @notice Deploys PilTreasury, OpilToken, OmegaVault, and OmegaOrchestrator, in the only
///         order that actually works given their constructor dependencies, and wires the
///         cross-contract role grants each one's own docstring says are required.
///
/// DEPENDENCY ORDERING -- not arbitrary, forced by what each constructor needs:
///
///   1. PilTreasury needs nothing but profit_token/admin (and an OPTIONAL vault address for
///      a sanity check it can only run if Vault already exists -- it doesn't yet, so this
///      deploys with `_vaultForCheck = address(0)`, exactly as PilTreasury.sol's own
///      constructor doc says to do when Vault isn't deployed yet at this point).
///
///   2. OpilToken needs PilTreasury's address (constructor grants it MINTER_ROLE/BURNER_ROLE
///      directly) -- so PilTreasury must exist first. This is PilTreasury.sol's own
///      documented "Deploy order" note, step 2.
///
///   3. PilTreasury.setOpilToken() wires the reverse edge -- PilTreasury.sol's own note,
///      step 3. One-time; reverts if called twice.
///
///   4. SP1StarkVerifierAdapter is deployed BEFORE OmegaVault, since Vault's constructor
///      needs a `_starkVerifier` address and the adapter has no dependency on anything else
///      in this file -- it only wraps an already-deployed external SP1 verifier/gateway plus
///      a program vkey, neither of which come from anything deployed here. This replaces
///      what used to be a raw `STARK_VERIFIER` env var pointing directly at some
///      IStarkVerifier -- now Vault always points at the adapter, and the adapter points at
///      the real SP1 verifier.
///
///   5. OmegaVault needs pil_treasury (now known), the adapter's address (now known), and an
///      `_orchestrator` address to grant ORCHESTRATOR_ROLE to AT CONSTRUCTION TIME -- but
///      OmegaOrchestrator doesn't exist yet either, and OmegaOrchestrator's own constructor
///      needs Vault's address. This is a real circular dependency, not an oversight on my
///      part to work around cleverly -- OmegaSystem.t.sol's own setUp() resolves it by
///      passing address(0) as the orchestrator here, then granting the role as a separate
///      step (7) below. I'm following that exact pattern because it's the one already
///      exercised by your test suite, not inventing a different resolution.
///
///   6. OmegaOrchestrator needs Vault's address (now known) -- deployed here.
///
///   7. grantRole(ORCHESTRATOR_ROLE, orchestrator) on the Vault -- closes the loop from
///      step 5, exactly matching OmegaSystem.t.sol's setUp():
///        bytes32 orchestratorRole = vault.ORCHESTRATOR_ROLE();
///        vault.grantRole(orchestratorRole, address(orch));
///
///   8. grantRole(DEPTH_UPDATER_ROLE, depthUpdater) on the Vault -- OmegaVault.sol's own
///      constructor comment is explicit that this role is INTENTIONALLY not granted at
///      construction and MUST be granted separately, or "no profit can ever clear the C6
///      gate." Not optional; the script reverts if DEPTH_UPDATER is unset, rather than
///      silently deploying a Vault that can never release anything.
///
/// WHAT I WILL NOT FABRICATE, same as DeployStrategies.s.sol:
///   - `PROFIT_TOKEN`: the real ERC20 this whole system accounts profit in (WETH, USDC,
///     whatever you've chosen) -- required env var, no default.
///   - `SP1_VERIFIER`: address of a real, already-deployed ISP1Verifier-conforming contract
///     (Succinct's canonical SP1_VERIFIER_GATEWAY for your chain is their own recommendation
///     -- see https://docs.succinct.xyz/docs/sp1/verification/contract-addresses). This is
///     passed to SP1StarkVerifierAdapter's constructor, NOT directly to Vault anymore.
///     Required env var, no default -- same reasoning as every other external protocol
///     address in this script: I won't assert one from memory.
///   - `PROGRAM_VKEY`: your compiled SP1 program's vkey. Does not exist until that program
///     exists and is compiled (per SP1StarkVerifierAdapter.sol's own header) -- required env
///     var, no default, and this script will deploy a permanently-nonfunctional adapter if
///     you pass a placeholder here rather than waiting until the real value exists.
///   - `BALANCER_VAULT`: passed to OmegaOrchestrator as `_flashloanProvider`. Balancer V2's
///     Vault is deployed at the same address on many networks via CREATE2, but I am not
///     asserting that address here -- confirm it against Balancer's own current deployment
///     docs for Arbitrum specifically before using it, rather than trusting it from any
///     external memory (mine or otherwise).
///   - `AAVE_POOL`, `PER_TRANSFER_CAP`, `DAILY_CAP`, `EXECUTION_KEY`, `DAO_FEE_ADDRESS`,
///     `DEPTH_UPDATER`: all required env vars, all deployment-specific decisions that are
///     not mine to make up.
///
/// USAGE:
///   PROFIT_TOKEN=0x... DAO_FEE_ADDRESS=0x... ADMIN=0x... \
///   SP1_VERIFIER=0x... PROGRAM_VKEY=0x... \
///   PER_TRANSFER_CAP=<wei> DAILY_CAP=<wei> \
///   BALANCER_VAULT=0x... AAVE_POOL=0x... EXECUTION_KEY=0x... DEPTH_UPDATER=0x... \
///     forge script script/DeployCore.s.sol --rpc-url <arbitrum_rpc> --broadcast --account <admin_key_alias>
contract DeployCore is Script {
    function run() external {
        address profitToken   = vm.envAddress("PROFIT_TOKEN");
        address daoFeeAddr    = vm.envAddress("DAO_FEE_ADDRESS");
        address sp1Verifier   = vm.envAddress("SP1_VERIFIER");
        bytes32 programVKey   = vm.envBytes32("PROGRAM_VKEY");
        address admin         = vm.envAddress("ADMIN");
        uint256 perTransferCap = vm.envUint("PER_TRANSFER_CAP");
        uint256 dailyCap       = vm.envUint("DAILY_CAP");
        address balancerVault = vm.envAddress("BALANCER_VAULT");
        address aavePool      = vm.envAddress("AAVE_POOL");
        address executionKey  = vm.envAddress("EXECUTION_KEY");
        address depthUpdater  = vm.envAddress("DEPTH_UPDATER");

        require(profitToken   != address(0), "PROFIT_TOKEN env var not set");
        require(daoFeeAddr    != address(0), "DAO_FEE_ADDRESS env var not set");
        require(sp1Verifier   != address(0), "SP1_VERIFIER env var not set");
        require(programVKey   != bytes32(0), "PROGRAM_VKEY env var not set -- see SP1StarkVerifierAdapter.sol's header on why this can't be a placeholder");
        require(admin         != address(0), "ADMIN env var not set");
        require(balancerVault != address(0), "BALANCER_VAULT env var not set");
        require(aavePool      != address(0), "AAVE_POOL env var not set");
        require(executionKey  != address(0), "EXECUTION_KEY env var not set");
        require(depthUpdater  != address(0), "DEPTH_UPDATER env var not set -- see OmegaVault.sol's own constructor note on why this is required, not optional");

        vm.startBroadcast();

        // ── Step 1: SP1StarkVerifierAdapter -- no dependency on anything else deployed here,
        //    only on the externally-supplied SP1_VERIFIER/PROGRAM_VKEY. Deployed first purely
        //    so its address exists in time for OmegaVault's constructor in step 5. ──────────
        SP1StarkVerifierAdapter starkVerifierAdapter = new SP1StarkVerifierAdapter(sp1Verifier, programVKey);
        console2.log("SP1StarkVerifierAdapter deployed:", address(starkVerifierAdapter));

        // ── Step 2: PilTreasury, with no Vault yet to sanity-check against ─────────────────
        PilTreasury pilTreasury = new PilTreasury(profitToken, admin, address(0));
        console2.log("PilTreasury deployed:", address(pilTreasury));

        // ── Step 3: OpilToken, now that PilTreasury's address exists ───────────────────────
        OpilToken opilToken = new OpilToken(address(pilTreasury), admin);
        console2.log("OpilToken deployed:", address(opilToken));

        // ── Step 4: wire the reverse edge, one-time ─────────────────────────────────────────
        pilTreasury.setOpilToken(address(opilToken));
        console2.log("PilTreasury.setOpilToken() called");

        // ── Step 5: OmegaVault, pointed at the adapter (not a raw external verifier), with
        //    orchestrator = address(0) placeholder (see header note on why -- this exactly
        //    matches OmegaSystem.t.sol's own setUp() resolution of the Vault/Orchestrator
        //    circular dependency) ────────────────────────────────────────────────────────────
        OmegaVault vault = new OmegaVault(
            address(pilTreasury),
            daoFeeAddr,
            address(starkVerifierAdapter),
            profitToken,
            admin,
            address(0),          // orchestrator placeholder -- granted for real in step 7
            perTransferCap,
            dailyCap
        );
        console2.log("OmegaVault deployed:", address(vault));

        // ── Step 6: OmegaOrchestrator, now that Vault's real address exists ────────────────
        OmegaOrchestrator orchestrator = new OmegaOrchestrator(
            uint64(block.chainid),
            address(vault),
            balancerVault,
            aavePool,
            executionKey,
            admin
        );
        console2.log("OmegaOrchestrator deployed:", address(orchestrator));

        // ── Step 7: close the loop from step 5 ──────────────────────────────────────────────
        bytes32 orchestratorRole = vault.ORCHESTRATOR_ROLE();
        vault.grantRole(orchestratorRole, address(orchestrator));
        console2.log("Granted ORCHESTRATOR_ROLE on Vault to Orchestrator");

        // ── Step 8: DEPTH_UPDATER_ROLE -- required per OmegaVault.sol's own constructor note,
        //    not optional (see require() above; this line cannot be reached with a zero
        //    depthUpdater) ─────────────────────────────────────────────────────────────────
        bytes32 depthUpdaterRole = vault.DEPTH_UPDATER_ROLE();
        vault.grantRole(depthUpdaterRole, depthUpdater);
        console2.log("Granted DEPTH_UPDATER_ROLE on Vault to:", depthUpdater);

        vm.stopBroadcast();

        console2.log("--- Summary: copy into arbitrum.toml [orchestrator] and keep the rest for your records ---");
        console2.log("SP1StarkVerifierAdapter:", address(starkVerifierAdapter));
        console2.log("PilTreasury:      ", address(pilTreasury));
        console2.log("OpilToken:        ", address(opilToken));
        console2.log("OmegaVault:       ", address(vault));
        console2.log("OmegaOrchestrator:", address(orchestrator));
    }
}