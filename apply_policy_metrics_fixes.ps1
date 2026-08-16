# apply_policy_metrics_fixes.ps1
# Run from C:\Users\silve\Documents\omega-engine
# Writes: corrected omega-compliance/policy.rs (default allowed_strategies bug) and omega-hot-path/metrics.rs (clippy allow)
$ErrorActionPreference = 'Stop'

Write-Host 'Writing crates\omega-compliance\src\policy.rs...'
$content_0 = @'
// crates/omega-compliance/src/policy.rs
//
// ## Fix (this revision): asset_symbol()/notional_value() don't exist on
// ExecutionBlueprint, and can't be implemented on it
//
// `validate_blueprint` previously called `bp.asset_symbol()` and
// `bp.notional_value()` — neither method exists on
// `omega_core::types::blueprint::ExecutionBlueprint`
// (`error[E0599]: no method named ... found`). The original code's own
// comments ("Implement or adapt to your blueprint fields", "Implement
// helper if needed") mark these as unfinished stubs, not a real
// implementation that just needs wiring up.
//
// They can't be added to `ExecutionBlueprint` itself, either: that
// struct's actual fields are `flashloan_provider: Address`,
// `flashloan_amount: U256` (raw token units), and
// `expected_profit_net: U256` (wei) — there is no human-readable token
// symbol anywhere on it, and no USD-denominated price. Deriving an
// "asset symbol" from a raw contract address, or a "notional value" in
// USD without a price oracle, would mean fabricating exactly the data
// this compliance check exists to verify — an allowlist/position-size
// gate that silently guesses at the asset and dollar value it's
// checking is worse than one that fails to compile, since a wrong guess
// here fails *open* (a disallowed asset or oversized position reads as
// compliant) rather than failing loudly.
//
// This crate's own dependency list (see imports below: `omega_core`,
// `chrono`, `serde`, `thiserror` — no `omega_oracle`) confirms it has no
// price-feed access of its own to compute either value correctly.
//
// Fixed by making both values explicit, caller-supplied parameters to
// `validate_blueprint` instead of methods on the blueprint. The caller
// — whatever code sits between the oracle/pricing layer and this
// compliance gate — already has to resolve "what token does this
// blueprint touch, and what's it worth in USD" for other reasons (gas
// cost accounting, profit reporting); this makes that resolution an
// explicit, visible input to the compliance decision rather than an
// implicit method call that silently returns nothing meaningful. This
// is a breaking signature change for any existing caller of
// `validate_blueprint`; there was no way to fix the underlying missing
// data without one.
//
// ## Fix (this revision, 2): sample_blueprint missing 4
// ExecutionBlueprint fields
//
// `omega-core` added four more required fields to `ExecutionBlueprint`
// (`flashloan_provider_type`, `provider_contract`, `flashloan_token`,
// `max_base_fee_gwei`) to support real flashloan provider/pool
// selection — see that crate's `types::blueprint` module doc comment.
// `sample_blueprint` here predates them
// (`error[E0063]: missing fields flashloan_provider_type,
// flashloan_token, max_base_fee_gwei and 1 other field`).
// `ComplianceChecker::validate_blueprint` reads none of the four — its
// checks are asset/chain/position-size/time-window/strategy only — so
// these are inert placeholders, same treatment as every other
// test-only `ExecutionBlueprint` literal fixed elsewhere in this
// workspace: `flashloan_provider_type: FlashloanProviderType::Balancer`
// / `provider_contract: Address::ZERO` / `flashloan_token: Address::ZERO`
// alongside the existing `flashloan_provider: Address::ZERO` no-flashloan
// path, and `max_base_fee_gwei` derived from `base_fee_at_creation * 3`
// matching the placeholder headroom multiplier used in
// `omega-strategies`' `sa.rs`/`la.rs`/`msa.rs`/`mev.rs`.

// ## Fix (this revision, 3): CompliancePolicy::default()'s
// allowed_strategies could never match any real blueprint
//
// `bp.strategy_id.to_string()` (via `StrategyId`'s `Display` impl in
// `omega_core::types::blueprint`) produces uppercase abbreviations —
// `"SA"`, `"CNRY"`, `"MSA"`, `"LA"`, `"MEV"` — not the lowercase
// `["mev", "flashloan"]` the previous default listed. `"flashloan"`
// additionally isn't a `StrategyId` variant at all (flashloan is a
// capital-sourcing mechanism a strategy may use, not a strategy
// itself), so it could never match regardless of casing. The net
// effect: the strategy-allowlist check in `validate_blueprint` would
// reject every single real blueprint the compliance gate was ever
// asked to check — not merely a strict default, but a default that
// failed even the passing-case test
// (`allowed_asset_and_chain_and_size_passes`, whose `sample_blueprint`
// uses `StrategyId::Sa` → `"SA"`) with `expected .is_ok(), got Err`.
// Fixed by listing the real, correctly-cased strategies this policy
// intends to permit by default: SA, MSA, LA, MEV. CNRY is deliberately
// excluded — per `omega-strategies`' own `cnry.rs` module doc comment,
// CNRY never produces a real, submittable blueprint (`build_blueprint`
// always returns `Err`), so there is nothing for this compliance gate
// to ever check for it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

use omega_core::types::blueprint::ExecutionBlueprint; // Adjust import as needed in your core

#[derive(Debug, Clone, Error)]
pub enum ComplianceError {
    #[error("Policy violation: {0}")]
    Violation(String),
    #[error("Configuration error: {0}")]
    Config(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TimeWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompliancePolicy {
    pub allowed_assets: Vec<String>,
    pub allowed_chains: Vec<u64>,
    pub max_position_size_usd: f64,
    pub max_leverage_bps: u16,
    pub trading_windows: Vec<TimeWindow>,
    pub cooldown_period_secs: u64,
    pub allowed_strategies: Vec<String>,
}

impl Default for CompliancePolicy {
    fn default() -> Self {
        Self {
            allowed_assets: vec!["ETH".into(), "BTC".into(), "USDC".into()],
            allowed_chains: vec![42161], // Arbitrum mainnet
            max_position_size_usd: 100_000.0,
            max_leverage_bps: 5000, // 50x example
            trading_windows: vec![],
            cooldown_period_secs: 300,
            // Real, correctly-cased StrategyId::Display values — see
            // this file's module-level "Fix (this revision, 3)" note.
            // CNRY excluded deliberately: it never produces a real
            // blueprint for this checker to validate.
            allowed_strategies: vec![
                "SA".into(),
                "MSA".into(),
                "LA".into(),
                "MEV".into(),
            ],
        }
    }
}

#[derive(Debug)]
pub struct ComplianceChecker {
    policy: Arc<CompliancePolicy>,
}

impl ComplianceChecker {
    pub fn new(policy: CompliancePolicy) -> Self {
        Self {
            policy: Arc::new(policy),
        }
    }

    /// Validate a blueprint against the configured compliance policy.
    ///
    /// `asset_symbol` and `notional_value_usd` are supplied by the
    /// caller rather than read off `bp` — see this file's module-level
    /// "Fix" note for why: `ExecutionBlueprint` carries a raw
    /// `flashloan_provider` address and wei-denominated amounts, not a
    /// human-readable symbol or a USD price, so resolving either
    /// requires a price/token-metadata lookup this crate has no access
    /// to. The caller (sitting closer to the oracle/pricing layer) is
    /// expected to resolve both before calling this.
    pub fn validate_blueprint(
        &self,
        bp: &ExecutionBlueprint,
        asset_symbol: &str,
        notional_value_usd: f64,
        now: DateTime<Utc>,
    ) -> Result<(), ComplianceError> {
        // Asset permission
        if !self.policy.allowed_assets.iter().any(|a| a == asset_symbol) {
            return Err(ComplianceError::Violation(format!(
                "Asset {asset_symbol} not allowed"
            )));
        }

        // Chain permission
        if !self.policy.allowed_chains.contains(&bp.chain_id) {
            return Err(ComplianceError::Violation(format!(
                "Chain {} not allowed",
                bp.chain_id
            )));
        }

        // Position size
        if notional_value_usd > self.policy.max_position_size_usd {
            return Err(ComplianceError::Violation(
                "Exceeds max position size".into(),
            ));
        }

        // Time window
        if !self.is_in_trading_window(now) {
            return Err(ComplianceError::Violation(
                "Outside allowed trading window".into(),
            ));
        }

        // Strategy
        if !self
            .policy
            .allowed_strategies
            .contains(&bp.strategy_id.to_string())
        {
            return Err(ComplianceError::Violation("Strategy not allowed".into()));
        }

        Ok(())
    }

    fn is_in_trading_window(&self, now: DateTime<Utc>) -> bool {
        if self.policy.trading_windows.is_empty() {
            return true; // No restriction
        }
        self.policy
            .trading_windows
            .iter()
            .any(|w| w.start <= now && now <= w.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, Bytes, B256, U256};
    use omega_core::types::blueprint::StrategyId;
    use omega_core::types::flashloan_provider::FlashloanProviderType;
    use omega_core::types::lane::{Lane, Simulator};
    use uuid::Uuid;

    fn sample_blueprint(chain_id: u64) -> ExecutionBlueprint {
        let signal_id = Uuid::from_bytes([0xAAu8; 16]);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(StrategyId::Sa, chain_id, 1, signal_id);
        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO,
            chain_id,
            strategy_id: StrategyId::Sa,
            lane: Lane::Microtx,
            simulator: Simulator::Revm,
            signal_state_hash: B256::from([0xABu8; 32]),
            state_version: 1,
            signal_id,
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::from(1_000_000u64),
            flashloan_available: U256::from(2_000_000u64),
            flashloan_provider_type: FlashloanProviderType::Balancer,
            provider_contract: Address::ZERO,
            flashloan_token: Address::ZERO,
            calldata: Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]),
            strategy_bytecode_hash: B256::from([0xCDu8; 32]),
            l2_exec_gas_estimate: 100_000,
            l1_data_gas_estimate: 5_000,
            extraction_gas: 45_000,
            expected_profit_net: U256::from(1u64),
            dynamic_min_profit: U256::from(1u64),
            l2_buffer_factor: 1.15,
            l1_data_buffer_factor: 1.10,
            slippage_bps: 20,
            base_fee_at_creation: 1,
            l1_data_fee_at_creation: 40,
            priority_fee_gwei: 10,
            max_base_fee_gwei: 3, // base_fee_at_creation * 3 — see module note
            price_impact_bps: None,
            ofa_compliant: true,
            expiry_block: 1_000,
            nonce: 1,
            confirmation_depth: 12,
            client_order_id,
            idempotency_key: B256::ZERO,
            relay_targets: vec![],
            zk_proof_commitment: None,
        };
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();
        bp
    }

    #[test]
    fn allowed_asset_and_chain_and_size_passes() {
        let checker = ComplianceChecker::new(CompliancePolicy::default());
        let bp = sample_blueprint(42161);
        let now = Utc::now();
        assert!(checker
            .validate_blueprint(&bp, "ETH", 50_000.0, now)
            .is_ok());
    }

    #[test]
    fn disallowed_asset_is_rejected() {
        let checker = ComplianceChecker::new(CompliancePolicy::default());
        let bp = sample_blueprint(42161);
        let now = Utc::now();
        let err = checker
            .validate_blueprint(&bp, "DOGE", 1_000.0, now)
            .unwrap_err();
        assert!(matches!(err, ComplianceError::Violation(msg) if msg.contains("DOGE")));
    }

    #[test]
    fn disallowed_chain_is_rejected() {
        let checker = ComplianceChecker::new(CompliancePolicy::default());
        let bp = sample_blueprint(1); // Ethereum mainnet, not in default allowed_chains
        let now = Utc::now();
        let err = checker
            .validate_blueprint(&bp, "ETH", 1_000.0, now)
            .unwrap_err();
        assert!(matches!(err, ComplianceError::Violation(_)));
    }

    #[test]
    fn oversized_position_is_rejected() {
        let checker = ComplianceChecker::new(CompliancePolicy::default());
        let bp = sample_blueprint(42161);
        let now = Utc::now();
        let err = checker
            .validate_blueprint(&bp, "ETH", 1_000_000.0, now) // > 100_000 default cap
            .unwrap_err();
        assert!(matches!(err, ComplianceError::Violation(msg) if msg.contains("position size")));
    }

    #[test]
    fn empty_trading_windows_means_unrestricted() {
        let checker = ComplianceChecker::new(CompliancePolicy::default());
        assert!(checker.is_in_trading_window(Utc::now()));
    }

    #[test]
    fn outside_configured_trading_window_is_rejected() {
        let mut policy = CompliancePolicy::default();
        let now = Utc::now();
        policy.trading_windows = vec![TimeWindow {
            start: now - chrono::Duration::hours(2),
            end: now - chrono::Duration::hours(1),
        }];
        let checker = ComplianceChecker::new(policy);
        let bp = sample_blueprint(42161);
        let err = checker
            .validate_blueprint(&bp, "ETH", 1_000.0, now)
            .unwrap_err();
        assert!(matches!(err, ComplianceError::Violation(msg) if msg.contains("trading window")));
    }
}

'@
Set-Content -Path 'crates\omega-compliance\src\policy.rs' -Value $content_0 -Encoding UTF8 -NoNewline

Write-Host 'Writing crates\omega-hot-path\src\metrics.rs...'
$content_1 = @'
// crates/omega-hot-path/src/metrics.rs
//
// HotPathMetrics — observability for the <1ms Microtx execution lane.
//
// ## Spec §4, §16
//
//   The hot-path simulation SLA is <1ms per blueprint.  HotPathMetrics
//   tracks:
//     - p50/p95/p99 simulation latency (via a compact reservoir)
//     - Miss rate (simulations rejected before execution)
//     - Successes and total EV captured
//     - Slot utilisation (live vs capacity)
//
//   All counters use atomics — the metrics struct is `Clone + Send + Sync`
//   and safe to read from any thread without locking.  The shadow scorecard
//   `sim_latency_p95_ms` metric reads directly from this struct.
//
// ## Latency histogram
//
//   We use a fixed-width histogram over microseconds with 8 buckets:
//     [0,100), [100,250), [250,500), [500,1000), [1000,2000),
//     [2000,5000), [5000,10000), [10000,∞)
//
//   The SLA target is 1000µs (<1ms).  Buckets 0–3 are within-SLA;
//   buckets 4–7 are SLA violations.  A separate `sla_violations` counter
//   tracks the total number of executions that exceeded 1ms.
//
//   p95 is estimated from the histogram by summing bucket counts until
//   ≥ 95% of total observations are covered.
//
// ## Fix (this revision): clippy::expect_used in snapshot_is_serialisable
//
// Same root cause as every other test-module clippy failure in this
// crate (see gate.rs's own note): this crate's Cargo.toml `[lints]`
// table sets `clippy::expect_used` to "warn" unconditionally (no
// `cfg(test)` carve-out possible at the manifest level), and
// `cargo clippy -- -D warnings` promotes that to a hard error for this
// module's single, ordinary test-only `.expect("serialisable")` call.
// Scoped `#[allow]` added to `mod tests` here, matching gate.rs/
// simulator.rs/lib.rs in this same crate.

use std::sync::atomic::{AtomicU64, Ordering};

use alloy_primitives::U256;
use chrono::{DateTime, Utc};
use serde::Serialize;

// ─────────────────────────────────────────────────────────────────────────────
// Histogram buckets
// ─────────────────────────────────────────────────────────────────────────────

/// Upper bounds (exclusive) of the latency histogram buckets in microseconds.
///
/// The final bucket `u64::MAX` is the catch-all for everything above 10ms.
pub const LATENCY_BUCKETS_US: &[u64] = &[100, 250, 500, 1_000, 2_000, 5_000, 10_000, u64::MAX];

const NUM_BUCKETS: usize = 8;

/// The hot-path latency SLA in microseconds (spec §4: <1ms).
pub const SLA_US: u64 = 1_000;

// ─────────────────────────────────────────────────────────────────────────────
// HotPathMetrics
// ─────────────────────────────────────────────────────────────────────────────

/// Thread-safe observability metrics for the Microtx execution lane.
///
/// Shared via `Arc<HotPathMetrics>`.  All write operations use
/// `Relaxed` ordering — metrics are eventually-consistent diagnostics,
/// not synchronisation primitives.
pub struct HotPathMetrics {
    /// Total blueprints that entered simulation (accepted + rejected).
    pub total: AtomicU64,
    /// Blueprints that completed simulation successfully.
    pub successes: AtomicU64,
    /// Blueprints rejected by any simulation guard (expiry, gas, profit).
    pub misses: AtomicU64,
    /// Simulations that exceeded the 1ms SLA.
    pub sla_violations: AtomicU64,
    /// Sum of latencies for all successful simulations (microseconds).
    pub latency_sum_us: AtomicU64,
    /// Sum of net profit wei captured across all successful simulations.
    /// Stored as two u64 halves (high, low) to avoid overflow.
    total_profit_hi: AtomicU64,
    total_profit_lo: AtomicU64,
    /// Latency histogram: count per bucket (see `LATENCY_BUCKETS_US`).
    histogram: [AtomicU64; NUM_BUCKETS],
    /// Timestamp of the last successful simulation.
    pub last_success_at: std::sync::Mutex<Option<DateTime<Utc>>>,
}

impl HotPathMetrics {
    /// Create a zeroed metrics instance.
    pub fn new() -> Self {
        Self {
            total: AtomicU64::new(0),
            successes: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            sla_violations: AtomicU64::new(0),
            latency_sum_us: AtomicU64::new(0),
            total_profit_hi: AtomicU64::new(0),
            total_profit_lo: AtomicU64::new(0),
            histogram: std::array::from_fn(|_| AtomicU64::new(0)),
            last_success_at: std::sync::Mutex::new(None),
        }
    }

    /// Record a successful simulation result.
    pub fn record_success(&self, latency_us: u64, profit_net: U256) {
        self.total.fetch_add(1, Ordering::Relaxed);
        self.successes.fetch_add(1, Ordering::Relaxed);
        self.latency_sum_us.fetch_add(latency_us, Ordering::Relaxed);

        if latency_us >= SLA_US {
            self.sla_violations.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(latency_us, sla_us = SLA_US, "Hot-path SLA exceeded");
        }

        self.increment_histogram(latency_us);
        self.add_profit(profit_net);

        if let Ok(mut guard) = self.last_success_at.lock() {
            *guard = Some(Utc::now());
        }
    }

    /// Record a simulation miss (any guard rejection before execution).
    pub fn record_miss(&self) {
        self.total.fetch_add(1, Ordering::Relaxed);
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Miss rate in [0.0, 1.0].
    pub fn miss_rate(&self) -> f64 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        self.misses.load(Ordering::Relaxed) as f64 / total as f64
    }

    /// Mean simulation latency in microseconds.
    pub fn mean_latency_us(&self) -> f64 {
        let n = self.successes.load(Ordering::Relaxed);
        if n == 0 {
            return 0.0;
        }
        self.latency_sum_us.load(Ordering::Relaxed) as f64 / n as f64
    }

    /// Mean simulation latency in milliseconds.
    pub fn mean_latency_ms(&self) -> f64 {
        self.mean_latency_us() / 1_000.0
    }

    /// Estimated p95 latency in microseconds from the histogram.
    pub fn p95_latency_us(&self) -> u64 {
        let total = self.successes.load(Ordering::Relaxed);
        if total < 20 {
            return 0;
        }

        let target = (total as f64 * 0.95).ceil() as u64;
        let mut cumulative = 0u64;

        for (i, bucket) in self.histogram.iter().enumerate() {
            cumulative += bucket.load(Ordering::Relaxed);
            if cumulative >= target {
                return LATENCY_BUCKETS_US[i];
            }
        }
        LATENCY_BUCKETS_US[NUM_BUCKETS - 2]
    }

    /// p95 latency in milliseconds.
    pub fn p95_latency_ms(&self) -> f64 {
        self.p95_latency_us() as f64 / 1_000.0
    }

    /// SLA compliance rate: fraction of simulations within 1ms.
    pub fn sla_compliance_rate(&self) -> f64 {
        let n = self.successes.load(Ordering::Relaxed);
        if n == 0 {
            return 1.0;
        }
        1.0 - (self.sla_violations.load(Ordering::Relaxed) as f64 / n as f64)
    }

    /// Total net profit captured in wei as a u128.
    pub fn total_profit_wei(&self) -> u128 {
        let hi = self.total_profit_hi.load(Ordering::Relaxed) as u128;
        let lo = self.total_profit_lo.load(Ordering::Relaxed) as u128;
        hi.saturating_mul(u64::MAX as u128 + 1).saturating_add(lo)
    }

    /// Produce an immutable snapshot for serialisation.
    pub fn snapshot(&self) -> HotPathMetricsSnapshot {
        HotPathMetricsSnapshot {
            total: self.total.load(Ordering::Relaxed),
            successes: self.successes.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            sla_violations: self.sla_violations.load(Ordering::Relaxed),
            miss_rate: self.miss_rate(),
            mean_latency_us: self.mean_latency_us(),
            p95_latency_us: self.p95_latency_us(),
            p95_latency_ms: self.p95_latency_ms(),
            sla_compliance_rate: self.sla_compliance_rate(),
            histogram: std::array::from_fn(|i| self.histogram[i].load(Ordering::Relaxed)),
        }
    }

    /// Reset all counters.
    pub fn reset(&self) {
        self.total.store(0, Ordering::Relaxed);
        self.successes.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
        self.sla_violations.store(0, Ordering::Relaxed);
        self.latency_sum_us.store(0, Ordering::Relaxed);
        self.total_profit_hi.store(0, Ordering::Relaxed);
        self.total_profit_lo.store(0, Ordering::Relaxed);
        for bucket in &self.histogram {
            bucket.store(0, Ordering::Relaxed);
        }
        if let Ok(mut g) = self.last_success_at.lock() {
            *g = None;
        }
    }

    // ── Private helpers ───────────────────────────────────────────────────

    fn increment_histogram(&self, latency_us: u64) {
        for (i, &upper) in LATENCY_BUCKETS_US.iter().enumerate() {
            if latency_us < upper {
                self.histogram[i].fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        self.histogram[NUM_BUCKETS - 1].fetch_add(1, Ordering::Relaxed);
    }

    fn add_profit(&self, profit: U256) {
        let as_u128 = if profit > U256::from(u128::MAX) {
            u128::MAX
        } else {
            profit.to::<u128>()
        };
        let hi = (as_u128 >> 64) as u64;
        let lo = as_u128 as u64;
        self.total_profit_hi.fetch_add(hi, Ordering::Relaxed);
        self.total_profit_lo.fetch_add(lo, Ordering::Relaxed);
    }
}

impl Default for HotPathMetrics {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// HotPathMetricsSnapshot
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct HotPathMetricsSnapshot {
    pub total: u64,
    pub successes: u64,
    pub misses: u64,
    pub sla_violations: u64,
    pub miss_rate: f64,
    pub mean_latency_us: f64,
    pub p95_latency_us: u64,
    pub p95_latency_ms: f64,
    pub sla_compliance_rate: f64,
    pub histogram: [u64; NUM_BUCKETS],
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

// See this file's module-level "Fix (this revision)" note: this crate's
// Cargo.toml `[lints]` table sets clippy::expect_used to "warn"
// unconditionally, and `cargo clippy -- -D warnings` promotes that to a
// hard error for this module's ordinary test-only `.expect(...)` call
// otherwise.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn new_metrics_are_zeroed() {
        let m = HotPathMetrics::new();
        assert_eq!(m.total.load(Ordering::Relaxed), 0);
        assert!((m.miss_rate() - 0.0).abs() < f64::EPSILON);
        assert_eq!(m.p95_latency_us(), 0);
    }

    #[test]
    fn record_success_increments_counters() {
        let m = HotPathMetrics::new();
        m.record_success(500, U256::from(1_000_000_u64));
        assert_eq!(m.total.load(Ordering::Relaxed), 1);
        assert_eq!(m.successes.load(Ordering::Relaxed), 1);
        assert_eq!(m.misses.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn record_miss_increments_counters() {
        let m = HotPathMetrics::new();
        m.record_miss();
        assert_eq!(m.total.load(Ordering::Relaxed), 1);
        assert_eq!(m.misses.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn miss_rate_correct() {
        let m = HotPathMetrics::new();
        for _ in 0..80 {
            m.record_success(300, U256::from(1_000_u64));
        }
        for _ in 0..20 {
            m.record_miss();
        }
        assert!((m.miss_rate() - 0.20).abs() < 1e-9);
    }

    #[test]
    fn sla_violation_recorded_when_latency_exceeds_1ms() {
        let m = HotPathMetrics::new();
        m.record_success(1_500, U256::from(1_000_u64));
        assert_eq!(m.sla_violations.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn no_sla_violation_under_1ms() {
        let m = HotPathMetrics::new();
        m.record_success(999, U256::from(1_000_u64));
        assert_eq!(m.sla_violations.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn p95_latency_requires_min_20_observations() {
        let m = HotPathMetrics::new();
        for _ in 0..19 {
            m.record_success(300, U256::from(1_u64));
        }
        assert_eq!(m.p95_latency_us(), 0);
    }

    #[test]
    fn p95_latency_estimated_from_histogram() {
        let m = HotPathMetrics::new();
        for _ in 0..95 {
            m.record_success(200, U256::from(1_u64));
        }
        for _ in 0..5 {
            m.record_success(1_500, U256::from(1_u64));
        }
        let p95 = m.p95_latency_us();
        assert!(
            p95 <= 500,
            "p95={p95} should be ≤500µs with 95% under 250µs"
        );
    }

    #[test]
    fn mean_latency_computed() {
        let m = HotPathMetrics::new();
        m.record_success(200, U256::ZERO);
        m.record_success(400, U256::ZERO);
        assert!((m.mean_latency_us() - 300.0).abs() < 1.0);
        assert!((m.mean_latency_ms() - 0.3).abs() < 0.001);
    }

    #[test]
    fn reset_clears_all_state() {
        let m = HotPathMetrics::new();
        m.record_success(500, U256::from(1_000_u64));
        m.record_miss();
        m.reset();
        assert_eq!(m.total.load(Ordering::Relaxed), 0);
        assert_eq!(m.successes.load(Ordering::Relaxed), 0);
        assert_eq!(m.misses.load(Ordering::Relaxed), 0);
        assert_eq!(m.p95_latency_us(), 0);
    }

    #[test]
    fn snapshot_is_serialisable() {
        let m = HotPathMetrics::new();
        m.record_success(300, U256::from(5_000_u64));
        let snap = m.snapshot();
        let json = serde_json::to_string(&snap).expect("serialisable");
        assert!(json.contains("\"successes\":1"));
    }
}

'@
Set-Content -Path 'crates\omega-hot-path\src\metrics.rs' -Value $content_1 -Encoding UTF8 -NoNewline

Write-Host ''
Write-Host 'Verifying...'
$check = Select-String -Path 'crates\omega-compliance\src\policy.rs' -Pattern '"SA".into\(\)' -Quiet
if ($check) { Write-Host '  OK: crates\omega-compliance\src\policy.rs' } else { Write-Host '  MISSING: crates\omega-compliance\src\policy.rs' -ForegroundColor Red }
$check = Select-String -Path 'crates\omega-hot-path\src\metrics.rs' -Pattern 'allow\(clippy::unwrap_used, clippy::expect_used\)' -Quiet
if ($check) { Write-Host '  OK: crates\omega-hot-path\src\metrics.rs' } else { Write-Host '  MISSING: crates\omega-hot-path\src\metrics.rs' -ForegroundColor Red }