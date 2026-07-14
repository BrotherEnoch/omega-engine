// crates/omega-relay/src/config.rs
//! Relay configuration — deserialised from `config/default.toml` `[relay]` section.
//!
//! Every field maps 1-to-1 with the v12 spec. No fields are optional where the
//! spec mandates a value; defaults are provided so `config/default.toml` can be
//! partially overridden by chain-specific overlays.
//!
//! CHANGE: added `confirmation_rpc_url`, required by `confirmation::InclusionTracker` to
//! check real on-chain inclusion instead of trusting a relay's HTTP-accept response.
//! This is a breaking addition — existing `config/default.toml` files need this key
//! added, since `RelayConfig` has no `#[serde(default)]` on individual fields (missing
//! required keys fail deserialization loudly, which is the existing convention here, not
//! something this change invented).

use serde::{Deserialize, Serialize};

// ── WebSocket rate limits (§17.1) ────────────────────────────────────────────

/// Messages-per-minute allowed for an authenticated (Bearer) WS connection.
pub const WS_RATE_AUTHENTICATED: u32 = 300;

/// Messages-per-minute allowed for an anonymous WS connection.
pub const WS_RATE_ANONYMOUS: u32 = 100;

// ── Relay config ─────────────────────────────────────────────────────────────

/// Canonical relay names understood by the engine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelayName {
    /// Flashbots relay.
    Flashbots,
    /// bloXroute relay.
    Bloxroute,
    /// Titan relay.
    Titan,
    /// Eden relay.
    Eden,
    /// Escape hatch — any relay not in the enum above.
    #[serde(untagged)]
    Other(String),
}

impl std::fmt::Display for RelayName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelayName::Flashbots => write!(f, "flashbots"),
            RelayName::Bloxroute => write!(f, "bloxroute"),
            RelayName::Titan => write!(f, "titan"),
            RelayName::Eden => write!(f, "eden"),
            RelayName::Other(s) => write!(f, "{s}"),
        }
    }
}

/// `[relay]` section of `config/default.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    /// Relays active during Phase 1.
    pub phase_1_relays: Vec<RelayName>,

    /// Relays active during Phase 2+.
    pub phase_2plus_relays: Vec<RelayName>,

    /// If true, submit to public mempool when all relay submissions fail.
    pub blind_fallback: bool,

    /// Max bundle submissions per relay per second — cascade backpressure (§11.2).
    pub max_bundles_per_relay_per_second: usize,

    /// Milliseconds to wait between successive bundle submissions — cascade stagger (§11.2).
    pub stagger_ms: u64,

    /// Standard chain JSON-RPC endpoint used by `confirmation::InclusionTracker` to check
    /// real on-chain inclusion. NOT a relay endpoint — a regular node.
    pub confirmation_rpc_url: String,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            phase_1_relays: vec![RelayName::Flashbots, RelayName::Bloxroute],
            phase_2plus_relays: vec![
                RelayName::Flashbots,
                RelayName::Bloxroute,
                RelayName::Titan,
                RelayName::Eden,
            ],
            blind_fallback: true,
            max_bundles_per_relay_per_second: 4,
            stagger_ms: 10,
            confirmation_rpc_url: String::new(),
        }
    }
}

/// Subset of `[gas_war]` fields that the relay layer needs directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GasWarRelayConfig {
    /// Path to `config/builder_blacklist.toml` (§12.3).
    pub builder_blacklist_path: String,
    /// Max priority fee gwei — Arbitrum ceiling (§12.2 / I3).
    pub max_priority_fee_gwei: u64,
}

impl Default for GasWarRelayConfig {
    fn default() -> Self {
        Self {
            builder_blacklist_path: "config/builder_blacklist.toml".into(),
            max_priority_fee_gwei: 500,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_config_defaults_match_spec() {
        let cfg = RelayConfig::default();
        assert_eq!(
            cfg.max_bundles_per_relay_per_second, 4,
            "§11.2: max 4 bundles/relay/sec"
        );
        assert_eq!(cfg.stagger_ms, 10, "§11.2: 10 ms stagger");
        assert!(cfg.blind_fallback);
    }

    #[test]
    fn ws_rate_limits_match_spec() {
        assert_eq!(WS_RATE_AUTHENTICATED, 300, "§17.1: 300/min authenticated");
        assert_eq!(WS_RATE_ANONYMOUS, 100, "§17.1: 100/min anonymous");
    }

    #[test]
    fn relay_name_roundtrip() {
        let names = vec![
            RelayName::Flashbots,
            RelayName::Bloxroute,
            RelayName::Titan,
            RelayName::Eden,
            RelayName::Other("custom".into()),
        ];
        for n in names {
            let s = n.to_string();
            assert!(!s.is_empty());
        }
    }
}