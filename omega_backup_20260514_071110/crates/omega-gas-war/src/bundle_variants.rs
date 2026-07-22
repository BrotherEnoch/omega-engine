ï»¿// crates/omega-gas-war/src/bundle_variants.rs
//
// 3-bundle fee variant strategy (spec Â§12, Â§12.1).
//
// Every LA blueprint is submitted as up to three fee variants to maximise
// inclusion probability while bounding cost:
//
//   Conservative  0.70 Ã— cap  â€” wins when competition is moderate
//   Aggressive    1.00 Ã— cap  â€” wins most competitive scenarios
//   Emergency     2.00 Ã— cap  â€” only emitted when profitable (fix M2)
//
// ## v12 M2 fix: emergency bundle profit check
//
//   The emergency bundle at 2Ã— cap is ONLY submitted when:
//     expected_profit_net > emergency_gas_cost_wei + dynamic_min_profit
//
//   The original v11 code submitted the emergency bundle unconditionally,
//   risking a net loss when the liquidation bonus barely exceeded fees.
//
// ## Gas cost calculation (Â§7 dual-component model)
//
//   Total bundle gas cost (wei) =
//     (l2_exec_gas Ã— l2_fee_gwei Ã— 1e9)     [L2 execution cost]
//   + (l1_data_gas Ã— l1_data_fee_gwei Ã— 1e9) [L1 data cost]
//
//   Priority fee does NOT enter the cost calculation â€” it is an
//   additional sequencer tip paid on top, expressed in gwei per gas unit.
//   The emergency bundle profit check must use the TOTAL cost at the
//   emergency fee level, not just the priority fee difference.
//
// ## Why 3 bundles Ã— up to 4 relays = 12 submissions (Â§11.2)
//
//   3 variants Ã— 4 relays = 12 relay submissions maximum.  The spec caps
//   cascade mode at 4 relays (config relay.cascade_max_relays) and
//   limits submissions to 4 per relay per second (config
//   relay.max_bundles_per_relay_per_second).  The 12-submission bound
//   is the anti-fingerprint ceiling â€” see Â§11.2 fix I2.

use alloy_primitives::U256;

use omega_core::GasConfig;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// BundleVariants
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Fee variants for a single LA blueprint (spec Â§12).
///
/// `emergency_fee` is `None` when the emergency bundle would be
/// unprofitable at 2Ã— cap (fix M2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleVariants {
    /// 0.70 Ã— cap â€” conservative fee in gwei.
    pub conservative_fee_gwei: u64,
    /// 1.00 Ã— cap â€” aggressive fee in gwei.
    pub aggressive_fee_gwei:   u64,
    /// 2.00 Ã— cap â€” emergency fee in gwei, or `None` if unprofitable.
    pub emergency_fee_gwei:    Option<u64>,
}

impl BundleVariants {
    /// Returns the list of fee tiers to submit, in ascending order.
    ///
    /// The relay submission loop iterates this slice to build one bundle
    /// per tier before applying the cascade backpressure stagger (Â§11.2).
    pub fn fee_tiers(&self) -> Vec<u64> {
        let mut tiers = vec![self.conservative_fee_gwei, self.aggressive_fee_gwei];
        if let Some(e) = self.emergency_fee_gwei {
            tiers.push(e);
        }
        tiers
    }

    /// Number of bundle variants that will be submitted.
    pub fn count(&self) -> usize {
        if self.emergency_fee_gwei.is_some() { 3 } else { 2 }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// EmergencySkipReason
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Why the emergency bundle was not included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmergencySkipReason {
    /// Emergency bundle disabled in config (Â§12.1).
    DisabledInConfig,
    /// Profit at 2Ã— fee would be below `dynamic_min_profit` (fix M2).
    InsufficientProfit,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// compute_variants
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Compute the three fee variants for a blueprint.
///
/// ## Arguments
///
/// - `cap_gwei`: adaptive cap from `adaptive_gas_cap_gwei` (spec Â§12.2).
/// - `expected_profit_net`: blueprint's net profit estimate in wei (U256).
/// - `dynamic_min_profit`: minimum acceptable profit in wei (U256).
/// - `l2_exec_gas`: buffered L2 execution gas units (from
///   `blueprint.total_l2_gas_budget()`).
/// - `l1_data_gas`: buffered L1 data gas units.
/// - `l2_base_fee_gwei`: current L2 base fee in gwei.
/// - `l1_data_fee_gwei`: current L1 data fee in gwei.
/// - `config`: `GasConfig` for `emergency_bundle_enabled` and
///   `conservative_fee_fraction`.
///
/// ## Returns
///
/// `(BundleVariants, Option<EmergencySkipReason>)` â€” the variants and,
/// when the emergency bundle is absent, why it was omitted.
pub fn compute_variants(
    cap_gwei:             u64,
    expected_profit_net:  U256,
    dynamic_min_profit:   U256,
    l2_exec_gas:          u64,
    l1_data_gas:          u64,
    l2_base_fee_gwei:     u64,
    l1_data_fee_gwei:     u64,
    config:               &GasConfig,
) -> (BundleVariants, Option<EmergencySkipReason>) {
    // â”€â”€ Conservative and aggressive fees â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    let conservative_fee = apply_fraction(cap_gwei, config.conservative_fee_fraction);
    let aggressive_fee   = cap_gwei;
    let emergency_fee    = cap_gwei.saturating_mul(2).min(500);

    // â”€â”€ Emergency bundle profit check (fix M2) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    let (emergency_fee_opt, skip_reason) = if !config.emergency_bundle_enabled {
        (None, Some(EmergencySkipReason::DisabledInConfig))
    } else {
        // Compute total gas cost at emergency priority fee (Â§7).
        //
        // Cost breakdown:
        //   L2 execution cost = (l2_base_fee_gwei + emergency_priority_fee_gwei)
        //                       Ã— l2_exec_gas
        //   L1 data cost      = l1_data_fee_gwei Ã— l1_data_gas
        //
        // Both multiplied by 1e9 to convert gwei â†’ wei.
        //
        // Using U256 throughout to prevent u128 overflow for large positions.
        let l2_fee_total_gwei = l2_base_fee_gwei.saturating_add(emergency_fee);
        let l2_cost_wei = U256::from(l2_fee_total_gwei)
            .saturating_mul(U256::from(l2_exec_gas))
            .saturating_mul(U256::from(1_000_000_000_u64));

        let l1_cost_wei = U256::from(l1_data_fee_gwei)
            .saturating_mul(U256::from(l1_data_gas))
            .saturating_mul(U256::from(1_000_000_000_u64));

        let total_cost_wei = l2_cost_wei.saturating_add(l1_cost_wei);

        // Profit check: expected_profit_net > total_cost_wei + dynamic_min_profit
        let required = total_cost_wei.saturating_add(dynamic_min_profit);

        if expected_profit_net > required {
            (Some(emergency_fee), None)
        } else {
            tracing::debug!(
                cap_gwei,
                emergency_fee_gwei    = emergency_fee,
                expected_profit_net   = %expected_profit_net,
                total_cost_wei        = %total_cost_wei,
                dynamic_min_profit    = %dynamic_min_profit,
                required              = %required,
                "Emergency bundle skipped: profit insufficient at 2Ã— fee (fix M2)",
            );
            (None, Some(EmergencySkipReason::InsufficientProfit))
        }
    };

    let variants = BundleVariants {
        conservative_fee_gwei: conservative_fee,
        aggressive_fee_gwei:   aggressive_fee,
        emergency_fee_gwei:    emergency_fee_opt,
    };

    tracing::debug!(
        conservative_fee_gwei = conservative_fee,
        aggressive_fee_gwei   = aggressive_fee,
        emergency_fee_gwei    = ?emergency_fee_opt,
        bundle_count          = variants.count(),
        "BundleVariants computed",
    );

    (variants, skip_reason)
}

/// Apply a fractional multiplier to a gwei cap, with floor at 1.
///
/// Ensures the conservative fee is always at least 1 gwei.
fn apply_fraction(cap_gwei: u64, fraction: f64) -> u64 {
    let result = (cap_gwei as f64 * fraction) as u64;
    result.max(1)
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> GasConfig {
        GasConfig::default()
    }

    fn profitable_profit() -> U256 {
        // Very large profit â€” emergency bundle should always be included
        U256::from(10_000_000_000_000_000_u64) // 0.01 ETH
    }

    fn zero_profit() -> U256 {
        U256::ZERO
    }

    // â”€â”€ Fee tier arithmetic â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn conservative_is_70_pct_of_cap() {
        let (v, _) = compute_variants(
            100,
            profitable_profit(),
            U256::ZERO,
            21_000, 0,
            0, 0,
            &default_config(),
        );
        assert_eq!(v.conservative_fee_gwei, 70);
        assert_eq!(v.aggressive_fee_gwei,   100);
    }

    #[test]
    fn emergency_is_2x_cap_clamped_to_500() {
        let (v, skip) = compute_variants(
            300,
            profitable_profit(),
            U256::ZERO,
            21_000, 0,
            0, 0,
            &default_config(),
        );
        assert!(skip.is_none());
        assert_eq!(v.emergency_fee_gwei, Some(500)); // 600 clamped to 500
    }

    #[test]
    fn emergency_2x_cap_below_ceiling() {
        let (v, skip) = compute_variants(
            100,
            profitable_profit(),
            U256::ZERO,
            21_000, 0,
            0, 0,
            &default_config(),
        );
        assert!(skip.is_none());
        assert_eq!(v.emergency_fee_gwei, Some(200));
    }

    // â”€â”€ M2 profit check â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn emergency_skipped_when_profit_insufficient() {
        let (v, skip) = compute_variants(
            100,
            zero_profit(),
            U256::ZERO,
            21_000, 0,
            100, 0,   // 100 gwei base fee â†’ L2 cost eats into zero profit
            &default_config(),
        );
        assert_eq!(skip, Some(EmergencySkipReason::InsufficientProfit));
        assert!(v.emergency_fee_gwei.is_none());
        assert_eq!(v.count(), 2);
    }

    #[test]
    fn emergency_skipped_when_disabled_in_config() {
        let mut cfg = default_config();
        cfg.emergency_bundle_enabled = false;

        let (v, skip) = compute_variants(
            100,
            profitable_profit(),
            U256::ZERO,
            21_000, 0,
            0, 0,
            &cfg,
        );
        assert_eq!(skip, Some(EmergencySkipReason::DisabledInConfig));
        assert!(v.emergency_fee_gwei.is_none());
    }

    // â”€â”€ Cost calculation correctness â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn l1_data_cost_included_in_profit_check() {
        // l1_data_fee = 1000 gwei, l1_data_gas = 10000
        // l1_cost = 1000 Ã— 10000 Ã— 1e9 = 1e16 wei = 0.01 ETH
        // With expected_profit = 0.005 ETH < l1_cost â†’ skip emergency
        let l1_cost = U256::from(1_000_u64)
            .saturating_mul(U256::from(10_000_u64))
            .saturating_mul(U256::from(1_000_000_000_u64)); // = 1e16

        let small_profit = l1_cost / U256::from(2); // 0.5 Ã— l1_cost < total_cost

        let (v, skip) = compute_variants(
            100,
            small_profit,
            U256::ZERO,
            21_000,
            10_000, // l1_data_gas
            0,
            1_000,  // l1_data_fee_gwei
            &default_config(),
        );
        assert_eq!(skip, Some(EmergencySkipReason::InsufficientProfit),
            "L1 data cost must be included in emergency profit check");
        assert!(v.emergency_fee_gwei.is_none());
    }

    // â”€â”€ fee_tiers helper â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn fee_tiers_ascending_with_emergency() {
        let (v, _) = compute_variants(
            100,
            profitable_profit(),
            U256::ZERO,
            21_000, 0,
            0, 0,
            &default_config(),
        );
        let tiers = v.fee_tiers();
        assert_eq!(tiers.len(), 3);
        assert!(tiers[0] < tiers[1] && tiers[1] < tiers[2],
            "fee tiers must be strictly ascending: {tiers:?}");
    }

    #[test]
    fn fee_tiers_two_when_no_emergency() {
        let (v, _) = compute_variants(
            100,
            zero_profit(),
            U256::ZERO,
            21_000, 0,
            100, 0,
            &default_config(),
        );
        assert_eq!(v.fee_tiers().len(), 2);
    }

    // â”€â”€ Zero cap edge case â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn zero_cap_conservative_floor_at_1() {
        let (v, _) = compute_variants(
            0,
            profitable_profit(),
            U256::ZERO,
            21_000, 0,
            0, 0,
            &default_config(),
        );
        assert_eq!(v.conservative_fee_gwei, 1,
            "conservative fee must have floor of 1 gwei");
    }
}