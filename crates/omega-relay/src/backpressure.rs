// crates/omega-relay/src/backpressure.rs
//! Cascade relay submission backpressure (§11.2, C2, I2, I7).
//!
//! ## Spec requirements
//! - Stagger successive bundle submissions by 10 ms (§11.2).
//! - Order relays by LA-inclusion-rate ranking (best relay first).
//! - Within the top-N tie band (within 5 % of best rate): randomise submission
//!   order per-blueprint to prevent relay fingerprinting (§11.2, I2).
//! - Max 4 bundles / relay / second enforced via `governor` rate limiter (§11.2).
//! - Normal (non-cascade) LA submission uses the same randomised round-robin
//!   within the tie band (§14.2).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::future::join_all;
use governor::{Quota, RateLimiter};
use nonzero_ext::nonzero;
use rand::seq::SliceRandom;
use rand::thread_rng;
use tokio::time::sleep;
use tracing::{debug, warn};

use crate::client::{BundlePayload, RelayClient, SubmissionOutcome};
use crate::config::RelayConfig;
use crate::error::{RelayError, RelayResult};
use crate::metrics::{LaRelayMetrics, RelayRateSnapshot};

// ── Rate limiter map ──────────────────────────────────────────────────────────

type DirectRateLimiter = governor::RateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    governor::clock::DefaultClock,
>;

/// Per-relay rate limiters capping submissions at `max_per_second` (§11.2).
pub struct RelayRateLimiters {
    limiters: HashMap<String, Arc<DirectRateLimiter>>,
}

impl RelayRateLimiters {
    /// Build per-relay direct rate limiters from a list of relay names.
    pub fn new(relay_names: &[String], max_per_second: u32) -> Self {
        let quota = Quota::per_second(
            std::num::NonZeroU32::new(max_per_second).unwrap_or(nonzero!(4u32)),
        );
        let limiters = relay_names
            .iter()
            .map(|name| (name.clone(), Arc::new(RateLimiter::direct(quota))))
            .collect();
        Self { limiters }
    }

    /// Block asynchronously until the rate limiter for `relay` permits a submission.
    pub async fn wait(&self, relay: &str) {
        if let Some(limiter) = self.limiters.get(relay) {
            limiter.until_ready().await;
        }
    }
}

// ── CascadeSubmitter ──────────────────────────────────────────────────────────

/// Type alias for the relay-clients-and-metrics pair returned by the test
/// helper, avoiding a complex inline return type.
#[cfg(test)]
type ClientsAndMetrics = (
    Arc<HashMap<String, Arc<dyn RelayClient>>>,
    Arc<LaRelayMetrics>,
);

/// Executes cascade bundle submission with all v12 backpressure controls (§11.2).
pub struct CascadeSubmitter {
    relay_clients: Arc<HashMap<String, Arc<dyn RelayClient>>>,
    metrics:       Arc<LaRelayMetrics>,
    rate_limiters: Arc<RelayRateLimiters>,
    stagger_ms:    u64,
}

impl CascadeSubmitter {
    /// Create a cascade submitter from relay clients, live relay metrics, and relay config.
    pub fn new(
        relay_clients: Arc<HashMap<String, Arc<dyn RelayClient>>>,
        metrics:       Arc<LaRelayMetrics>,
        cfg:           &RelayConfig,
    ) -> Self {
        let relay_names: Vec<String> = relay_clients.keys().cloned().collect();
        let rate_limiters = Arc::new(RelayRateLimiters::new(
            &relay_names,
            cfg.max_bundles_per_relay_per_second as u32,
        ));
        Self { relay_clients, metrics, rate_limiters, stagger_ms: cfg.stagger_ms }
    }

    /// Submit `bundles` in cascade order with inter-bundle stagger (§11.2).
    pub async fn submit_cascade(&self, bundles: Vec<BundlePayload>) -> Vec<CascadeResult> {
        let submission_order = self.build_submission_order();

        if submission_order.is_empty() {
            warn!("cascade: no relays in submission order — aborting");
            return vec![];
        }

        let mut results = Vec::with_capacity(bundles.len());

        for (bundle_idx, bundle) in bundles.into_iter().enumerate() {
            if bundle_idx > 0 {
                sleep(Duration::from_millis(self.stagger_ms)).await;
            }
            let result = self
                .submit_bundle_to_all_relays(&bundle, &submission_order)
                .await;
            results.push(result);
        }

        results
    }

    async fn submit_bundle_to_all_relays(
        &self,
        bundle: &BundlePayload,
        order:  &[RelayRateSnapshot],
    ) -> CascadeResult {
        let futs: Vec<_> = order
            .iter()
            .filter_map(|snap| {
                let relay_name = snap.relay.to_string();
                self.relay_clients.get(&relay_name).map(|client| {
                    let client = Arc::clone(client);
                    let bundle = bundle.clone();
                    let rl     = Arc::clone(&self.rate_limiters);
                    async move {
                        rl.wait(&relay_name).await;
                        let outcome = client.submit_bundle(bundle).await;
                        (relay_name, outcome)
                    }
                })
            })
            .collect();

        let outcomes: Vec<(String, RelayResult<SubmissionOutcome>)> = join_all(futs).await;

        for (relay_name, outcome) in &outcomes {
            let relay = self.metrics.active_address();
            let _     = relay;
            if let Ok(rn) = parse_relay_name(relay_name) {
                self.metrics.record(
                    &rn,
                    outcome.as_ref().map(|o| o.included).unwrap_or(false),
                );
            }
        }

        let any_included = outcomes
            .iter()
            .any(|(_, o)| o.as_ref().map(|x| x.included).unwrap_or(false));

        debug!(
            bundle_hash  = %bundle.bundle_hash,
            relay_count  = outcomes.len(),
            any_included,
            "cascade: bundle submitted to all relays"
        );

        CascadeResult {
            bundle_hash: bundle.bundle_hash.clone(),
            relay_outcomes: outcomes
                .into_iter()
                .map(|(r, o)| RelayOutcome {
                    relay:    r,
                    included: o.as_ref().map(|x| x.included).unwrap_or(false),
                    error:    o.err().map(|e| e.to_string()),
                })
                .collect(),
            any_included,
        }
    }

    /// Build the submission order: tie band shuffled + below-band ranked (§11.2, I2).
    fn build_submission_order(&self) -> Vec<RelayRateSnapshot> {
        let ranked = self.metrics.la_ranked_relays();
        if ranked.is_empty() { return ranked; }

        let Some(best) = ranked.first().map(|s| s.la_rate) else { return ranked; };
        let threshold = best * 0.95;

        let (mut in_band, below_band): (Vec<_>, Vec<_>) =
            ranked.into_iter().partition(|r| r.la_rate >= threshold);

        in_band.shuffle(&mut thread_rng());

        let mut order = in_band;
        order.extend(below_band);
        order
    }
}

// ── Normal (non-cascade) LA submission ───────────────────────────────────────

/// Submit a single bundle using the same anti-fingerprint round-robin as
/// cascade, but without multi-bundle staggering. Used for non-cascade
/// normal LA paths (§14.2).
pub async fn submit_single_bundle(
    bundle:        BundlePayload,
    relay_clients: &HashMap<String, Arc<dyn RelayClient>>,
    metrics:       &Arc<LaRelayMetrics>,
    rate_limiters: &RelayRateLimiters,
) -> RelayResult<bool> {
    let mut ranked = metrics.la_ranked_relays();
    if ranked.is_empty() {
        return Err(RelayError::AllRelaysFailed { bundle_hash: bundle.bundle_hash.clone() });
    }

    let Some(best) = ranked.first().map(|s| s.la_rate) else {
        return Err(RelayError::AllRelaysFailed { bundle_hash: bundle.bundle_hash.clone() });
    };
    let threshold = best * 0.95;
    let (mut band, rest): (Vec<_>, Vec<_>) =
        ranked.drain(..).partition(|r| r.la_rate >= threshold);
    band.shuffle(&mut thread_rng());
    let order: Vec<_> = band.into_iter().chain(rest).collect();

    let futs: Vec<_> = order
        .iter()
        .filter_map(|snap| {
            let relay_name = snap.relay.to_string();
            relay_clients.get(&relay_name).map(|client| {
                let client = Arc::clone(client);
                let bundle = bundle.clone();
                let rl     = rate_limiters;
                async move {
                    rl.wait(&relay_name).await;
                    let outcome = client.submit_bundle(bundle).await;
                    (relay_name, outcome)
                }
            })
        })
        .collect();

    let outcomes: Vec<_> = join_all(futs).await;
    let any_included = outcomes
        .iter()
        .any(|(_, o)| o.as_ref().map(|x| x.included).unwrap_or(false));

    for (relay_name, outcome) in &outcomes {
        if let Ok(rn) = parse_relay_name(relay_name) {
            metrics.record(&rn, outcome.as_ref().map(|o| o.included).unwrap_or(false));
        }
    }

    Ok(any_included)
}

// ── Result types ──────────────────────────────────────────────────────────────

/// Outcome of submitting one bundle to one relay in the cascade fanout.
#[derive(Debug, Clone)]
pub struct RelayOutcome {
    /// Relay name that handled the submission attempt.
    pub relay:    String,
    /// Whether the relay reported inclusion.
    pub included: bool,
    /// Stringified submission error, if the relay request failed.
    pub error:    Option<String>,
}

/// Aggregate result for one bundle submitted across the cascade relay set.
#[derive(Debug, Clone)]
pub struct CascadeResult {
    /// Bundle hash associated with this cascade submission.
    pub bundle_hash:    String,
    /// Per-relay submission outcomes in submission order.
    pub relay_outcomes: Vec<RelayOutcome>,
    /// `true` when at least one relay reported inclusion.
    pub any_included:   bool,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_relay_name(s: &str) -> Result<crate::config::RelayName, ()> {
    match s {
        "flashbots" => Ok(crate::config::RelayName::Flashbots),
        "bloxroute" => Ok(crate::config::RelayName::Bloxroute),
        "titan"     => Ok(crate::config::RelayName::Titan),
        "eden"      => Ok(crate::config::RelayName::Eden),
        other       => Ok(crate::config::RelayName::Other(other.to_string())),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::type_complexity)]
mod tests {
    use super::*;
    use crate::client::MockRelayClient;
    use crate::metrics::ExecutionAddress;

    fn make_clients_and_metrics(relays: &[(&str, bool)]) -> ClientsAndMetrics {
        let addr    = ExecutionAddress("0xTEST".into());
        let metrics = LaRelayMetrics::new(50, addr);

        let mut clients: HashMap<String, Arc<dyn RelayClient>> = HashMap::new();
        for (name, includes) in relays {
            let rn = parse_relay_name(name).unwrap();
            for i in 0..20u32 {
                metrics.record(&rn, i < 18);
            }
            clients.insert(name.to_string(), Arc::new(MockRelayClient::new(*includes)));
        }
        (Arc::new(clients), metrics)
    }

    fn cfg() -> RelayConfig {
        RelayConfig {
            max_bundles_per_relay_per_second: 100,
            stagger_ms: 0,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn cascade_submits_to_all_relays() {
        let (clients, metrics) =
            make_clients_and_metrics(&[("flashbots", true), ("bloxroute", false)]);
        let submitter = CascadeSubmitter::new(clients, metrics, &cfg());

        let bundles = vec![
            BundlePayload { bundle_hash: "0xaaa".into(), ..Default::default() },
            BundlePayload { bundle_hash: "0xbbb".into(), ..Default::default() },
        ];

        let results = submitter.submit_cascade(bundles).await;
        assert_eq!(results.len(), 2);
        assert!(results[0].any_included);
        assert!(results[1].any_included);
        assert_eq!(results[0].relay_outcomes.len(), 2);
    }

    #[tokio::test]
    async fn stagger_delays_between_bundles() {
        let (clients, metrics) = make_clients_and_metrics(&[("flashbots", true)]);
        let submitter = CascadeSubmitter::new(
            clients,
            metrics,
            &RelayConfig {
                stagger_ms: 20,
                max_bundles_per_relay_per_second: 100,
                ..Default::default()
            },
        );

        let start   = std::time::Instant::now();
        let bundles = vec![
            BundlePayload { bundle_hash: "0x1".into(), ..Default::default() },
            BundlePayload { bundle_hash: "0x2".into(), ..Default::default() },
            BundlePayload { bundle_hash: "0x3".into(), ..Default::default() },
        ];
        submitter.submit_cascade(bundles).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(35),
            "stagger must introduce delays: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn empty_relay_list_returns_empty() {
        let metrics   = LaRelayMetrics::new(50, ExecutionAddress("0xX".into()));
        let submitter = CascadeSubmitter::new(Arc::new(HashMap::new()), metrics, &cfg());
        let results   = submitter
            .submit_cascade(vec![BundlePayload { bundle_hash: "0x0".into(), ..Default::default() }])
            .await;
        assert!(results.is_empty());
    }
}