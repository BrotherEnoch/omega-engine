// crates/omega-execution/src/relay_factory.rs
//
// RelayClientFactory — Gap 2 in ExecutionPipelineSpecification's
// production-integration-plan.md ("Relay Bootstrap").
//
// See that document for the full finding: `HttpRelayClient::new` is
// fully implemented in `omega-relay/src/client.rs` and has ZERO
// production call sites anywhere in the workspace, confirmed by
// exhaustive grep across every `.rs` file including the backup copy.
// Every `HashMap<String, Arc<dyn RelayClient>>` construction site found
// is test or bench code building `MockRelayClient`s.
//
// This trait exists for exactly the same reason `TransactionSigner`
// does (see signer.rs): it lets the rest of this crate depend on "a way
// to obtain real relay clients" as a typed contract, without this crate
// fabricating real endpoint URLs or `RelayAuth` credentials that don't
// exist in anything read during this investigation. A production
// implementation needs, at minimum:
//   - Real relay endpoint URLs per `RelayName` (Gap 3 — deployment
//     secrets, not yet decided where they live).
//   - Real `RelayAuth` credentials per relay (same source).
//   - A translation from `omega_core::config::RelayConfig` (what
//     `OmegaConfig` actually holds) to `omega_relay::config::RelayConfig`
//     (what `MultiRelayClient::new` actually consumes) — Gap 4, since
//     those are two different types with almost no field overlap and no
//     conversion code exists between them anywhere in the workspace.
//
// This file implements neither. It defines the contract only.

use std::collections::HashMap;
use std::sync::Arc;

use omega_relay::{RelayClient, RelayConfig};

/// Builds the map of relay name → live `RelayClient` that
/// `MultiRelayClient::new` requires, from a `RelayConfig`.
///
/// Deliberately returns `anyhow::Result` rather than `ExecutionError`:
/// factory construction is a startup-time concern (build the relay map
/// once, before any blueprint exists), not a per-blueprint pipeline
/// failure — conflating the two error types would blur that distinction.
pub trait RelayClientFactory: Send + Sync {
    fn build(&self, config: &RelayConfig) -> anyhow::Result<HashMap<String, Arc<dyn RelayClient>>>;
}

/// Always-fails factory — the honest default when no real
/// `RelayClientFactory` has been wired in. Returns a descriptive error
/// naming exactly which gaps (3 and 4, per the production integration
/// plan) block a real implementation, rather than silently returning an
/// empty map (which would let `MultiRelayClient` construct successfully
/// with zero relays — a worse failure mode, since it would look like
/// working configuration instead of an obvious startup error).
pub struct UnconfiguredRelayClientFactory;

impl RelayClientFactory for UnconfiguredRelayClientFactory {
    fn build(&self, _config: &RelayConfig) -> anyhow::Result<HashMap<String, Arc<dyn RelayClient>>> {
        anyhow::bail!(
            "no RelayClientFactory configured — HttpRelayClient::new has zero production \
             call sites anywhere in the omega-engine workspace as of this writing. Building \
             real relay clients requires: (1) a secrets source for endpoint URLs and RelayAuth \
             credentials (production-integration-plan.md Gap 3, not yet decided), and (2) a \
             translation from omega_core::config::RelayConfig to omega_relay::config::RelayConfig \
             (Gap 4, no conversion code exists yet). Do not fabricate either — wire in a real \
             RelayClientFactory implementation once both gaps are resolved."
        )
    }
}

/// Test-only fake, compiled only under `cfg(test)` — same reasoning as
/// `signer::MockTransactionSigner`: structurally impossible to link into
/// a production binary, never gated behind a feature flag a production
/// build could accidentally enable.
#[cfg(test)]
pub struct MockRelayClientFactory {
    /// The client every relay name in the config maps to. Callers
    /// construct this with whatever test double (`AlwaysAcceptRelay`,
    /// `CountingRelay`, `SlowRelay`, etc.) their test needs.
    pub client: Arc<dyn RelayClient>,
}

#[cfg(test)]
impl RelayClientFactory for MockRelayClientFactory {
    fn build(&self, config: &RelayConfig) -> anyhow::Result<HashMap<String, Arc<dyn RelayClient>>> {
        // Real factories would build one distinct client per relay name
        // (each with its own endpoint/auth); this mock intentionally
        // maps every configured name to the same shared test double,
        // since tests care about dispatch/aggregation logic, not
        // per-relay distinctness.
        let mut map: HashMap<String, Arc<dyn RelayClient>> = HashMap::new();
        for name in config.phase_1_relays.iter().chain(config.phase_2plus_relays.iter()) {
            map.insert(name.to_string(), Arc::clone(&self.client));
        }
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omega_relay::RelayName;

    #[test]
    fn unconfigured_factory_fails_with_named_gaps() {
        let factory = UnconfiguredRelayClientFactory;
        let cfg = RelayConfig::default();
        let err = factory.build(&cfg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Gap 3"), "error must name the secrets gap");
        assert!(msg.contains("Gap 4"), "error must name the config-translation gap");
    }

    #[test]
    fn unconfigured_factory_never_returns_an_empty_ok_map() {
        // Regression guard against the specific worse failure mode this
        // type exists to avoid: silently succeeding with zero relays.
        let factory = UnconfiguredRelayClientFactory;
        let cfg = RelayConfig::default();
        assert!(factory.build(&cfg).is_err());
    }

    #[test]
    fn mock_factory_maps_every_configured_relay_name() {
        struct Dummy;
        #[async_trait::async_trait]
        impl RelayClient for Dummy {
            async fn submit_bundle(
                &self,
                bundle: omega_relay::BundlePayload,
            ) -> omega_relay::RelayResult<omega_relay::SubmissionOutcome> {
                Ok(omega_relay::SubmissionOutcome {
                    accepted: true,
                    relay_bundle_id: Some(bundle.bundle_hash),
                })
            }
            fn name(&self) -> &str { "dummy" }
        }

        let factory = MockRelayClientFactory { client: Arc::new(Dummy) };
        let cfg = RelayConfig {
            phase_1_relays: vec![RelayName::Flashbots],
            phase_2plus_relays: vec![RelayName::Flashbots, RelayName::Bloxroute],
            ..Default::default()
        };
        let map = factory.build(&cfg).unwrap();
        assert!(map.contains_key("flashbots"));
        assert!(map.contains_key("bloxroute"));
    }
}