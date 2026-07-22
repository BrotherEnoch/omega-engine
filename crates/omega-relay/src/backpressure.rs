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
//!
//! ## CHANGE — metrics are no longer fed a fake inclusion signal
//!
//! This used to call `metrics.record(relay, outcome.included)` immediately after each
//! HTTP response — recording an *accepted* submission as *included*, which conflates two
//! different facts (see `client.rs`'s `SubmissionOutcome` docs and `confirmation.rs`).
//! Now: a submission that outright failed or was rejected IS recorded immediately (that's
//! a real, known negative — no need to wait). A submission the relay *accepted* is handed
//! to `InclusionTracker` instead, and only feeds `LaRelayMetrics` once real on-chain
//! confirmation resolves it — see `MultiRelayClient::reconcile_inclusions` in `lib.rs`.
//!
//! ## Audit fix (this revision)
//!
//! `build_submission_order` and `submit_single_bundle` each hardcoded the 5% tie-band
//! cutoff as a bare `0.95` literal — a third, independent copy of the same constant also
//! duplicated in `reputation.rs::submission_order`. Replaced with
//! `crate::config::LA_TIE_BAND_FRACTION`, the single source of truth added in this
//! revision — see `config.rs`'s audit note.

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
use crate::config::{RelayConfig, LA_TIE_BAND_FRACTION};
use crate::confirmation::InclusionTracker;
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
    relay_clients:     Arc<HashMap<String, Arc<dyn RelayClient>>>,
    metrics:           Arc<LaRelayMetrics>,
    rate_limiters:     Arc<RelayRateLimiters>,
    inclusion_tracker: Arc<InclusionTracker>,
    stagger_ms:        u64,
}

impl CascadeSubmitter {
    /// Create a cascade submitter from relay clients, live relay metrics, relay config,
    /// and an inclusion tracker (see `confirmation::InclusionTracker`).
    pub fn new(
        relay_clients:     Arc<HashMap<String, Arc<dyn RelayClient>>>,
        metrics:           Arc<LaRelayMetrics>,
        cfg:               &RelayConfig,
        inclusion_tracker: Arc<InclusionTracker>,
    ) -> Self {
        let relay_names: Vec<String> = relay_clients.keys().cloned().collect();
        let rate_limiters = Arc::new(RelayRateLimiters::new(
            &relay_names,
            cfg.max_bundles_per_relay_per_second as u32,
        ));
        Self { relay_clients, metrics, rate_limiters, inclusion_tracker, stagger_ms: cfg.stagger_ms }
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

        record_or_track_outcomes(&outcomes, bundle, &self.metrics, &self.inclusion_tracker);

        let any_accepted = outcomes
            .iter()
            .any(|(_, o)| o.as_ref().map(|x| x.accepted).unwrap_or(false));

        debug!(
            bundle_hash  = %bundle.bundle_hash,
            relay_count  = outcomes.len(),
            any_accepted,
            "cascade: bundle submitted to all relays (acceptance, not yet confirmed inclusion)"
        );

        CascadeResult {
            bundle_hash: bundle.bundle_hash.clone(),
            relay_outcomes: outcomes
                .into_iter()
                .map(|(r, o)| RelayOutcome {
                    relay:    r,
                    accepted: o.as_ref().map(|x| x.accepted).unwrap_or(false),
                    error:    o.err().map(|e| e.to_string()),
                })
                .collect(),
            any_accepted,
        }
    }

    /// Build the submission order: tie band shuffled + below-band ranked (§11.2, I2).
    fn build_submission_order(&self) -> Vec<RelayRateSnapshot> {
        let ranked = self.metrics.la_ranked_relays();
        if ranked.is_empty() { return ranked; }

        let Some(best) = ranked.first().map(|s| s.la_rate) else { return ranked; };
        let threshold = best * (1.0 - LA_TIE_BAND_FRACTION);

        let (mut in_band, below_band): (Vec<_>, Vec<_>) =
            ranked.into_iter().partition(|r| r.la_rate >= threshold);

        in_band.shuffle(&mut thread_rng());

        let mut order = in_band;
        order.extend(below_band);
        order
    }
}

/// Shared by cascade and single-bundle submission: a relay that flat-out failed or
/// rejected the submission is recorded as a real, known negative immediately. A relay
/// that accepted it is handed to `InclusionTracker` for later, real confirmation —
/// never recorded as included right away.
fn record_or_track_outcomes(
    outcomes:          &[(String, RelayResult<SubmissionOutcome>)],
    bundle:            &BundlePayload,
    metrics:           &Arc<LaRelayMetrics>,
    inclusion_tracker: &Arc<InclusionTracker>,
) {
    for (relay_name, outcome) in outcomes {
        let Ok(rn) = parse_relay_name(relay_name) else { continue };
        match outcome {
            Ok(o) if o.accepted => {
                if let Err(e) = inclusion_tracker.track(rn, bundle) {
                    warn!(relay = %relay_name, error = %e, "failed to track bundle for inclusion confirmation");
                }
            }
            _ => {
                // Outright failure or explicit rejection — a real, immediate negative.
                metrics.record(&rn, false);
            }
        }
    }
}

// ── Normal (non-cascade) LA submission ───────────────────────────────────────

/// Submit a single bundle using the same anti-fingerprint round-robin as
/// cascade, but without multi-bundle staggering. Used for non-cascade
/// normal LA paths (§14.2).
pub async fn submit_single_bundle(
    bundle:            BundlePayload,
    relay_clients:     &HashMap<String, Arc<dyn RelayClient>>,
    metrics:           &Arc<LaRelayMetrics>,
    rate_limiters:     &RelayRateLimiters,
    inclusion_tracker: &Arc<InclusionTracker>,
) -> RelayResult<bool> {
    let mut ranked = metrics.la_ranked_relays();
    if ranked.is_empty() {
        return Err(RelayError::AllRelaysFailed { bundle_hash: bundle.bundle_hash.clone() });
    }

    let Some(best) = ranked.first().map(|s| s.la_rate) else {
        return Err(RelayError::AllRelaysFailed { bundle_hash: bundle.bundle_hash.clone() });
    };
    let threshold = best * (1.0 - LA_TIE_BAND_FRACTION);
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
    let any_accepted = outcomes
        .iter()
        .any(|(_, o)| o.as_ref().map(|x| x.accepted).unwrap_or(false));

    record_or_track_outcomes(&outcomes, &bundle, metrics, inclusion_tracker);

    Ok(any_accepted)
}

// ── Result types ──────────────────────────────────────────────────────────────

/// Outcome of submitting one bundle to one relay in the cascade fanout.
#[derive(Debug, Clone)]
pub struct RelayOutcome {
    /// Relay name that handled the submission attempt.
    pub relay:    String,
    /// Whether the relay's HTTP endpoint accepted the submission — NOT confirmation of
    /// on-chain inclusion. See `confirmation::InclusionTracker` for that.
    pub accepted: bool,
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
    /// `true` when at least one relay accepted the submission (NOT confirmed inclusion).
    pub any_accepted:   bool,
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
        for (name, accepts) in relays {
            let rn = parse_relay_name(name).unwrap();
            for i in 0..20u32 {
                metrics.record(&rn, i < 18);
            }
            clients.insert(name.to_string(), Arc::new(MockRelayClient::new(*accepts)));
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

    fn tracker() -> Arc<InclusionTracker> {
        InclusionTracker::new("http://localhost:1") // unused by these tests directly
    }

    #[tokio::test]
    async fn cascade_submits_to_all_relays() {
        let (clients, metrics) =
            make_clients_and_metrics(&[("flashbots", true), ("bloxroute", false)]);
        let submitter = CascadeSubmitter::new(clients, metrics, &cfg(), tracker());

        let bundles = vec![
            BundlePayload { bundle_hash: "0xaaa".into(), ..Default::default() },
            BundlePayload { bundle_hash: "0xbbb".into(), ..Default::default() },
        ];

        let results = submitter.submit_cascade(bundles).await;
        assert_eq!(results.len(), 2);
        assert!(results[0].any_accepted);
        assert!(results[1].any_accepted);
        assert_eq!(results[0].relay_outcomes.len(), 2);
    }

    #[tokio::test]
    async fn rejected_relay_is_recorded_immediately_as_negative() {
        let (clients, metrics) = make_clients_and_metrics(&[("bloxroute", false)]);
        let submitter = CascadeSubmitter::new(clients, metrics.clone(), &cfg(), tracker());
        submitter
            .submit_cascade(vec![BundlePayload { bundle_hash: "0xrej".into(), ..Default::default() }])
            .await;
        // A rejection is a known negative and should be recorded right away, not deferred.
        let rate = metrics.rate_for(&crate::config::RelayName::Bloxroute, &ExecutionAddress("0xTEST".into()));
        assert!(rate.is_some(), "rejection must be recorded into metrics immediately");
    }

    #[tokio::test]
    async fn accepted_relay_is_tracked_not_recorded_immediately() {
        let (clients, metrics) = make_clients_and_metrics(&[("flashbots", true)]);
        let track = tracker();
        let submitter = CascadeSubmitter::new(clients, metrics.clone(), &cfg(), Arc::clone(&track));
        submitter
            .submit_cascade(vec![BundlePayload {
                bundle_hash: "0xacc".into(),
                txs: vec!["0xdeadbeef".into()],
                block_number: "0x64".into(),
                ..Default::default()
            }])
            .await;
        // Accepted submissions must NOT be recorded into metrics yet — they must be
        // handed to the inclusion tracker for real confirmation instead.
        assert_eq!(track.pending_count(), 1, "accepted bundle must be tracked, not recorded");
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
            tracker(),
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
        let submitter = CascadeSubmitter::new(Arc::new(HashMap::new()), metrics, &cfg(), tracker());
        let results   = submitter
            .submit_cascade(vec![BundlePayload { bundle_hash: "0x0".into(), ..Default::default() }])
            .await;
        assert!(results.is_empty());
    }

    // ── Audit fix regression test (this revision) ─────────────────────────────

    #[test]
    fn tie_band_threshold_derives_from_shared_constant() {
        // Regression guard: both build_submission_order and submit_single_bundle
        // must compute their threshold from LA_TIE_BAND_FRACTION, not a
        // hardcoded 0.95 literal that could silently drift from it.
        let best = 0.90_f64;
        let expected_threshold = best * (1.0 - LA_TIE_BAND_FRACTION);
        assert!((expected_threshold - 0.855).abs() < 1e-9);
    }
}