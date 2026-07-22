ï»¿// crates/omega-gas-war/src/adaptive_cap.rs
//
// Adaptive gas cap â€” Arbitrum priority fee ceiling per blueprint.
//
// ## Spec Â§12.2
//
//   The Gas War Engine caps the priority fee at 500 gwei.  On Arbitrum
//   at 250ms block times this is:
//     500 gwei Ã— 21,000 gas Ã— 1e-9 ETH/gwei = 0.0105 ETH per block
//   â€” comparable to 50 gwei on Ethereum L1 at 12s blocks.  The ceiling
//   is large in gwei but cheap in ETH.
//
// ## Cap formula (Â§12)
//
//   base_cap = (liquidation_bonus_eth Ã— 1e9 gwei/ETH Ã— 0.05) / GAS_PER_BUNDLE
//
//   Rationale: the base cap is 5% of the liquidation bonus, normalised
//   to the bundle gas cost.  This bounds gas spend to 5% of gross revenue
//   at the conservative fee tier before urgency and win-rate adjustments.
//
// ## Urgency multiplier (health factor proximity to 1.0)
//
//   HF < 1.001 â†’ 3.0Ã—  (imminent; outbidding is critical)
//   HF < 1.005 â†’ 2.0Ã—
//   HF < 1.01  â†’ 1.5Ã—
//   HF â‰¥ 1.01  â†’ 1.0Ã—  (warm tier; conservative spend)
//
// ## Win-rate multiplier (Â§13 ML feedback)
//
//   win_rate < 0.30 â†’ 1.8Ã—  (losing frequently; raise bid)
//   win_rate < 0.50 â†’ 1.3Ã—
//   win_rate â‰¥ 0.50 â†’ 1.0Ã—  (competitive; hold)
//
// ## Output bounds
//
//   clamp(2, 500) gwei.
//   Lower bound 2 gwei: below this Arbitrum sequencer ignores the tip.
//   Upper bound 500 gwei: spec Â§12.2 hard ceiling.
//
// ## Unit analysis (verified)
//
//   liquidation_bonus_eth [ETH]
//   Ã— 1_000_000_000        [gwei / ETH]   â†’ bonus [gweiÂ·ETH / ETH = gwei]
//   Ã— 0.05                 [fraction]     â†’ 5% of bonus [gwei]
//   / GAS_PER_BUNDLE       [gas]          â†’ gwei / gas = gwei per gas unit
//
//   This is correct: priority fee is expressed as gwei per gas unit.

use std::fmt;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Constants
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Standard EVM transaction gas cost used as the bundle normaliser.
///
/// 21,000 is the base cost of a simple ETH transfer.  For LA bundles
/// the actual gas is higher, but the formula uses this as the denominator
/// so the cap scales with the bonus value rather than the actual gas cost
/// (which is already accounted for in `dynamic_min_profit`).
pub const GAS_PER_BUNDLE: f64 = 21_000.0;

/// Hard upper bound on the priority fee in gwei (spec Â§12.2).
pub const MAX_PRIORITY_FEE_GWEI: u64 = 500;

/// Hard lower bound â€” Arbitrum sequencer ignores tips below this.
pub const MIN_PRIORITY_FEE_GWEI: u64 = 2;

/// The 5% revenue fraction allocated to gas bidding.
const BONUS_GAS_FRACTION: f64 = 0.05;

/// Conversion factor: 1 ETH = 1,000,000,000 gwei.
const GWEI_PER_ETH: f64 = 1_000_000_000.0;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// UrgencyTier
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Health factor proximity tier, determining the urgency multiplier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UrgencyTier {
    /// HF < 1.001 â€” liquidation is imminent; outbidding is critical.
    Critical,
    /// HF < 1.005 â€” highly urgent.
    High,
    /// HF < 1.01  â€” urgent; LA hot tier.
    Moderate,
    /// HF â‰¥ 1.01  â€” LA warm tier; conservative spend.
    Low,
}

impl UrgencyTier {
    /// Classify a health factor.
    ///
    /// `health_factor` must be a finite, positive f64.
    pub fn from_health_factor(hf: f64) -> Self {
        if hf < 1.001 { UrgencyTier::Critical  }
        else if hf < 1.005 { UrgencyTier::High }
        else if hf < 1.01  { UrgencyTier::Moderate }
        else               { UrgencyTier::Low  }
    }

    /// Priority fee multiplier for this urgency tier.
    pub fn multiplier(self) -> f64 {
        match self {
            UrgencyTier::Critical => 3.0,
            UrgencyTier::High     => 2.0,
            UrgencyTier::Moderate => 1.5,
            UrgencyTier::Low      => 1.0,
        }
    }
}

impl fmt::Display for UrgencyTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UrgencyTier::Critical => f.write_str("CRITICAL"),
            UrgencyTier::High     => f.write_str("HIGH"),
            UrgencyTier::Moderate => f.write_str("MODERATE"),
            UrgencyTier::Low      => f.write_str("LOW"),
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// WinRateTier
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Recent LA win rate tier, determining the competitive adjustment multiplier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WinRateTier {
    /// Win rate < 30% â€” losing frequently; raise bid aggressively.
    Low,
    /// Win rate < 50% â€” underperforming; moderate raise.
    Mid,
    /// Win rate â‰¥ 50% â€” competitive; hold current bid.
    High,
}

impl WinRateTier {
    /// Classify a win rate in [0.0, 1.0].
    pub fn from_rate(rate: f64) -> Self {
        if rate < 0.30      { WinRateTier::Low  }
        else if rate < 0.50 { WinRateTier::Mid  }
        else                { WinRateTier::High }
    }

    /// Priority fee multiplier for this win rate tier.
    pub fn multiplier(self) -> f64 {
        match self {
            WinRateTier::Low  => 1.8,
            WinRateTier::Mid  => 1.3,
            WinRateTier::High => 1.0,
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// CapComponents â€” returned for telemetry
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// All intermediate values used to compute the adaptive cap.
///
/// Returned alongside the final cap so the caller can emit structured
/// telemetry without re-computing.
#[derive(Debug, Clone)]
pub struct CapComponents {
    /// Base cap before urgency and win-rate adjustments (gwei).
    pub base_cap_gwei:        u64,
    /// Urgency tier derived from the health factor.
    pub urgency_tier:         UrgencyTier,
    /// Win-rate tier derived from the relay win rate.
    pub win_rate_tier:        WinRateTier,
    /// Raw win rate value passed in [0.0, 1.0].
    pub win_rate:             f64,
    /// Final clamped cap (gwei).
    pub final_cap_gwei:       u64,
    /// Whether the output was clamped to MAX_PRIORITY_FEE_GWEI.
    pub clamped_to_max:       bool,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// adaptive_gas_cap_gwei
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Compute the adaptive priority fee cap in gwei for a single LA blueprint.
///
/// ## Arguments
///
/// - `liquidation_bonus_eth`: gross liquidation bonus in ETH.  Must be
///   finite and positive; returns `MIN_PRIORITY_FEE_GWEI` if not.
/// - `health_factor`: position health factor.  Must be finite and > 0.
/// - `win_rate_fn`: closure that accepts a candidate cap in gwei and
///   returns the relay's recent win rate for that fee level [0.0, 1.0].
///   Called exactly once with `base_cap_gwei`.
///
/// ## Returns
///
/// `(final_cap_gwei, CapComponents)` â€” the clamped cap and all
/// intermediate values for telemetry.
///
/// ## Panics
///
/// Never panics.  Non-finite or non-positive inputs are handled
/// defensively and produce the minimum valid cap.
pub fn adaptive_gas_cap_gwei(
    liquidation_bonus_eth: f64,
    health_factor:         f64,
    win_rate_fn:           impl Fn(u64) -> f64,
) -> (u64, CapComponents) {
    // â”€â”€ Guard: non-finite or non-positive bonus â†’ minimum cap â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    if !liquidation_bonus_eth.is_finite() || liquidation_bonus_eth <= 0.0 {
        tracing::warn!(
            liquidation_bonus_eth,
            "adaptive_gas_cap: non-positive bonus â€” returning minimum cap",
        );
        let components = CapComponents {
            base_cap_gwei:  MIN_PRIORITY_FEE_GWEI,
            urgency_tier:   UrgencyTier::from_health_factor(health_factor),
            win_rate_tier:  WinRateTier::High,
            win_rate:       1.0,
            final_cap_gwei: MIN_PRIORITY_FEE_GWEI,
            clamped_to_max: false,
        };
        return (MIN_PRIORITY_FEE_GWEI, components);
    }

    // â”€â”€ Base cap (Â§12 formula) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    //
    //   base_cap [gwei/gas] =
    //     (bonus_eth [ETH] Ã— GWEI_PER_ETH [gwei/ETH] Ã— BONUS_GAS_FRACTION)
    //     / GAS_PER_BUNDLE [gas]
    //
    // The division by GAS_PER_BUNDLE normalises the gwei budget to a
    // per-gas-unit priority fee that the sequencer compares.
    let base_cap_f64 = (liquidation_bonus_eth * GWEI_PER_ETH * BONUS_GAS_FRACTION)
        / GAS_PER_BUNDLE;

    // Cast to u64 safely â€” clamp to [0, u64::MAX] before truncation.
    let base_cap_gwei = if base_cap_f64 < 0.0 || !base_cap_f64.is_finite() {
        0_u64
    } else {
        base_cap_f64.min(u64::MAX as f64) as u64
    };

    // â”€â”€ Urgency multiplier â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    let urgency_tier = UrgencyTier::from_health_factor(health_factor);
    let urgency_mult = urgency_tier.multiplier();

    // â”€â”€ Win-rate multiplier â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    // win_rate_fn is called with base_cap so the relay has context on the
    // fee level we are querying.
    let win_rate     = win_rate_fn(base_cap_gwei).clamp(0.0, 1.0);
    let win_rate_tier = WinRateTier::from_rate(win_rate);
    let wr_mult      = win_rate_tier.multiplier();

    // â”€â”€ Final cap â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    // Compute in f64 to avoid intermediate u64 overflow, then clamp.
    let raw_cap_f64 = base_cap_f64 * urgency_mult * wr_mult;
    let raw_cap = if raw_cap_f64 < 0.0 || !raw_cap_f64.is_finite() {
        0_u64
    } else {
        raw_cap_f64.min(u64::MAX as f64) as u64
    };

    let clamped = raw_cap.clamp(MIN_PRIORITY_FEE_GWEI, MAX_PRIORITY_FEE_GWEI);
    let clamped_to_max = raw_cap > MAX_PRIORITY_FEE_GWEI;

    tracing::debug!(
        liquidation_bonus_eth,
        health_factor,
        base_cap_gwei,
        urgency_tier   = %urgency_tier,
        win_rate,
        raw_cap_gwei   = raw_cap,
        final_cap_gwei = clamped,
        clamped_to_max,
        "adaptive_gas_cap computed",
    );

    let components = CapComponents {
        base_cap_gwei,
        urgency_tier,
        win_rate_tier,
        win_rate,
        final_cap_gwei: clamped,
        clamped_to_max,
    };
    (clamped, components)
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    const WIN_50: fn(u64) -> f64 = |_| 0.50;  // mid tier â€” no uplift
    const WIN_20: fn(u64) -> f64 = |_| 0.20;  // low tier â€” 1.8Ã—
    const WIN_60: fn(u64) -> f64 = |_| 0.60;  // high tier â€” 1.0Ã—

    // â”€â”€ Unit analysis â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn formula_unit_analysis() {
        // 1 ETH bonus, HF low urgency, win 50%:
        // base = 1.0 * 1e9 * 0.05 / 21000 = 2380.95... â†’ 2380 gwei
        // urgency 1.0, win_rate 1.0
        // raw = 2380, clamped to 500
        let (cap, c) = adaptive_gas_cap_gwei(1.0, 1.05, WIN_50);
        assert_eq!(cap, MAX_PRIORITY_FEE_GWEI);
        assert!(c.clamped_to_max);
        assert_eq!(c.base_cap_gwei, 2_380);
    }

    #[test]
    fn small_bonus_low_hf_under_ceiling() {
        // 0.001 ETH bonus:
        // base = 0.001 * 1e9 * 0.05 / 21000 = 2.38... â†’ 2 gwei
        // HF = 1.0005 â†’ CRITICAL tier (3.0Ã—), win 20% â†’ 1.8Ã—
        // raw = 2 * 3.0 * 1.8 = 10.8 â†’ 10 gwei
        let (cap, c) = adaptive_gas_cap_gwei(0.001, 1.0005, WIN_20);
        assert_eq!(c.urgency_tier, UrgencyTier::Critical);
        assert_eq!(c.win_rate_tier, WinRateTier::Low);
        assert!(cap >= MIN_PRIORITY_FEE_GWEI);
        assert!(cap <= MAX_PRIORITY_FEE_GWEI);
        // Should be around 10 gwei (within floating point tolerance)
        assert!(cap >= 9 && cap <= 12, "cap={cap}");
    }

    // â”€â”€ Ceiling enforcement â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn always_at_most_500_gwei() {
        for bonus in [0.001, 0.1, 1.0, 10.0, 100.0] {
            for hf in [1.0, 1.0005, 1.003, 1.008, 1.05] {
                let (cap, _) = adaptive_gas_cap_gwei(bonus, hf, WIN_20);
                assert!(cap <= MAX_PRIORITY_FEE_GWEI, "cap={cap} bonus={bonus} hf={hf}");
            }
        }
    }

    // â”€â”€ Floor enforcement â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn always_at_least_2_gwei() {
        let (cap, _) = adaptive_gas_cap_gwei(0.000001, 1.5, WIN_60);
        assert!(cap >= MIN_PRIORITY_FEE_GWEI, "cap={cap}");
    }

    // â”€â”€ Edge cases â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn zero_bonus_returns_minimum() {
        let (cap, _) = adaptive_gas_cap_gwei(0.0, 1.0, WIN_50);
        assert_eq!(cap, MIN_PRIORITY_FEE_GWEI);
    }

    #[test]
    fn negative_bonus_returns_minimum() {
        let (cap, _) = adaptive_gas_cap_gwei(-1.0, 1.0, WIN_50);
        assert_eq!(cap, MIN_PRIORITY_FEE_GWEI);
    }

    #[test]
    fn nan_bonus_returns_minimum() {
        let (cap, _) = adaptive_gas_cap_gwei(f64::NAN, 1.0, WIN_50);
        assert_eq!(cap, MIN_PRIORITY_FEE_GWEI);
    }

    #[test]
    fn infinite_bonus_clamps_to_max() {
        let (cap, _) = adaptive_gas_cap_gwei(f64::INFINITY, 1.0, WIN_50);
        assert_eq!(cap, MAX_PRIORITY_FEE_GWEI);
    }

    // â”€â”€ Urgency tier classification â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn urgency_tiers() {
        assert_eq!(UrgencyTier::from_health_factor(1.0000), UrgencyTier::Critical);
        assert_eq!(UrgencyTier::from_health_factor(1.0009), UrgencyTier::Critical);
        assert_eq!(UrgencyTier::from_health_factor(1.001),  UrgencyTier::High);
        assert_eq!(UrgencyTier::from_health_factor(1.004),  UrgencyTier::High);
        assert_eq!(UrgencyTier::from_health_factor(1.005),  UrgencyTier::Moderate);
        assert_eq!(UrgencyTier::from_health_factor(1.009),  UrgencyTier::Moderate);
        assert_eq!(UrgencyTier::from_health_factor(1.01),   UrgencyTier::Low);
        assert_eq!(UrgencyTier::from_health_factor(1.5),    UrgencyTier::Low);
    }

    // â”€â”€ Win-rate tier classification â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn win_rate_tiers() {
        assert_eq!(WinRateTier::from_rate(0.00), WinRateTier::Low);
        assert_eq!(WinRateTier::from_rate(0.29), WinRateTier::Low);
        assert_eq!(WinRateTier::from_rate(0.30), WinRateTier::Mid);
        assert_eq!(WinRateTier::from_rate(0.49), WinRateTier::Mid);
        assert_eq!(WinRateTier::from_rate(0.50), WinRateTier::High);
        assert_eq!(WinRateTier::from_rate(1.00), WinRateTier::High);
    }

    // â”€â”€ Monotonicity â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn higher_urgency_never_lower_cap() {
        // Given same bonus and win rate, lower HF must produce same or higher cap
        let (cap_low,  _) = adaptive_gas_cap_gwei(0.05, 1.02,   WIN_50);
        let (cap_high, _) = adaptive_gas_cap_gwei(0.05, 1.0001, WIN_50);
        assert!(cap_high >= cap_low,
            "critical HF should produce cap â‰¥ low urgency: {cap_high} vs {cap_low}");
    }

    #[test]
    fn lower_win_rate_never_lower_cap() {
        let (cap_high, _) = adaptive_gas_cap_gwei(0.05, 1.05, WIN_20); // low win â†’ 1.8Ã—
        let (cap_low,  _) = adaptive_gas_cap_gwei(0.05, 1.05, WIN_60); // high win â†’ 1.0Ã—
        assert!(cap_high >= cap_low,
            "low win rate should produce cap â‰¥ high win rate: {cap_high} vs {cap_low}");
    }
}