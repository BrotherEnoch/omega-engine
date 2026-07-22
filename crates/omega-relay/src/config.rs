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
//!
//! ## Audit fix (this revision): RelayName's serde derive was invalid
//!
//! `RelayName::Other(String)` previously carried `#[serde(untagged)]` while the enum's
//! other variants stayed under the default external tagging (plus
//! `#[serde(rename_all = "lowercase")]`). `untagged` is a container-level attribute in
//! serde — it selects the ENTIRE enum's wire representation, not a single variant's.
//! There is no supported way to mix "external tag for these variants, untagged for that
//! one" in one derive; this was, at minimum, extremely likely a compile error, and even
//! if some serde version tolerated it, it would not produce the intended behavior
//! (arbitrary unrecognized strings falling through to `Other(String)`) — externally
//! tagged enums match on the variant's own (renamed) name as the discriminant, not on
//! "whatever didn't match." Replaced with a manual `Deserialize`/`Serialize` impl: known
//! variants match their exact lowercase name string; anything else becomes
//! `Other(<original string>)`. `Serialize` delegates to the existing `Display` impl,
//! which already produces the correct lowercase names for known variants and the raw
//! string for `Other`.
//!
//! ## Audit fix (this revision): tie-band fraction was duplicated as three magic numbers
//!
//! The 5% LA-inclusion-rate tie-band cutoff (`best_rate * 0.95`) was hardcoded
//! independently in `backpressure.rs` (`build_submission_order`,
//! `submit_single_bundle`) and `reputation.rs` (`submission_order`) — three separate
//! literals encoding the same spec constant (§11.2, §14.2), with no single source of
//! truth. Added `LA_TIE_BAND_FRACTION` here as that source; all three call sites now
//! derive their threshold from it instead of a bare `0.95`.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ── WebSocket rate limits (§17.1) ────────────────────────────────────────────

/// Messages-per-minute allowed for an authenticated (Bearer) WS connection.
pub const WS_RATE_AUTHENTICATED: u32 = 300;

/// Messages-per-minute allowed for an anonymous WS connection.
pub const WS_RATE_ANONYMOUS: u32 = 100;

// ── LA cascade tie-band (§11.2, §14.2) ───────────────────────────────────────

/// Fraction defining the LA inclusion-rate tie band: relays with a rate within
/// this fraction of the best relay's rate are treated as tied and their
/// submission order is randomised (anti-fingerprinting, fix I2). Single source
/// of truth for the threshold computed independently in `backpressure.rs` and
/// `reputation.rs` — see this file's audit note above.
pub const LA_TIE_BAND_FRACTION: f64 = 0.05;

// ── Relay config ─────────────────────────────────────────────────────────────

/// Canonical relay names understood by the engine.
///
/// Deserializes from a plain string: `"flashbots"`, `"bloxroute"`, `"titan"`,
/// `"eden"` map to their respective unit variants (case-sensitive, matching
/// exactly what the previous `rename_all = "lowercase"` attribute intended);
/// any other string is preserved verbatim in `Other`. See this file's audit
/// note for why this is a manual impl rather than a derive.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RelayName {
    /// Flashbots relay.
    Flashbots,
    /// bloXroute relay.
    Bloxroute,
    /// Titan relay.
    Titan,
    /// Eden relay.
    Eden,
    /// Escape hatch — any relay not in the enum above. Holds the original,
    /// unmodified string as received.
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

impl<'de> Deserialize<'de> for RelayName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "flashbots" => RelayName::Flashbots,
            "bloxroute" => RelayName::Bloxroute,
            "titan" => RelayName::Titan,
            "eden" => RelayName::Eden,
            _ => RelayName::Other(s),
        })
    }
}

impl Serialize for RelayName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
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

    // ── Audit fix regression tests (this revision) ────────────────────────────

    #[test]
    fn relay_name_deserializes_known_variants_from_plain_strings() {
        let f: RelayName = serde_json::from_str("\"flashbots\"").unwrap();
        assert_eq!(f, RelayName::Flashbots);
        let b: RelayName = serde_json::from_str("\"bloxroute\"").unwrap();
        assert_eq!(b, RelayName::Bloxroute);
        let t: RelayName = serde_json::from_str("\"titan\"").unwrap();
        assert_eq!(t, RelayName::Titan);
        let e: RelayName = serde_json::from_str("\"eden\"").unwrap();
        assert_eq!(e, RelayName::Eden);
    }

    #[test]
    fn relay_name_deserializes_unknown_string_into_other() {
        let o: RelayName = serde_json::from_str("\"some_custom_relay\"").unwrap();
        assert_eq!(o, RelayName::Other("some_custom_relay".to_string()));
    }

    #[test]
    fn relay_name_serializes_as_plain_string() {
        assert_eq!(serde_json::to_string(&RelayName::Flashbots).unwrap(), "\"flashbots\"");
        assert_eq!(
            serde_json::to_string(&RelayName::Other("xyz".into())).unwrap(),
            "\"xyz\""
        );
    }

    #[test]
    fn relay_name_serde_round_trips() {
        for n in [
            RelayName::Flashbots,
            RelayName::Bloxroute,
            RelayName::Titan,
            RelayName::Eden,
            RelayName::Other("custom_relay".into()),
        ] {
            let json = serde_json::to_string(&n).unwrap();
            let back: RelayName = serde_json::from_str(&json).unwrap();
            assert_eq!(n, back, "round trip must preserve identity for {json}");
        }
    }

    #[test]
    fn relay_config_toml_with_relay_names_deserializes() {
        // Confirms RelayName actually works as a plain TOML string value inside
        // a Vec, the real-world shape `config/default.toml` uses — this is
        // exactly the scenario the invalid #[serde(untagged)] would have broken.
        let toml_str = r#"
            phase_1_relays = ["flashbots", "bloxroute"]
            phase_2plus_relays = ["flashbots", "bloxroute", "titan", "eden"]
            blind_fallback = true
            max_bundles_per_relay_per_second = 4
            stagger_ms = 10
            confirmation_rpc_url = "http://localhost:8545"
        "#;
        let cfg: RelayConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.phase_1_relays, vec![RelayName::Flashbots, RelayName::Bloxroute]);
    }

    #[test]
    fn la_tie_band_fraction_matches_spec() {
        assert!(
            (LA_TIE_BAND_FRACTION - 0.05).abs() < 1e-9,
            "§11.2/§14.2: 5% tie band"
        );
    }
}