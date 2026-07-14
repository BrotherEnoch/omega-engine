# omega-engine\crates\omega-simulation\README.md
omega-simulation
Phase 0.5 harness for omega-engine: runs real opportunity-detection and profitability logic against a forked copy of live chain state, using real (unmodified) flash loan pool interfaces and real contract bytecode — but on a disposable local node, with zero relay submissions and zero real signing keys.
This is not a mock/stub simulator. It executes the actual call path (flash loan borrow → arb/liquidation logic → repay) against forked mainnet/L2 state, so the profitability numbers it produces reflect real pool depth, real fees, and real slippage at the block you fork from.
What it validates
Profitability math under real (forked) pool reserves/prices
The flash loan borrow/fee/repay call path against real pool interfaces
Contract behavior under real interface constraints (reentrancy guards, callback decoding, etc.)
What it does NOT validate
Relay latency or bundle inclusion probability
Competition from other searchers front-running your bundle
HSM/execution-key signing flow
Any auth/config wiring for Flashbots, bloXroute, Titan, or Eden
Those are the job of the testnet dry-run layer (omega-testnet) and, eventually, staged production rollout — not this crate.
Safety design
This crate is structurally incapable of reaching a live relay or real signing backend:
Type-level: SimulationSubmitter (the only BundleSubmitter impl here) can only be constructed via SimulationSubmitter::bound_to(&ForkHandle, ...). There is no constructor that accepts a relay URL, auth token, or HSM/KMS key ID.
Runtime guard: SimulationSubmitter::reject_if_live_looking() screens any destination string containing relay-shaped markers (flashbots, bloxroute, titan, eden, relay., mev-share) and errors out with SimError::LiveTransportForbidden rather than silently proceeding.
Signing: the only key this crate ever uses is derived from Anvil's well-known public dev mnemonic (test test test ... junk), which is the same for every Anvil instance in existence. It never holds real value and can't sign anything against a real chain, since it only exists inside that process's ephemeral fork.
Dependency guard: ops/check_no_live_deps.sh fails CI if the crate's dependency tree ever picks up a crate whose name looks like a relay client or signing/HSM backend. Run it as part of any PR that touches this crate's Cargo.toml.
State never persists: ForkHandle owns the child Anvil process; dropping it kills the node and discards all state. A run can't leave residue a later run might accidentally rely on.
Requirements
Foundry installed, so anvil is on PATH.
A read-only RPC endpoint for the chain you're forking (e.g. an Arbitrum full node/provider URL). This is only ever used for reads during fork sync — this crate never sends state-changing calls to it.
Running the example
export ARBITRUM_RPC_URL="https://your-read-only-arbitrum-rpc"
cargo run --example run_sim -- \
    --cycles 50 \
    --fork-block 245000000 \
    --report out/sim_report.json
This uses a fixture OpportunityDetector that emits placeholder opportunities — enough to exercise fork spawn, submission, and reporting end-to-end. Swap in the engine's real detector (from omega-core) before treating results as a meaningful phase-gate signal; see "Integration" below.
Integration
To wire this into the real workspace:
Delete src/traits.rs here and depend on omega_core::{Bundle, BundleSubmitter, Opportunity, OpportunityDetector, Receipt} instead — sim and live execution must share one definition of these types, or the whole point of Phase 0.5 (identical detection/economics logic, divergent transport) breaks down.
Replace FixtureDetector in examples/run_sim.rs with the real detector implementation.
Deploy LiquidationArb.sol / MevOfa.sol onto the fork inside SimulationHarness::start (or a pre-run setup step) before running cycles, using the same bytecode/ABI you intend to deploy live. A deployment helper isn't included yet — add one that takes compiled artifact paths and returns deployed addresses, so the harness config can reference them.
Feed SimulationReport output into whatever tracks your phase-gate checklist (Phase 0.5 → 0.75 promotion criteria).
Reading a report
SimulationReport::summary_line() gives a conservative one-line summary:
cycles=50 attempted=41 success=33 failed=8 success_rate=80.5% net_profit_wei=-142000000000000
Note net_profit_wei can be — and often will be, at first — negative. That's the harness doing its job: catching opportunities that look profitable in shadow-mode heuristics but aren't once real pool depth, real loan fees, and real gas are accounted for.
Treat a single run's success_rate and net_profit_wei as one data point, not a verdict — vary --fork-block across multiple runs spanning different market conditions before drawing a promotion decision from it.