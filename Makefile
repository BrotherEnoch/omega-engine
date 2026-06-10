// Makefile
# Makefile
.PHONY: check build test fmt clippy contracts certora shadow

check:
	cargo check --workspace

build:
	cargo build --workspace --release

test:
	cargo test --workspace

fmt:
	cargo fmt --all

clippy:
	cargo clippy --workspace -- -D warnings

contracts:
	cd contracts && forge build && forge test

certora:
	certoraRun certora/specs/Orchestrator.spec --msg "OmegaEngine v12 Orchestrator"
	certoraRun certora/specs/Vault.spec --msg "OmegaEngine v12 Vault"

shadow:
	cargo run --bin omega-shadow -- \
		--config config/arbitrum.toml \
		--fork-url $$ARBITRUM_RPC_URL \
		--duration-days 21 \
		--output-dir ./shadow-output \
		--competition-stress

backtest:
	cargo run --bin omega-backtest -- \
		--days 30 \
		--rpc-url $$ARBITRUM_RPC_URL \
		--output-dir ./backtest-output

control-plane:
	cargo run --bin omega-control-plane

docker-build:
	docker build -t omega-engine:v12 .
