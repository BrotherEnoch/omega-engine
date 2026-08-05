{
  "files": [
    "contracts/src/OmegaOrchestrator.sol",
    "contracts/src/OpilToken.sol",
    "contracts/lib/openzeppelin-contracts/contracts/token/ERC20/ERC20.sol"
  ],
  "verify": "OmegaOrchestrator:contracts/certora/specs/OmegaSystem.spec",
  "packages": ["@openzeppelin/contracts=contracts/lib/openzeppelin-contracts/contracts"],
  "solc": "solc",
  "optimistic_loop": true,
  "loop_iter": "3",
  "rule_sanity": "basic",
  "msg": "OmegaSystem: Orchestrator C1-C5 + OpilToken C7-C8"
}
