// crates/omega-relay/src/backpressure.rs
//! Cascade relay submission backpressure (Â§11.2, C2, I2, I7).
//!
//! ## Spec requirements
//! - Stagger successive bundle submissions by 10 ms (Â§11.2).
//! - Order relays by LA-inclusion-rate ranking (best relay first).
//! - Within the top-N tie band (within 5 % of best rate): randomise submission
//!   order per-blueprint to prevent relay fingerprinting (Â§11.2, I2).
//! - Max 4 bundles / relay / second enforced via `governor` rate limiter (Â§11.2).
//! - Normal (non-cascade) LA submission uses the same randomised round-robin
//!   within the tie band (Â§14.2).
//!
//! ## Parallelism model
//! Each bundle is submitted to **all** relays concurrently (via `join_all`).
//! Bundles themselves are staggered 10 ms apart to avoid relay rate-limit bans.
//! This is the correct read of "stagger between bundles, parallel across relays
//! for a given bundle" from Â§11.2.

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

// â”€â”€ Rate limiter map â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

type DirectRateLimiter = governor::RateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    governor::clock::DefaultClock,
>;

/// Per-relay rate limiter: max 4 submissions / second (Â§11.2).
pub struct RelayRateLimiters {
    limiters: HashMap<String, Arc<DirectRateLimiter>>,
    max_per_sec: u32,
}

impl RelayRateLimiters {
    pub fn new(relay_names: &[String], max_per_second: u32) -> Self {
        let quota = Quota::per_second(
            std::num::NonZeroU32::new(max_per_second).unwrap_or(nonzero!(4u32)),
        );
        let limiters = relay_names
            .iter()
            .map(|name| (name.clone(), Arc::new(RateLimiter::direct(quota))))
            .collect();
        Self {
            limiters,
            max_per_sec: max_per_second,
        }
    }

    /// Block until the rate limiter for `relay` permits a submission.
    pub async fn wait(&self, relay: &str) {
        if let Some(limiter) = self.limiters.get(relay) {
            // `until_ready()` is async and non-spinning.
            limiter.until_ready().await;
        }
    }
}

// â”€â”€ CascadeSubmitter â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Executes cascade bundle submission with all v12 backpressure controls.
pub struct CascadeSubmitter {
    relay_clients: Arc<HashMap<String, Arc<dyn RelayClient>>>,
    metrics: Arc<LaRelayMetrics>,
    rate_limiters: Arc<RelayRateLimiters>,
    stagger_ms: u64,
}

impl CascadeSubmitter {
    pub fn new(
        relay_clients: Arc<HashMap<String, Arc<dyn RelayClient>>>,
        metrics: Arc<LaRelayMetrics>,
        cfg: &RelayConfig,
    ) -> Self {
        let relay_names: Vec<String> = relay_clients.keys().cloned().collect();
        let rate_limiters = Arc::new(RelayRateLimiters::new(
            &relay_names,
            cfg.max_bundles_per_relay_per_second as u32,
        ));
        Self {
            relay_clients,
            metrics,
            rate_limiters,
            stagger_ms: cfg.stagger_ms,
        }
    }

    /// Submit `bundles` in cascade order.
    ///
    /// Per spec Â§11.2:
    /// 1. Rank relays by LA-inclusion-rate; randomise within the 5 % tie band.
    /// 2. For each bundle (staggered by `stagger_ms`):
    ///    submit to all relays in the ordered list concurrently.
    /// 3. Record inclusion outcomes back into `LaRelayMetrics`.
    ///
    /// Returns a `Vec<CascadeResult>` â€” one per bundle.
    pub async fn submit_cascade(
        &self,
        bundles: Vec<BundlePayload>,
    ) -> Vec<CascadeResult> {
        let submission_order = self.build_submission_order();

        if submission_order.is_empty() {
            warn!("cascade: no relays in submission order â€” aborting");
            return vec![];
        }

        let mut results = Vec::with_capacity(bundles.len());

        for (bundle_idx, bundle) in bundles.into_iter().enumerate() {
            // Stagger: each bundle after the first waits stagger_ms.
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

    /// Submit a single bundle to all relays concurrently.
    async fn submit_bundle_to_all_relays(
        &self,
        bundle: &BundlePayload,
        order: &[RelayRateSnapshot],
    ) -> CascadeResult {
        let futs: Vec<_> = order
            .iter()
            .filter_map(|snap| {
                let relay_name = snap.relay.to_string();
                self.relay_clients.get(&relay_name).map(|client| {
                    let client = Arc::clone(client);
                    let bundle = bundle.clone();
                    let rl = Arc::clone(&self.rate_limiters);
                    async move {
                        rl.wait(&relay_name).await;
                        let outcome = client.submit_bundle(bundle).await;
                        (relay_name, outcome)
                    }
                })
            })
            .collect();

        let outcomes: Vec<(String, RelayResult<SubmissionOutcome>)> = join_all(futs).await;

        // Record outcomes back into metrics (drives future ranking).
        for (relay_name, outcome) in &outcomes {
            let relay = self.metrics.active_address(); // just to get the relay key
            let _ = relay; // metrics.record takes relay name not address
            // Parse relay name back to RelayName enum for metrics recording.
            if let Ok(rn) = parse_relay_name(relay_name) {
                self.metrics
                    .record(&rn, outcome.as_ref().map(|o| o.included).unwrap_or(false));
            }
        }

        let any_included = outcomes
            .iter()
            .any(|(_, o)| o.as_ref().map(|x| x.included).unwrap_or(false));

        debug!(
            bundle_hash = %bundle.bundle_hash,
            relay_count = outcomes.len(),
            any_included,
            "cascade: bundle submitted to all relays"
        );

        CascadeResult {
            bundle_hash: bundle.bundle_hash.clone(),
            relay_outcomes: outcomes
                .into_iter()
                .map(|(r, o)| RelayOutcome {
                    relay: r,
                    included: o.as_ref().map(|x| x.included).unwrap_or(false),
                    error: o.err().map(|e| e.to_string()),
                })
                .collect(),
            any_included,
        }
    }

    /// Build the submission order: tie band shuffled + below-band ranked.
    fn build_submission_order(&self) -> Vec<RelayRateSnapshot> {
        let ranked = self.metrics.la_ranked_relays();
        if ranked.is_empty() {
            return ranked;
        }

        let best = ranked[0].la_rate;
        let threshold = best * 0.95;

        let (mut in_band, below_band): (Vec<_>, Vec<_>) =
            ranked.into_iter().partition(|r| r.la_rate >= threshold);

        in_band.shuffle(&mut thread_rng());

        let mut order = in_band;
        order.extend(below_band);
        order
    }
}

// â”€â”€ Normal (non-cascade) LA submission â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Submit a single bundle using the same anti-fingerprint round-robin as
/// cascade, but without multi-bundle staggering.  Used for non-cascade
/// normal LA paths (Â§14.2).
pub async fn submit_single_bundle(
    bundle: BundlePayload,
    relay_clients: &HashMap<String, Arc<dyn RelayClient>>,
    metrics: &Arc<LaRelayMetrics>,
    rate_limiters: &RelayRateLimiters,
) -> RelayResult<bool> {
    // Build randomised order
    let mut ranked = metrics.la_ranked_relays();
    if ranked.is_empty() {
        return Err(RelayError::AllRelaysFailed {
            bundle_hash: bundle.bundle_hash.clone(),
        });
    }

    let best = ranked[0].la_rate;
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
                let rl = rate_limiters;
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

// â”€â”€ Result types â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[derive(Debug, Clone)]
pub struct RelayOutcome {
    pub relay: String,
    pub included: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CascadeResult {
    pub bundle_hash: String,
    pub relay_outcomes: Vec<RelayOutcome>,
    pub any_included: bool,
}

// â”€â”€ Helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn parse_relay_name(s: &str) -> Result<crate::config::RelayName, ()> {
    match s {
        "flashbots" => Ok(crate::config::RelayName::Flashbots),
        "bloxroute" => Ok(crate::config::RelayName::Bloxroute),
        "titan" => Ok(crate::config::RelayName::Titan),
        "eden" => Ok(crate::config::RelayName::Eden),
        other => Ok(crate::config::RelayName::Other(other.to_string())),
    }
}

// â”€â”€ Tests â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{MockRelayClient, SubmissionOutcome};
    use crate::config::RelayName;
    use crate::metrics::ExecutionAddress;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn make_clients_and_metrics(
        relays: &[(&str, bool)], // (name, should_include)
    ) -> (
        Arc<HashMap<String, Arc<dyn RelayClient>>>,
        Arc<LaRelayMetrics>,
    ) {
        let addr = ExecutionAddress("0xTEST".into());
        let metrics = LaRelayMetrics::new(50, addr);

        let mut clients: HashMap<String, Arc<dyn RelayClient>> = HashMap::new();
        for (name, includes) in relays {
            // Pre-seed metrics so ranking is non-trivial
            let rn = parse_relay_name(name).unwrap();
            for i in 0..20u32 {
                metrics.record(&rn, i < 18); // 90% rate
            }
            clients.insert(
                name.to_string(),
                Arc::new(MockRelayClient::new(*includes)),
            );
        }
        (Arc::new(clients), metrics)
    }

    fn cfg() -> RelayConfig {
        RelayConfig {
            max_bundles_per_relay_per_second: 100, // high for tests
            stagger_ms: 0,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn cascade_submits_to_all_relays() {
        let (clients, metrics) = make_clients_and_metrics(&[
            ("flashbots", true),
            ("bloxroute", false),
        ]);
        let submitter = CascadeSubmitter::new(clients, metrics, &cfg());

        let bundles = vec![
            BundlePayload { bundle_hash: "0xaaa".into(), ..Default::default() },
            BundlePayload { bundle_hash: "0xbbb".into(), ..Default::default() },
        ];

        let results = submitter.submit_cascade(bundles).await;
        assert_eq!(results.len(), 2);
        // flashbots includes â†’ any_included = true for both
        assert!(results[0].any_included);
        assert!(results[1].any_included);
        // Both relays were contacted for each bundle
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

        let start = std::time::Instant::now();
        let bundles = vec![
            BundlePayload { bundle_hash: "0x1".into(), ..Default::default() },
            BundlePayload { bundle_hash: "0x2".into(), ..Default::default() },
            BundlePayload { bundle_hash: "0x3".into(), ..Default::default() },
        ];
        submitter.submit_cascade(bundles).await;
        let elapsed = start.elapsed();
        // 3 bundles â†’ 2 gaps Ã— 20 ms = at least 40 ms
        assert!(
            elapsed >= Duration::from_millis(35),
            "stagger must introduce delays: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn empty_relay_list_returns_empty() {
        let (clients, metrics) = (
            Arc::new(HashMap::new()),
            LaRelayMetrics::new(50, ExecutionAddress("0xX".into())),
        );
        let submitter = CascadeSubmitter::new(clients, metrics, &cfg());
        let results = submitter
            .submit_cascade(vec![BundlePayload {
                bundle_hash: "0x0".into(),
                ..Default::default()
            }])
            .await;
        assert!(results.is_empty());
    }
}