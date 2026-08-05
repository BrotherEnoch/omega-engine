$p = Join-Path (Get-Location) "README.md"
$c = @"
# OmegaEngine v12

**Live trading infrastructure for zero-capital flash-loan MEV and DeFi execution**

OmegaEngine is a modular control plane and on-chain execution stack for atomic arbitrage, liquidations, and related flash-loan strategies. It is built for **live capital** operation: deterministic blueprints, pre-trade risk gating, sub-millisecond hot-path simulation, multi-relay submission, and audited Solidity settlement.

> **Version:** 12.0.0  
> **Orientation:** Production trading infrastructure. Phase, rollout, capital limits, and relay exposure are **operator decisions**—not defaults assumed by this document.

---

## Purpose

Run MEV and DeFi strategies end-to-end against live chains and builders:

| Layer | Responsibility |
|-------|----------------|
| **Ingest** | Oracles, mempool, protocol state |
| **Decide** | Strategies → ``ExecutionBlueprint`` (optional advisory PRL) |
| **Admit** | DAG scheduling / capacity |
| **Simulate** | Hot-path (``revm``) or ZK path |
| **Gate** | Risk checks, kill switches, integrity |
| **Submit** | Signed bundles → multi-relay cascade |
| **Settle** | Orchestrator → strategy → vault on-chain |

Advisory components (e.g. pattern recognition) must not block liquidation, submission, or gas bidding when they fail or halt.

---

## Capabilities

- **Flash-loan native** — Provider selection and fallback across supported protocols (e.g. Balancer V2, Aave v3, Uniswap V3), as implemented in-repo.
- **Strategies** — Simple 2-DEX arb, multi-hop routes, liquidations, canary path validation.
- **Hot path** — Microtx lane targeting sub-millisecond simulation for simple arb and high-health-factor liquidations.
- **Risk** — Pre-trade check suite, kill switches, oracle freshness/divergence, liquidity margins, idempotency, integrity registry.
- **Relay** — Multi-relay submission with dedup, reorg awareness, and back-pressure.
- **Formal / ops** — Certora targets for core contracts; control plane (REST / gRPC / WebSocket); calibration and backtest tooling where present.

---

## Architecture

Signals / Oracles / Mempool → omega-prl (advisory) → omega-strategies → ExecutionBlueprint → omega-dag → hot-path (revm) or omega-zk → ExecutionPipeline (risk → sign → payload) → omega-relay → builders/sequencers → OmegaOrchestrator → Strategy → Vault

**Invariant:** PRL outputs are advisory. Execution must remain operable if PRL is down.

---

## Repository layout

**crates/ (21 workspace members):** omega-core, omega-health, omega-rpc, omega-oracle, omega-security, omega-compliance, omega-risk, omega-dag, omega-zk, omega-flashloan, omega-relay, omega-gas-war, omega-loss-attribution, omega-address-rotation, omega-strategies, omega-cross-chain, omega-hot-path, omega-observability, omega-chaos, omega-prl, omega-execution

**ops/ (4 workspace members):** control-plane, shadow, backtest, calibrate

**Total workspace members: 25** (see root Cargo.toml ``[workspace].members``).

Also present: contracts/ (Foundry), config/, certora/specs/, docs/, Makefile, .env.example

---

## Prerequisites

- Rust (stable, edition 2021) + Cargo
- Foundry (forge, anvil)
- protoc (gRPC)
- Docker (optional)
- Production-grade WebSocket/HTTP RPC endpoints

---

## Quick start (build and verify)

    git clone https://github.com/BrotherEnoch/omega-engine.git
    cd omega-engine
    copy .env.example .env
    make build
    make test
    make contracts

| Target | Description |
|--------|-------------|
| check | cargo check --workspace |
| build | Release workspace build |
| test | Rust tests |
| clippy | Lint |
| contracts | Foundry build + test |
| certora | Certora (Orchestrator + Vault) |
| control-plane | REST + gRPC control plane |
| backtest | Historical backtest (if configured) |
| docker-build | Container image |

**Live trading** is not a single Make target. It requires operator-set phase, keys, relays, risk thresholds, and a binary that drives ExecutionPipeline with a production signer and relay clients.

---

## Configuration

- Environment — .env.example and split env files (.env.rpc, .env.relays, .env.signing, …)
- Chain — config/arbitrum.toml, config/base.toml, config/default.toml
- Phase / rollout — operator configuration only; this document does not prescribe defaults

---

## On-chain contracts

| Contract | Role |
|----------|------|
| OmegaOrchestrator | Flash-loan execution entry |
| OmegaVault | Profit / capital custody |
| Strategies | SimpleArb, MultiStepArb, LiquidationArb, CanaryArb |
| OpilToken / Treasury | Protocol token and fees |

    cd contracts && forge build && forge test

---

## Design principles

1. Determinism
2. Fail closed
3. Idempotency
4. Isolation (crate boundaries)
5. Observability first
6. Non-blocking advisory (PRL)

---

## Safety (live operation)

- Never commit keys, HSM endpoints, or relay secrets
- Prefer dedicated nodes for latency-sensitive paths
- Operate kill switches, blacklists, and integrity registry deliberately
- Independent review before mainnet capital at scale

---

## Development

    make fmt
    make clippy
    make check

---

## Documentation

- ExecutionPipelineSpecification.md
- ProductionIntegrationPlan.md
- docs/runbooks/
- Crate lib.rs module docs

---

## License and disclaimer

See the repository license file if present. You own phase promotion, capital limits, key custody, and go-live criteria.
"@
[System.IO.File]::WriteAllText($p, $c)
Write-Host "Wrote $p bytes=$((Get-Item $p).Length)"
