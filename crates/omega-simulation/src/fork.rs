// omega-engine\crates\omega-simulation\src\fork.rs
//! Local forked-node lifecycle. Owns the child Anvil process; dropping
//! `ForkHandle` kills the node and discards all state, so a run can't leave
//! residue a later run might accidentally rely on.

use crate::error::{Result, SimError};
use ethers::providers::{Http, Middleware, Provider};
use ethers::utils::{Anvil, AnvilInstance};
use std::sync::Arc;
use std::time::Duration;

/// Configuration for spawning a local forked node.
#[derive(Debug, Clone)]
pub struct ForkConfig {
    /// Read-only RPC endpoint for the chain being forked (e.g. an Arbitrum
    /// full node/provider URL). Only ever used for reads during fork sync.
    pub upstream_rpc_url: String,
    /// Block to fork from. `None` forks from the latest block.
    pub fork_block_number: Option<u64>,
    /// Local port for the spawned Anvil instance. `0` lets Anvil pick a
    /// free port.
    pub port: u16,
    /// Number of dev-funded accounts Anvil should generate.
    pub dev_accounts: u32,
    /// How long to wait for the forked node to respond before giving up.
    pub startup_timeout: Duration,
}

/// A running local fork of upstream chain state.
pub struct ForkHandle {
    _anvil: AnvilInstance,
    provider: Arc<Provider<Http>>,
    endpoint: String,
}

impl ForkHandle {
    /// Spawns a new local Anvil fork per `cfg` and waits until it accepts
    /// RPC calls.
    pub async fn spawn(cfg: ForkConfig) -> Result<Self> {
        let mut builder = Anvil::new()
            .fork(cfg.upstream_rpc_url.clone())
            .args(["--accounts", &cfg.dev_accounts.to_string()]);

        if cfg.port != 0 {
            builder = builder.port(cfg.port);
        }
        if let Some(block) = cfg.fork_block_number {
            builder = builder.fork_block_number(block);
        }

        // FIX (this revision): `try_spawn()` doesn't exist on this
        // workspace's pinned `ethers` version — the compiler's own
        // suggestion (E0599) points at `spawn()` instead. Unlike
        // `try_spawn()` (which presumably returned a `Result` in a newer
        // `ethers`), `Anvil::spawn()` here returns `AnvilInstance`
        // directly and PANICS internally if the child process fails to
        // start or never becomes reachable (e.g. `anvil` not on PATH).
        // This is a real behavior change from the prior
        // `.map_err(SimError::ForkSpawnFailed)?` shape: a spawn failure
        // now aborts the calling task via panic instead of returning
        // `Err(SimError::ForkSpawnFailed(..))`. Flagging this rather than
        // silently absorbing it — if callers of `ForkHandle::spawn` (e.g.
        // a long-running harness) need spawn failures to be a normal,
        // recoverable `Err` rather than a panic, that needs either a
        // newer `ethers` with a real `try_spawn`, or a
        // `std::panic::catch_unwind` wrapper here, neither of which I've
        // added since both are bigger decisions than a compile fix
        // warrants on their own.
        let anvil = builder.spawn();

        let endpoint = anvil.endpoint();
        let provider = Provider::<Http>::try_from(endpoint.clone())
            .map_err(|e| SimError::ForkUnreachable(e.to_string()))?;

        Self::wait_until_ready(&provider, cfg.startup_timeout).await?;

        tracing::info!(
            endpoint = %endpoint,
            fork_block = ?cfg.fork_block_number,
            "local fork ready"
        );

        Ok(Self {
            _anvil: anvil,
            provider: Arc::new(provider),
            endpoint,
        })
    }

    /// Spawns a fork suitable for unit tests. Requires `TEST_FORK_RPC_URL`
    /// (any EVM chain's read-only RPC works). Tests using this are marked
    /// `#[ignore]` by default since they need network access to an external
    /// provider — run them explicitly with `cargo test -- --ignored`.
    #[cfg(test)]
    pub async fn new_test() -> Result<Self> {
        let rpc_url = std::env::var("TEST_FORK_RPC_URL").map_err(|_| {
            SimError::ForkSpawnFailed(
                "TEST_FORK_RPC_URL must be set to a read-only RPC endpoint to run fork-backed tests"
                    .into(),
            )
        })?;

        Self::spawn(ForkConfig {
            upstream_rpc_url: rpc_url,
            fork_block_number: None,
            port: 0,
            dev_accounts: 5,
            startup_timeout: Duration::from_secs(30),
        })
        .await
    }

    async fn wait_until_ready(provider: &Provider<Http>, timeout: Duration) -> Result<()> {
        let start = std::time::Instant::now();
        loop {
            match provider.get_block_number().await {
                Ok(_) => return Ok(()),
                Err(e) => {
                    if start.elapsed() > timeout {
                        return Err(SimError::ForkUnreachable(e.to_string()));
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    }

    /// Advances the fork by exactly one block. Used by the harness between
    /// cycles so successive iterations sample different states even when
    /// no opportunity was found (Anvil otherwise only auto-mines in
    /// response to a submitted transaction).
    pub async fn mine_block(&self) -> Result<()> {
        self.provider
            .request::<_, serde_json::Value>("evm_mine", ())
            .await
            .map_err(SimError::Provider)?;
        Ok(())
    }

    pub fn provider(&self) -> Arc<Provider<Http>> {
        self.provider.clone()
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}