# docs/Andre.md
# Done — path 2 (local Anvil). Contracts were deployed on a local chain; addresses are real for chain id 31337 only, not Arbitrum.
# Chain

# ItemValueRPChttp://127.0.0.1:8545Chain ID31337DeployerAnvil account #0 0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266
# Core (DeployCore)

# ContractAddressMockERC20 (profit token)0x5FbDB2315678afecb367f032d93F642f64180aa3SP1StarkVerifierAdapter0xe7f1725E7734CE288F8367e1Bb143E90bb3F0512PilTreasury0x9fE46736679d2D9a65F0992F2272dE9f3c7fa6e0OpilToken0xCf7Ed3AccA5a467e9e704C703E8D87F634fB0Fc9OmegaVault0x5FC8d32690cc91D4c39d9d3abcBD16989F875707OmegaOrchestrator0x0165878A594ca255338adfa4d48449f69242Eb8F
Strategies (DeployStrategies) — verified non-empty code on Anvil

IDAddresscodehash (from deploy log)CNRY0x610178dA211FEF7D417bC0e6FeD39F05609AD7880x73a6a231…1beb64SA0x8A791620dd6260079BF849Dc5567aDC3F2FdC3180xe6f49fb1…2b3cecMSA0xA51c1fc2f0D1a1b8494Ed1FE312d7C3a78Ed91C00xe37e5a1a…df1ba2LA0xB7f8BC63BbcaD18155201308C8f3540b07f84F5e0x326a1b2e…736aa1MEV0x0DCd1Bf9A1b36cE34237eEaFef220932846BCD820x9d8c4e4d…276f93
Files written

config/strategies_manual.toml — manual input for gen-manifest
config/deployment_manifest.toml — addresses + bytecode hashes
(under /home/workdir/artifacts/config/ in this environment)
config/anvil_deploy_summary.toml — full address list
Broadcast JSONs under /home/workdir/artifacts/broadcast/

Important limits

Not production / not Arbitrum. External deps (SP1_VERIFIER, BALANCER_VAULT, AAVE_POOL, etc.) are non-zero stand-ins so constructors run. Flashloans / SP1 proofs will not work against real protocols on this chain.
Anvil is process-local; restart Anvil → these addresses disappear unless you keep the same state.
Copy the TOML into your repo’s config/ if you want them on your machine.

To use with the engine against this Anvil instance, point RPC at http://127.0.0.1:8545 and place deployment_manifest.toml where main.rs expects it.