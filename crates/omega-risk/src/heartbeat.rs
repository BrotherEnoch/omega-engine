// crates/omega-risk/src/heartbeat.rs
//
// Liveness / heartbeat tracking.
//
// Every other metric in this crate describes what a strategy or the risk
// layer is DOING (checks passed/failed, circuit breaker state, kill
// switch trips). None of them answer a more basic question: is the
// process that would be doing those things still running and still
// connected to whatever it depends on (RPC endpoint, relay, oracle feed)?
//
// A crashed process, a hung RPC connection, or a detector loop stuck on
// a panic-recovered task all look IDENTICAL to "market is quiet" if all
// you have is the checks/circuit-breaker/kill-switch metrics — those
// simply stop incrementing either way. This module exists so that
// distinction is visible: each component that should be periodically
// "alive" calls beat() on its own name; an external alert rule fires when
// a component's last beat is older than its own tolerance, independent
// of whether that component ever had anything to submit.
//
// Design notes:
//   • One HeartbeatRegistry per process, shared via Arc, mirroring the
//     CircuitBreakerRegistry / KillSwitchRegistry pattern already in this
//     crate.
//   • Each named component supplies its OWN expected interval at
//     registration time (a detector polling every block on Arbitrum has a
//     much tighter tolerance than, say, a daily reconciliation job) —
//     there is no single global staleness threshold.
//   • is_stale()/all_statuses() are pure reads with no side effects, so
//     they're safe to call from a health-check HTTP handler on every
//     request without perturbing state.
//   • Metrics: HEARTBEAT_LAST_BEAT_TIMESTAMP is a Unix-seconds gauge per
//     component, updated on every beat(). The staleness check itself
//     (`time() - omega_risk_heartbeat_last_beat_timestamp > tolerance`) is
//     expressed in the Prometheus alert rule, not computed here — this
//     module's job is to publish "when did this last check in," not to
//     own the alerting threshold logic, since Prometheus already owns
//     rate/staleness evaluation.
//
// ## Audit fix (this revision): deprecated chrono API
//
// The fallback in `status()`/`all_statuses()` for a `max_silence` whose
// `std::time::Duration` doesn't fit in `chrono::Duration`'s range now
// uses `chrono::Duration::MAX` instead of the deprecated
// `chrono::Duration::max_value()`. See the matching note in
// `kill_switch.rs`'s module doc comment — same fix, same reasoning,
// applied to the other three call sites in this crate.

use chrono::Utc;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::metrics;

#[derive(Debug, Clone)]
pub struct HeartbeatConfig {
    /// How long this component is allowed to go without a beat before
    /// it's considered stale. Purely informational at the Rust level (see
    /// `is_stale`/`all_statuses`) — the Prometheus alert rule re-expresses
    /// this same tolerance independently so alerting still works even if
    /// the process itself is the thing that died and can never report its
    /// own staleness.
    pub max_silence: Duration,
}

impl Default for HeartbeatConfig {
    /// 2 minutes — reasonable default for a tight per-block loop on an L2;
    /// override per component for anything slower by nature (e.g. an
    /// hourly reconciliation job should register with a much longer
    /// max_silence, not inherit this one).
    fn default() -> Self {
        Self {
            max_silence: Duration::from_secs(120),
        }
    }
}

#[derive(Debug, Clone)]
struct ComponentState {
    last_beat: chrono::DateTime<Utc>,
    config: HeartbeatConfig,
}

#[derive(Debug, Clone)]
pub struct ComponentStatus {
    pub component: String,
    pub last_beat: chrono::DateTime<Utc>,
    pub silence: Duration,
    pub max_silence: Duration,
    pub stale: bool,
}

/// Shared registry of per-component liveness beats.
#[derive(Clone)]
pub struct HeartbeatRegistry {
    components: Arc<DashMap<String, ComponentState>>,
}

impl HeartbeatRegistry {
    pub fn new() -> Self {
        Self {
            components: Arc::new(DashMap::new()),
        }
    }

    /// Explicitly register a component with a non-default tolerance.
    /// Optional — `beat()` will auto-register with `HeartbeatConfig::default()`
    /// if called on an unregistered component — but calling this first is
    /// recommended so intent (and the chosen tolerance) is visible in code
    /// rather than implied by whatever the default happens to be.
    pub fn register(&self, component: &str, config: HeartbeatConfig) {
        let now = Utc::now();
        self.components.insert(
            component.to_string(),
            ComponentState {
                last_beat: now,
                config,
            },
        );
        metrics::HEARTBEAT_LAST_BEAT_TIMESTAMP
            .with_label_values(&[component])
            .set(now.timestamp() as f64);
    }

    /// Record a liveness beat for `component` at the current time.
    /// Auto-registers with `HeartbeatConfig::default()` if this is the
    /// first beat seen for this component name.
    pub fn beat(&self, component: &str) {
        let now = Utc::now();
        self.components
            .entry(component.to_string())
            .and_modify(|s| s.last_beat = now)
            .or_insert_with(|| ComponentState {
                last_beat: now,
                config: HeartbeatConfig::default(),
            });

        metrics::HEARTBEAT_LAST_BEAT_TIMESTAMP
            .with_label_values(&[component])
            .set(now.timestamp() as f64);
    }

    /// True if `component` has gone silent longer than its configured
    /// tolerance, or was never registered/beaten at all (treated as stale
    /// — an unknown component should never read as "healthy" by default,
    /// unlike CircuitBreakerRegistry/KillSwitchRegistry's unknown-scope
    /// handling, since "never started" is exactly the failure mode this
    /// module exists to catch).
    pub fn is_stale(&self, component: &str) -> bool {
        match self.components.get(component) {
            Some(state) => {
                let silence = Utc::now() - state.last_beat;
                let max_silence = chrono::Duration::from_std(state.config.max_silence)
                    .unwrap_or(chrono::Duration::MAX);
                silence > max_silence
            }
            None => true,
        }
    }

    /// Full status for a single component, or `None` if it was never
    /// registered/beaten.
    pub fn status(&self, component: &str) -> Option<ComponentStatus> {
        self.components.get(component).map(|state| {
            let now = Utc::now();
            let silence = (now - state.last_beat).to_std().unwrap_or(Duration::ZERO);
            ComponentStatus {
                component: component.to_string(),
                last_beat: state.last_beat,
                silence,
                max_silence: state.config.max_silence,
                stale: self.is_stale(component),
            }
        })
    }

    /// Snapshot of every registered component's status, for a
    /// control-plane dashboard or a `/healthz`-style endpoint.
    pub fn all_statuses(&self) -> Vec<ComponentStatus> {
        self.components
            .iter()
            .map(|entry| {
                let component = entry.key().clone();
                let state = entry.value();
                let now = Utc::now();
                let silence = (now - state.last_beat).to_std().unwrap_or(Duration::ZERO);
                ComponentStatus {
                    component: component.clone(),
                    last_beat: state.last_beat,
                    silence,
                    max_silence: state.config.max_silence,
                    stale: self.is_stale(&component),
                }
            })
            .collect()
    }

    /// True only if every registered component is currently non-stale.
    /// Convenient single boolean for a `/healthz` HTTP handler; use
    /// `all_statuses()` instead when you need to know *which* component
    /// is the problem.
    pub fn all_healthy(&self) -> bool {
        self.components.iter().all(|entry| {
            let component = entry.key();
            !self.is_stale(component)
        })
    }
}

impl Default for HeartbeatRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn unregistered_component_is_stale() {
        let reg = HeartbeatRegistry::new();
        assert!(reg.is_stale("NEVER_SEEN"));
        assert!(reg.status("NEVER_SEEN").is_none());
    }

    #[test]
    fn freshly_beaten_component_is_not_stale() {
        let reg = HeartbeatRegistry::new();
        reg.beat("detector-arbitrum");
        assert!(!reg.is_stale("detector-arbitrum"));
    }

    #[test]
    fn component_goes_stale_after_max_silence() {
        let reg = HeartbeatRegistry::new();
        reg.register(
            "fast-loop",
            HeartbeatConfig {
                max_silence: Duration::from_millis(50),
            },
        );
        assert!(!reg.is_stale("fast-loop"));
        sleep(Duration::from_millis(120));
        assert!(reg.is_stale("fast-loop"));
    }

    #[test]
    fn beat_resets_staleness() {
        let reg = HeartbeatRegistry::new();
        reg.register(
            "fast-loop",
            HeartbeatConfig {
                max_silence: Duration::from_millis(50),
            },
        );
        sleep(Duration::from_millis(120));
        assert!(reg.is_stale("fast-loop"));
        reg.beat("fast-loop");
        assert!(!reg.is_stale("fast-loop"));
    }

    #[test]
    fn auto_registers_with_default_tolerance_on_first_beat() {
        let reg = HeartbeatRegistry::new();
        reg.beat("never-explicitly-registered");
        let status = reg.status("never-explicitly-registered").unwrap();
        assert_eq!(status.max_silence, HeartbeatConfig::default().max_silence);
        assert!(!status.stale);
    }

    #[test]
    fn components_are_independent() {
        let reg = HeartbeatRegistry::new();
        reg.register(
            "slow-job",
            HeartbeatConfig {
                max_silence: Duration::from_secs(3600),
            },
        );
        reg.register(
            "fast-loop",
            HeartbeatConfig {
                max_silence: Duration::from_millis(50),
            },
        );
        sleep(Duration::from_millis(120));
        assert!(
            !reg.is_stale("slow-job"),
            "slow job with 1h tolerance should still be fresh"
        );
        assert!(
            reg.is_stale("fast-loop"),
            "fast loop with 50ms tolerance should be stale"
        );
    }

    #[test]
    fn all_healthy_false_if_any_component_stale() {
        let reg = HeartbeatRegistry::new();
        reg.register(
            "ok-job",
            HeartbeatConfig {
                max_silence: Duration::from_secs(3600),
            },
        );
        reg.register(
            "dead-job",
            HeartbeatConfig {
                max_silence: Duration::from_millis(50),
            },
        );
        sleep(Duration::from_millis(120));
        assert!(!reg.all_healthy());
    }

    #[test]
    fn all_healthy_true_when_nothing_stale() {
        let reg = HeartbeatRegistry::new();
        reg.register(
            "ok-job",
            HeartbeatConfig {
                max_silence: Duration::from_secs(3600),
            },
        );
        reg.beat("ok-job");
        assert!(reg.all_healthy());
    }

    #[test]
    fn all_healthy_true_with_no_components_registered() {
        // Vacuously true — an empty registry has no failing components.
        // Callers wiring this into a /healthz endpoint at startup, before
        // any component has beaten yet, should be aware of this: register
        // expected components eagerly at startup rather than relying on
        // lazy auto-registration if you want /healthz to reflect "not yet
        // started" as unhealthy during the startup window.
        let reg = HeartbeatRegistry::new();
        assert!(reg.all_healthy());
    }

    #[test]
    fn all_statuses_reports_every_registered_component() {
        let reg = HeartbeatRegistry::new();
        reg.beat("a");
        reg.beat("b");
        reg.beat("c");
        let statuses = reg.all_statuses();
        assert_eq!(statuses.len(), 3);
        assert!(statuses.iter().all(|s| !s.stale));
    }
}
