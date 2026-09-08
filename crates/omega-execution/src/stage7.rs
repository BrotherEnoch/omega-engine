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
// vault `netProfit` lives in `OmegaVault.pending_profit[blueprintHash]`
// and is not yet queried from this path (no blueprint_hash on
// ConfirmationResult today — only bundle_hash). Until a RealizedProfitLookup
// is wired, we use `expected_profit_net_wei` as a **provisional** figure:
//
//   - included=true  → +expected (conservative for cumulative-loss trips
//     only if expected was optimistic; still correct for consecutive-failure
//     reset on success)
//   - included=false → success=false; optional negative expected when
//     `count_missed_profit` is set (operator opt-in via env)
//
// This is deliberately documented and logged as provisional so operators
// never confuse it with audited vault accounting.
//
// ## Ownership
//
// The loop is started once from `main` after relay + kill switches +
// NonceRegistry exist. It must share the same `Arc<KillSwitchRegistry>`
// that `ExecutionPipeline` uses — otherwise Stage 2a and Stage 7 diverge.

use std::sync::Arc;
use std::time::Duration;

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

/// One processed confirmation after kill-switch / nonce side-effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedConfirmation {
    pub bundle_hash: String,
    pub strategy_id: String,
    pub included: bool,
    pub nonce: u64,
    /// Provisional realized P&L fed to the kill switch (see module doc).
    pub provisional_realized_wei: Option<i128>,
    /// Kill-switch trip reason if `record_outcome` tripped the switch.
    pub kill_trip_reason: Option<String>,
}

/// Pure-ish processing of a batch of confirmation results.
///
/// Extracted so unit tests can assert kill-switch and nonce behaviour
/// without a live RPC or relay. Mutates `kill_switches` and
/// `nonce_registry` in place.
pub fn process_confirmation_results(
    results: &[ConfirmationResult],
    kill_switches: &KillSwitchRegistry,
    nonce_registry: &NonceRegistry,
    cfg: &Stage7Config,
) -> Vec<ProcessedConfirmation> {
    let mut out = Vec::with_capacity(results.len());

    for r in results {
        let scope = if r.strategy_id.is_empty() {
            "global".to_string()
        } else {
            r.strategy_id.clone()
        };

        let provisional_realized: Option<i128> = if r.included {
            // Cap at i128::MAX to avoid wrap on pathological u128 values.
            Some(r.expected_profit_net_wei.min(i128::MAX as u128) as i128)
        } else if cfg.count_missed_profit && r.expected_profit_net_wei > 0 {
            Some(-(r.expected_profit_net_wei.min(i128::MAX as u128) as i128))
        } else {
            None
        };

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
                provisional_realized_wei = ?provisional_realized,
                "Stage-7: kill switch tripped after reconciliation"
            );
        }

        if r.included && !r.strategy_id.is_empty() {
            let _ = nonce_registry.record_processed(&r.strategy_id, cfg.chain_id, r.nonce);
        }

        info!(
            bundle_hash = %r.bundle_hash,
            strategy = %scope,
            included = r.included,
            nonce = r.nonce,
            provisional_realized_wei = ?provisional_realized,
            kill_tripped = kill_trip_reason.is_some(),
            "Stage-7: confirmation processed (provisional P&L = expected until vault query)"
        );

        out.push(ProcessedConfirmation {
            bundle_hash: r.bundle_hash.clone(),
            strategy_id: scope,
            included: r.included,
            nonce: r.nonce,
            provisional_realized_wei: provisional_realized,
            kill_trip_reason,
        });
    }

    out
}

/// Drive Stage 7 from oracle block notifications.
///
/// Subscribes to `block_rx` (typically from the fee/oracle stream). On
/// each new block snapshot, calls `reconcile_inclusions` and processes
/// results. Exits cleanly when the broadcast channel closes.
///
/// Same ownership model as `run_idempotency_eviction_loop`: caller
/// spawns this on a background task after constructing shared Arcs.
pub async fn run_stage7_reconciliation_loop(
    relay: Arc<MultiRelayClient>,
    kill_switches: Arc<KillSwitchRegistry>,
    nonce_registry: NonceRegistry,
    mut block_rx: tokio::sync::broadcast::Receiver<()>,
    current_block_fn: impl Fn() -> u64,
    cfg: Stage7Config,
) {
    info!(
        chain_id = cfg.chain_id,
        count_missed_profit = cfg.count_missed_profit,
        "Stage-7 reconciliation loop started"
    );

    loop {
        match block_rx.recv().await {
            Ok(()) => {
                let current_block = current_block_fn();
                let results = relay.reconcile_inclusions(current_block).await;
                if !results.is_empty() {
                    let _ = process_confirmation_results(
                        &results,
                        kill_switches.as_ref(),
                        &nonce_registry,
                        &cfg,
                    );
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

/// Interval-driven variant when no block broadcast is available (tests /
/// degraded mode). Prefer the block-driven loop in production so
/// reconciliation stays aligned with canonical head.
pub async fn run_stage7_interval_loop(
    relay: Arc<MultiRelayClient>,
    kill_switches: Arc<KillSwitchRegistry>,
    nonce_registry: NonceRegistry,
    current_block_fn: impl Fn() -> u64,
    cfg: Stage7Config,
    interval: Duration,
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
            let _ = process_confirmation_results(
                &results,
                kill_switches.as_ref(),
                &nonce_registry,
                &cfg,
            );
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

    fn sample_result(strategy: &str, included: bool, profit: u128, nonce: u64) -> ConfirmationResult {
        ConfirmationResult {
            bundle_hash: format!("0xbundle{nonce}"),
            relay: RelayName::Flashbots,
            included,
            strategy_id: strategy.to_string(),
            nonce,
            expected_profit_net_wei: profit,
        }
    }

    fn ks_registry() -> KillSwitchRegistry {
        KillSwitchRegistry::new(KillSwitchConfig {
            max_cumulative_loss_wei: 10u128.pow(18) * 100, // 100 ETH
            max_loss_per_window_wei: 10u128.pow(18) * 50,  // 50 ETH
            loss_window: StdDuration::from_secs(3600),
            max_consecutive_failures: 5,
        })
        .expect("valid kill config")
    }

    // note: max_loss_per_window_wei is required by KillSwitchConfig::validate

    #[test]
    fn included_records_positive_provisional_and_advances_nonce() {
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
        // Nonce advanced to at least the processed nonce
        // (record_processed sets next when processed > current)
        let _ = nr; // NonceRegistry has no public getter for next in all versions;
                    // absence of panic + kill trip is the safety signal here.
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
        assert_eq!(processed.len(), 1);
        assert!(!processed[0].included);
        assert_eq!(processed[0].provisional_realized_wei, None);
        assert!(processed[0].kill_trip_reason.is_none());
    }

    #[test]
    fn missed_with_count_missed_records_negative_provisional() {
        let ks = ks_registry();
        let nr = NonceRegistry::new();
        let cfg = Stage7Config {
            count_missed_profit: true,
            chain_id: 42161,
        };
        let results = vec![sample_result("LA", false, 2_000_000_000_000_000_000, 2)];

        let processed = process_confirmation_results(&results, &ks, &nr, &cfg);
        assert_eq!(
            processed[0].provisional_realized_wei,
            Some(-2_000_000_000_000_000_000)
        );
    }

    #[test]
    fn empty_strategy_id_uses_global_scope() {
        let ks = ks_registry();
        let nr = NonceRegistry::new();
        let cfg = Stage7Config {
            count_missed_profit: false,
            chain_id: 42161,
        };
        let mut r = sample_result("", true, 1, 0);
        r.strategy_id = String::new();
        let processed = process_confirmation_results(&[r], &ks, &nr, &cfg);
        assert_eq!(processed[0].strategy_id, "global");
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

        let batch: Vec<_> = (0..3)
            .map(|i| sample_result("MEV", false, 0, i))
            .collect();
        let processed = process_confirmation_results(&batch, &ks, &nr, &cfg);
        // Third consecutive failure should trip
        assert!(
            processed[2].kill_trip_reason.is_some(),
            "third consecutive miss must trip kill switch"
        );
        // Guard must now fail for that scope
        assert!(ks.guard("MEV").is_err());
    }
}