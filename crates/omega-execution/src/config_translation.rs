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
// ## Fields with NO source anywhere in OmegaConfig
//
// These are required parameters on `RelayBootstrapInputs` below —
// **not** given a default here, since each is a real decision this
// module has no basis to make silently:
//
//   phase_1_relays, phase_2plus_relays — which relays are active per
//     phase is an operational decision, not derivable from any field
//     read in this investigation.
//   blind_fallback — whether to fall back to the public mempool on
//     total relay failure is a risk decision.
//   confirmation_rpc_url — NOT a relay endpoint (per that field's own
//     doc comment in omega-relay: "a regular node"). `main.rs` already
//     reads `ARBITRUM_RPC_URL` for exactly this purpose at startup; pass
//     that same value through here rather than inventing a second,
//     independent RPC URL config entry.
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

/// Fields `omega_core::config::RelayConfig` cannot supply. See this
/// module's doc comment for why each is a real decision rather than a
/// derivable default.
pub struct RelayBootstrapInputs {
    pub phase_1_relays: Vec<RelayName>,
    pub phase_2plus_relays: Vec<RelayName>,
    pub blind_fallback: bool,
    /// Pass `ARBITRUM_RPC_URL` (already read at startup in `main.rs`)
    /// through here — see this module's doc comment.
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
        phase_1_relays: inputs.phase_1_relays,
        phase_2plus_relays: inputs.phase_2plus_relays,
        blind_fallback: inputs.blind_fallback,
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
            phase_1_relays: vec![RelayName::Flashbots],
            phase_2plus_relays: vec![RelayName::Flashbots, RelayName::Bloxroute],
            blind_fallback: true,
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
    fn caller_supplied_fields_pass_through_unchanged() {
        let result = translate_relay_config(&sample_core_cfg(), sample_inputs());
        assert_eq!(result.config.phase_1_relays, vec![RelayName::Flashbots]);
        assert_eq!(
            result.config.phase_2plus_relays,
            vec![RelayName::Flashbots, RelayName::Bloxroute]
        );
        assert!(result.config.blind_fallback);
        assert_eq!(result.config.confirmation_rpc_url, "http://localhost:8545");
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
}
