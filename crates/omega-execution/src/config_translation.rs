// crates/omega-execution/src/config_translation.rs
//
// Gap 4 (production-integration-plan.md, "Config Translation Layer"):
// translates `omega_core::config::RelayConfig` (what `OmegaConfig`
// actually holds, matching `config/default.toml`'s `[relay]` section)
// into `omega_relay::RelayConfig` (what `MultiRelayClient::new` actually
// consumes). These are two different types with almost no field overlap
// — see the plan document for the full finding.
//
// ## Fields that map directly
//
//   omega_core::config::RelayConfig::max_bundles_per_relay_per_second
//     -> omega_relay::RelayConfig::max_bundles_per_relay_per_second
//   omega_core::config::RelayConfig::cascade_stagger_ms
//     -> omega_relay::RelayConfig::stagger_ms
//
// NOTE (this revision): `omega_core::config::RelayConfig::
// max_bundles_per_relay_per_second` is `usize`. This function assigns it
// directly into `omega_relay::RelayConfig::max_bundles_per_relay_per_second`
// with no cast, exactly as the pre-this-revision version of this file
// already did — that assignment compiled before this revision's changes
// and nothing here touches that field's type on either side, so it is
// assumed to still be correct. Flagged explicitly rather than silently
// carried forward: if `omega_relay::RelayConfig`'s own field type ever
// differs from `usize`, this is the exact line that would need a cast,
// and it wasn't independently re-verified against `omega_relay::
// RelayConfig`'s real definition in this revision (not pasted into this
// investigation).
//
// ## Fields with NO source anywhere in OmegaConfig — RESOLVED (this revision)
//
// Previously `RelayBootstrapInputs` required `phase_1_relays`,
// `phase_2plus_relays`, and `blind_fallback` to be supplied entirely by
// the caller, because `omega_core::config::RelayConfig` had no relay-name
// field at all — confirmed at the time by grepping that type's real
// field list. This revision assumes `omega_core::config::RelayConfig`
// has gained `phase_1_relays: Vec<String>`, `phase_2plus_relays:
// Vec<String>`, and `blind_fallback: bool` (see the companion change to
// that file). `RelayBootstrapInputs` below is updated accordingly: the
// two relay-name lists and the fallback flag are no longer caller-
// supplied — they're derived from `core_cfg` itself via
// `parse_relay_names`. Only `confirmation_rpc_url` remains a required
// caller input, since it is NOT a relay endpoint (per that field's own
// doc comment in omega-relay: "a regular node") and has no home in
// `omega_core::config::RelayConfig` — `main.rs` already reads
// `ARBITRUM_HTTP_RPC_URL` for exactly this purpose at startup; pass that
// same value through here rather than inventing a second, independent
// RPC URL config entry.
//
// ## String -> RelayName parsing
//
// `omega_core::config::RelayConfig`'s new `phase_1_relays`/
// `phase_2plus_relays` fields are `Vec<String>`, not `Vec<RelayName>` —
// deliberately: `omega-core` is the foundational crate nearly everything
// else depends on, and must not gain a dependency on `omega-relay` (a
// much higher-level crate) just to hold a relay-name list. The
// String -> RelayName conversion happens here, at the translation
// boundary, via `parse_relay_names` — matching the same four relay names
// (case-insensitive) `main.rs`'s own relay-construction loop has a
// verified auth convention for. An unrecognized name becomes
// `RelayName::Other(raw)`, NOT dropped — `main.rs`'s existing
// `RelayName::Other(raw) => { error!(...); continue }` arm already
// handles rejecting it explicitly and loudly; silently dropping it here
// instead would hide a likely typo or unsupported-relay configuration
// from ever surfacing.
//
// ## Fields with no destination at all
//
// `cascade_max_relays` and `inclusion_rate_tie_band_fraction` exist on
// the `omega_core` side and have NO counterpart anywhere in
// `omega_relay::RelayConfig`. Specifically:
//   - Relay count is implicitly the length of the `relay_clients` map
//     passed to `MultiRelayClient::new` — there's nothing to configure.
//   - `omega_relay::LA_TIE_BAND_FRACTION` is a hardcoded `f64 = 0.05`
//     constant, not read from any runtime config. If an operator
//     configures `inclusion_rate_tie_band_fraction` to something other
//     than 0.05 in `default.toml`, that intent is currently silently
//     overridden by omega-relay's own code — this function does not
//     paper over that; it surfaces it via `unmapped_fields` so a caller
//     can log or alert on it instead.

use omega_relay::{RelayConfig, RelayName};

/// Parses relay-name strings from `omega_core::config::RelayConfig`
/// (`phase_1_relays`/`phase_2plus_relays`) into `omega_relay::RelayName`.
/// Matches the same four names `main.rs`'s own relay-construction loop
/// has a verified auth convention for (case-insensitive, since operators
/// may write "Flashbots" or "flashbots" in TOML). Anything else becomes
/// `RelayName::Other(raw)` — NOT dropped silently — so a caller still
/// sees it and can reject it explicitly, rather than this function
/// quietly discarding a typo'd or unsupported relay name.
pub fn parse_relay_names(names: &[String]) -> Vec<RelayName> {
    names
        .iter()
        .map(|n| match n.to_lowercase().as_str() {
            "flashbots" => RelayName::Flashbots,
            "titan" => RelayName::Titan,
            "bloxroute" => RelayName::Bloxroute,
            "eden" => RelayName::Eden,
            _ => RelayName::Other(n.clone()),
        })
        .collect()
}

/// Fields `omega_core::config::RelayConfig` cannot supply. See this
/// module's doc comment for why `confirmation_rpc_url` specifically is
/// still a required caller input even after `phase_1_relays`/
/// `phase_2plus_relays`/`blind_fallback` moved onto `core_cfg` itself.
pub struct RelayBootstrapInputs {
    /// Pass `ARBITRUM_HTTP_RPC_URL` (already read at startup in
    /// `main.rs`) through here — see this module's doc comment.
    pub confirmation_rpc_url: String,
}

/// A field present in `omega_core::config::RelayConfig` with no
/// counterpart anywhere in `omega_relay::RelayConfig`. Not an error —
/// the translation still succeeds — but a caller should not silently
/// ignore a non-empty list of these.
#[derive(Debug, Clone, PartialEq)]
pub struct UnmappedRelayConfigField {
    pub field_name: &'static str,
    pub configured_value: String,
}

pub struct TranslatedRelayConfig {
    pub config: RelayConfig,
    pub unmapped_fields: Vec<UnmappedRelayConfigField>,
}

pub fn translate_relay_config(
    core_cfg: &omega_core::config::RelayConfig,
    inputs: RelayBootstrapInputs,
) -> TranslatedRelayConfig {
    let config = RelayConfig {
        phase_1_relays: parse_relay_names(&core_cfg.phase_1_relays),
        phase_2plus_relays: parse_relay_names(&core_cfg.phase_2plus_relays),
        blind_fallback: core_cfg.blind_fallback,
        max_bundles_per_relay_per_second: core_cfg.max_bundles_per_relay_per_second,
        stagger_ms: core_cfg.cascade_stagger_ms,
        confirmation_rpc_url: inputs.confirmation_rpc_url,
    };

    let mut unmapped_fields = Vec::new();
    unmapped_fields.push(UnmappedRelayConfigField {
        field_name: "cascade_max_relays",
        configured_value: core_cfg.cascade_max_relays.to_string(),
    });
    if (core_cfg.inclusion_rate_tie_band_fraction - omega_relay::LA_TIE_BAND_FRACTION).abs()
        > f64::EPSILON
    {
        unmapped_fields.push(UnmappedRelayConfigField {
            field_name: "inclusion_rate_tie_band_fraction",
            configured_value: core_cfg.inclusion_rate_tie_band_fraction.to_string(),
        });
    }

    TranslatedRelayConfig {
        config,
        unmapped_fields,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_core_cfg() -> omega_core::config::RelayConfig {
        omega_core::config::RelayConfig::default()
    }

    fn sample_inputs() -> RelayBootstrapInputs {
        RelayBootstrapInputs {
            confirmation_rpc_url: "http://localhost:8545".to_string(),
        }
    }

    #[test]
    fn direct_fields_map_correctly() {
        let core = sample_core_cfg();
        let result = translate_relay_config(&core, sample_inputs());
        assert_eq!(
            result.config.max_bundles_per_relay_per_second,
            core.max_bundles_per_relay_per_second
        );
        assert_eq!(result.config.stagger_ms, core.cascade_stagger_ms);
    }

    #[test]
    fn caller_supplied_confirmation_rpc_url_passes_through_unchanged() {
        let result = translate_relay_config(&sample_core_cfg(), sample_inputs());
        assert_eq!(result.config.confirmation_rpc_url, "http://localhost:8545");
    }

    #[test]
    fn phase_relay_lists_are_derived_from_core_cfg_not_caller_supplied() {
        // This is the resolved half of the prior gap: phase_1_relays/
        // phase_2plus_relays now come from core_cfg itself (via
        // parse_relay_names), not from a caller-supplied
        // RelayBootstrapInputs field — RelayBootstrapInputs no longer even
        // has those fields (compile-time proof: sample_inputs() above
        // doesn't set them).
        let mut core = sample_core_cfg();
        core.phase_1_relays = vec!["flashbots".to_string()];
        core.phase_2plus_relays = vec!["flashbots".to_string(), "bloxroute".to_string()];
        let result = translate_relay_config(&core, sample_inputs());
        assert_eq!(result.config.phase_1_relays, vec![RelayName::Flashbots]);
        assert_eq!(
            result.config.phase_2plus_relays,
            vec![RelayName::Flashbots, RelayName::Bloxroute]
        );
    }

    #[test]
    fn blind_fallback_is_derived_from_core_cfg() {
        let mut core = sample_core_cfg();
        core.blind_fallback = true;
        let result = translate_relay_config(&core, sample_inputs());
        assert!(result.config.blind_fallback);
    }

    #[test]
    fn default_core_cfg_produces_all_four_relays_for_both_phases() {
        // Confirms the full chain end-to-end from the real
        // omega_core::config::RelayConfig::default() through this
        // translator, matching the backward-compatibility guarantee
        // documented on that type's own defaults::relay_phase_1_relays().
        let result = translate_relay_config(&sample_core_cfg(), sample_inputs());
        assert_eq!(result.config.phase_1_relays.len(), 4);
        assert_eq!(result.config.phase_2plus_relays.len(), 4);
        assert!(result.config.phase_1_relays.contains(&RelayName::Flashbots));
        assert!(result.config.phase_1_relays.contains(&RelayName::Titan));
        assert!(result.config.phase_1_relays.contains(&RelayName::Bloxroute));
        assert!(result.config.phase_1_relays.contains(&RelayName::Eden));
    }

    #[test]
    fn cascade_max_relays_is_always_reported_unmapped() {
        let result = translate_relay_config(&sample_core_cfg(), sample_inputs());
        assert!(result
            .unmapped_fields
            .iter()
            .any(|f| f.field_name == "cascade_max_relays"));
    }

    #[test]
    fn matching_tie_band_fraction_is_not_reported_unmapped() {
        let mut core = sample_core_cfg();
        core.inclusion_rate_tie_band_fraction = omega_relay::LA_TIE_BAND_FRACTION;
        let result = translate_relay_config(&core, sample_inputs());
        assert!(!result
            .unmapped_fields
            .iter()
            .any(|f| f.field_name == "inclusion_rate_tie_band_fraction"));
    }

    #[test]
    fn diverging_tie_band_fraction_is_reported_unmapped() {
        let mut core = sample_core_cfg();
        core.inclusion_rate_tie_band_fraction = 0.10; // differs from the 0.05 constant
        let result = translate_relay_config(&core, sample_inputs());
        assert!(result
            .unmapped_fields
            .iter()
            .any(|f| f.field_name == "inclusion_rate_tie_band_fraction"));
    }

    // ── String -> RelayName parsing ──────────────────────────────────────

    #[test]
    fn known_names_map_correctly_case_insensitive() {
        let names = vec![
            "flashbots".to_string(),
            "TITAN".to_string(),
            "BloXroute".to_string(),
            "eden".to_string(),
        ];
        let parsed = parse_relay_names(&names);
        assert_eq!(
            parsed,
            vec![
                RelayName::Flashbots,
                RelayName::Titan,
                RelayName::Bloxroute,
                RelayName::Eden,
            ]
        );
    }

    #[test]
    fn unknown_name_becomes_other_not_dropped() {
        let names = vec!["some_new_relay".to_string()];
        let parsed = parse_relay_names(&names);
        assert_eq!(
            parsed,
            vec![RelayName::Other("some_new_relay".to_string())]
        );
        assert_eq!(
            parsed.len(),
            1,
            "unknown names must be surfaced, not silently dropped"
        );
    }

    #[test]
    fn empty_list_parses_to_empty_list() {
        let parsed = parse_relay_names(&[]);
        assert!(parsed.is_empty());
    }
}