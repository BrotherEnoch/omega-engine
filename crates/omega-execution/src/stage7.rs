// crates/omega-execution/src/stage7.rs
//
// Stage 7 — Confirmation reconciliation.
//
// ## Spec (ExecutionPipelineSpecification.md § Stage 7)
//
// `MultiRelayClient::reconcile_inclusions` already resolves pending bundles
// against real chain receipts. This module owns the *driver* and the
// *side-effects* that must follow each `ConfirmationResult`:
//
//   1. Feed kill-switch outcomes (success / failure + provisional realized P&L)
//   2. Advance NonceRegistry only on confirmed inclusion
//   3. Structured audit logging so every trade is reconstructable
//
// ## Realized profit policy (financial safety)
//
// On-chain inclusion only tells us the txs landed with status=1. True
// vault `netProfit` lives in `OmegaVault.pending_profit[blueprintHash]`.
// Prefer `AsyncRealizedProfitLookup` (async eth_call) over the sync trait.
// When lookup returns None, use `expected_profit_net_wei` as provisional.
//
// ## Async error handling
//
// Vault lookup failures must never panic the Stage-7 task. They log at
// `warn` and fall back to provisional expected profit so kill-switch
// consecutive-failure accounting still advances on `included`/`missed`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use omega_relay::{ConfirmationResult, MultiRelayClient};
use omega_risk::kill_switch::KillSwitchRegistry;
use omega_security::NonceRegistry;
use tracing::{error, info, warn};

/// Operator-tunable Stage-7 behaviour.
#[derive(Debug, Clone)]
pub struct Stage7Config {
    /// When true, a missed inclusion with positive expected profit is
    /// recorded as a negative realized figure (forces window/cumulative
    /// loss accounting). Default false — missed opportunities are not
    /// the same as capital loss.
    pub count_missed_profit: bool,
    /// Chain id used when advancing NonceRegistry.
    pub chain_id: u64,
}

impl Stage7Config {
    pub fn from_env(chain_id: u64) -> Self {
        let count_missed = std::env::var("OMEGA_KS_COUNT_MISSED_PROFIT")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        Self {
            count_missed_profit: count_missed,
            chain_id,
        }
    }
}

/// Sync lookup (tests / no-op). Prefer `AsyncRealizedProfitLookup` on the
/// live Stage-7 path so HTTP never blocks the tokio worker.
pub trait RealizedProfitLookup: Send + Sync {
    fn realized_profit_wei(&self, blueprint_hash_hex: &str) -> Option<i128>;
}

/// Async lookup for vault pending_profit (production path).
#[async_trait]
pub trait AsyncRealizedProfitLookup: Send + Sync {
    async fn realized_profit_wei(&self, blueprint_hash_hex: &str) -> Option<i128>;
}

/// No-op lookup: always returns None (provisional path only).
pub struct NoRealizedProfitLookup;

impl RealizedProfitLookup for NoRealizedProfitLookup {
    fn realized_profit_wei(&self, _blueprint_hash_hex: &str) -> Option<i128> {
        None
    }
}

#[async_trait]
impl AsyncRealizedProfitLookup for NoRealizedProfitLookup {
    async fn realized_profit_wei(&self, _blueprint_hash_hex: &str) -> Option<i128> {
        None
    }
}

/// One processed confirmation after kill-switch / nonce side-effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedConfirmation {
    pub bundle_hash: String,
    pub strategy_id: String,
    pub included: bool,
    pub nonce: u64,
    /// Provisional realized P&L fed to the kill switch (see module doc).
    pub provisional_realized_wei: Option<i128>,
    /// Vault lookup result when an async/sync lookup was used.
    pub vault_realized_wei: Option<i128>,
    /// Kill-switch trip reason if `record_outcome` tripped the switch.
    pub kill_trip_reason: Option<String>,
}

fn scope_of(r: &ConfirmationResult) -> String {
    if r.strategy_id.is_empty() {
        "global".to_string()
    } else {
        r.strategy_id.clone()
    }
}

fn provisional_from(
    r: &ConfirmationResult,
    cfg: &Stage7Config,
    vault: Option<i128>,
) -> Option<i128> {
    if r.included {
        match vault {
            Some(v) if v != 0 => Some(v),
            _ => Some(r.expected_profit_net_wei.min(i128::MAX as u128) as i128),
        }
    } else if cfg.count_missed_profit && r.expected_profit_net_wei > 0 {
        Some(-(r.expected_profit_net_wei.min(i128::MAX as u128) as i128))
    } else {
        None
    }
}

fn apply_one(
    r: &ConfirmationResult,
    kill_switches: &KillSwitchRegistry,
    nonce_registry: &NonceRegistry,
    cfg: &Stage7Config,
    vault: Option<i128>,
) -> ProcessedConfirmation {
    let scope = scope_of(r);
    let provisional_realized = provisional_from(r, cfg, vault);

    let kill_trip_reason = kill_switches
        .record_outcome(&scope, provisional_realized, r.included)
        .map(|reason| reason.to_string());

    if let Some(ref reason) = kill_trip_reason {
        error!(
            reason = %reason,
            strategy = %scope,
            included = r.included,
            nonce = r.nonce,
            bundle_hash = %r.bundle_hash,
            blueprint_hash = %r.blueprint_hash,
            provisional_realized_wei = ?provisional_realized,
            vault_realized_wei = ?vault,
            "Stage-7: kill switch tripped after reconciliation"
        );
    }

    if r.included && !r.strategy_id.is_empty() {
        let _ = nonce_registry.record_processed(&r.strategy_id, cfg.chain_id, r.nonce);
    }

    crate::audit::log_confirmed(
        chrono::Utc::now(),
        &scope,
        &r.blueprint_hash,
        &r.bundle_hash,
        r.included,
        provisional_realized,
        vault,
        kill_trip_reason.is_some(),
    );

    info!(
        bundle_hash = %r.bundle_hash,
        blueprint_hash = %r.blueprint_hash,
        strategy = %scope,
        included = r.included,
        nonce = r.nonce,
        provisional_realized_wei = ?provisional_realized,
        vault_realized_wei = ?vault,
        kill_tripped = kill_trip_reason.is_some(),
        "Stage-7: confirmation processed"
    );

    ProcessedConfirmation {
        bundle_hash: r.bundle_hash.clone(),
        strategy_id: scope,
        included: r.included,
        nonce: r.nonce,
        provisional_realized_wei: provisional_realized,
        vault_realized_wei: vault,
        kill_trip_reason,
    }
}

/// Pure processing without vault lookup (provisional expected profit only).
pub fn process_confirmation_results(
    results: &[ConfirmationResult],
    kill_switches: &KillSwitchRegistry,
    nonce_registry: &NonceRegistry,
    cfg: &Stage7Config,
) -> Vec<ProcessedConfirmation> {
    results
        .iter()
        .map(|r| apply_one(r, kill_switches, nonce_registry, cfg, None))
        .collect()
}

/// Sync lookup path (unit tests / offline). Prefer `process_confirmation_results_async`.
pub fn process_confirmation_results_with_lookup(
    results: &[ConfirmationResult],
    kill_switches: &KillSwitchRegistry,
    nonce_registry: &NonceRegistry,
    cfg: &Stage7Config,
    lookup: &dyn RealizedProfitLookup,
) -> Vec<ProcessedConfirmation> {
    results
        .iter()
        .map(|r| {
            let vault = if r.included && !r.blueprint_hash.is_empty() {
                lookup.realized_profit_wei(&r.blueprint_hash)
            } else {
                None
            };
            apply_one(r, kill_switches, nonce_registry, cfg, vault)
        })
        .collect()
}

/// Production path: async vault lookups, never blocks the runtime on sync HTTP.
///
/// Lookup errors are absorbed as `None` (provisional expected profit). Callers
/// that need hard failure should implement that in the lookup itself by
/// logging via `tracing` before returning None.
pub async fn process_confirmation_results_async(
    results: &[ConfirmationResult],
    kill_switches: &KillSwitchRegistry,
    nonce_registry: &NonceRegistry,
    cfg: &Stage7Config,
    lookup: &dyn AsyncRealizedProfitLookup,
) -> Vec<ProcessedConfirmation> {
    let mut out = Vec::with_capacity(results.len());
    for r in results {
        let vault = if r.included && !r.blueprint_hash.is_empty() {
            match lookup.realized_profit_wei(&r.blueprint_hash).await {
                Some(v) => Some(v),
                None => {
                    warn!(
                        blueprint_hash = %r.blueprint_hash,
                        bundle_hash = %r.bundle_hash,
                        "Stage-7: vault profit lookup returned None — using provisional expected P&L"
                    );
                    None
                }
            }
        } else {
            None
        };
        out.push(apply_one(r, kill_switches, nonce_registry, cfg, vault));
    }
    out
}

/// Drive Stage 7 from unit notifications (oracle bridge).
pub async fn run_stage7_reconciliation_loop(
    relay: Arc<MultiRelayClient>,
    kill_switches: Arc<KillSwitchRegistry>,
    nonce_registry: NonceRegistry,
    mut block_rx: tokio::sync::broadcast::Receiver<()>,
    current_block_fn: impl Fn() -> u64,
    cfg: Stage7Config,
    lookup: Arc<dyn AsyncRealizedProfitLookup>,
) {
    info!(
        chain_id = cfg.chain_id,
        count_missed_profit = cfg.count_missed_profit,
        "Stage-7 reconciliation loop started (async vault lookup)"
    );

    loop {
        match block_rx.recv().await {
            Ok(()) => {
                let current_block = current_block_fn();
                let results = relay.reconcile_inclusions(current_block).await;
                if !results.is_empty() {
                    let _ = process_confirmation_results_async(
                        &results,
                        kill_switches.as_ref(),
                        &nonce_registry,
                        &cfg,
                        lookup.as_ref(),
                    )
                    .await;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                warn!(skipped = n, "Stage-7 reconciliation loop lagged");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                info!("Stage-7 reconciliation loop exiting (channel closed)");
                break;
            }
        }
    }
}

/// Interval-driven variant when no block broadcast is available.
pub async fn run_stage7_interval_loop(
    relay: Arc<MultiRelayClient>,
    kill_switches: Arc<KillSwitchRegistry>,
    nonce_registry: NonceRegistry,
    current_block_fn: impl Fn() -> u64,
    cfg: Stage7Config,
    interval: Duration,
    lookup: Arc<dyn AsyncRealizedProfitLookup>,
) {
    let mut ticker = tokio::time::interval(interval);
    info!(
        chain_id = cfg.chain_id,
        interval_secs = interval.as_secs(),
        "Stage-7 interval reconciliation loop started"
    );

    loop {
        ticker.tick().await;
        let current_block = current_block_fn();
        let results = relay.reconcile_inclusions(current_block).await;
        if !results.is_empty() {
            let _ = process_confirmation_results_async(
                &results,
                kill_switches.as_ref(),
                &nonce_registry,
                &cfg,
                lookup.as_ref(),
            )
            .await;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use omega_relay::config::RelayName;
    use omega_risk::kill_switch::KillSwitchConfig;
    use std::time::Duration as StdDuration;

    fn sample_result(
        strategy: &str,
        included: bool,
        profit: u128,
        nonce: u64,
    ) -> ConfirmationResult {
        ConfirmationResult {
            bundle_hash: format!("0xbundle{nonce}"),
            relay: RelayName::Flashbots,
            included,
            strategy_id: strategy.to_string(),
            nonce,
            expected_profit_net_wei: profit,
            blueprint_hash: format!("0x{:0>64}", format!("{nonce:x}")),
        }
    }

    fn ks_registry() -> KillSwitchRegistry {
        KillSwitchRegistry::new(KillSwitchConfig {
            max_cumulative_loss_wei: 10u128.pow(18) * 100,
            max_loss_per_window_wei: 10u128.pow(18) * 50,
            loss_window: StdDuration::from_secs(3600),
            max_consecutive_failures: 5,
        })
        .expect("valid kill config")
    }

    #[test]
    fn included_records_positive_provisional() {
        let ks = ks_registry();
        let nr = NonceRegistry::new();
        let cfg = Stage7Config {
            count_missed_profit: false,
            chain_id: 42161,
        };
        let results = vec![sample_result("SA", true, 1_000_000_000_000_000_000, 3)];
        let processed = process_confirmation_results(&results, &ks, &nr, &cfg);
        assert_eq!(processed.len(), 1);
        assert!(processed[0].included);
        assert_eq!(
            processed[0].provisional_realized_wei,
            Some(1_000_000_000_000_000_000)
        );
        assert!(processed[0].kill_trip_reason.is_none());
    }

    #[test]
    fn missed_without_count_missed_records_failure_without_pnl() {
        let ks = ks_registry();
        let nr = NonceRegistry::new();
        let cfg = Stage7Config {
            count_missed_profit: false,
            chain_id: 42161,
        };
        let results = vec![sample_result("MSA", false, 5_000_000_000_000_000_000, 1)];
        let processed = process_confirmation_results(&results, &ks, &nr, &cfg);
        assert!(!processed[0].included);
        assert_eq!(processed[0].provisional_realized_wei, None);
    }

    #[test]
    fn consecutive_misses_can_trip_kill_switch() {
        let ks = KillSwitchRegistry::new(KillSwitchConfig {
            max_cumulative_loss_wei: 10u128.pow(18) * 1000,
            max_loss_per_window_wei: 10u128.pow(18) * 1000,
            loss_window: StdDuration::from_secs(3600),
            max_consecutive_failures: 3,
        })
        .expect("valid kill config");
        let nr = NonceRegistry::new();
        let cfg = Stage7Config {
            count_missed_profit: false,
            chain_id: 42161,
        };
        let batch: Vec<_> = (0..3).map(|i| sample_result("MEV", false, 0, i)).collect();
        let processed = process_confirmation_results(&batch, &ks, &nr, &cfg);
        assert!(processed[2].kill_trip_reason.is_some());
        assert!(ks.guard("MEV").is_err());
    }

    struct FixedLookup(i128);
    impl RealizedProfitLookup for FixedLookup {
        fn realized_profit_wei(&self, _: &str) -> Option<i128> {
            Some(self.0)
        }
    }

    #[test]
    fn vault_lookup_overrides_expected() {
        let ks = ks_registry();
        let nr = NonceRegistry::new();
        let cfg = Stage7Config {
            count_missed_profit: false,
            chain_id: 42161,
        };
        let results = vec![sample_result("SA", true, 9_000_000_000_000_000_000, 1)];
        let processed =
            process_confirmation_results_with_lookup(&results, &ks, &nr, &cfg, &FixedLookup(42));
        assert_eq!(processed[0].provisional_realized_wei, Some(42));
        assert_eq!(processed[0].vault_realized_wei, Some(42));
    }
}
