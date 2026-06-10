// omega-prl/src/features/extractor.rs
//! Deterministic feature extraction — §6
//!
//! Requirements (§6.1):
//!   - No heap allocations on hot path
//!   - No regex; no dynamic dispatch
//!   - Precompiled lookup tables
//!   - Branch-minimised logic
//!
//! Feature categories (§6.2):
//!   Temporal | Economic | Behavioral | Structural | Adversarial
//!   Execution | Market | Oracle

use std::sync::LazyLock;

use crate::ingestion::event_bus::{EventType, PatternEvent};

// ─────────────────────────────────────────────────────────────────────────────
// Fixed-size feature vector — no heap, cache-line aligned
// ─────────────────────────────────────────────────────────────────────────────

/// Feature vector length — must be divisible by SIMD width (8 f32 lanes).
pub const FEATURE_DIM: usize = 64;

/// Extracted feature vector. Stack-allocated; passed by value on hot path.
#[derive(Debug, Clone, Copy)]
#[repr(C, align(64))]
pub struct FeatureVector {
    /// Normalised feature values in `[-1, 1]`.
    pub values: [f32; FEATURE_DIM],
    /// Bitmask of which features are populated (non-zero).
    pub present: u64,
    /// Source event type that produced this vector.
    pub event_type: u8,
    /// Timestamp of the originating event (nanos).
    pub ts_nanos: u64,
}

impl FeatureVector {
    #[inline]
    pub const fn zeroed() -> Self {
        Self {
            values: [0.0f32; FEATURE_DIM],
            present: 0,
            event_type: 0,
            ts_nanos: 0,
        }
    }

    /// Set a feature by index and mark it present.
    #[inline]
    pub fn set(&mut self, idx: usize, value: f32) {
        debug_assert!(idx < FEATURE_DIM);
        self.values[idx] = value;
        self.present |= 1u64 << (idx.min(63));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Feature index constants (per §6.2 categories)
// ─────────────────────────────────────────────────────────────────────────────

/// Temporal features [0..7]
pub const F_INTER_BLOCK_LATENCY: usize = 0;
pub const F_RELAY_DELAY_US: usize = 1;
pub const F_INCLUSION_JITTER: usize = 2;
pub const F_BLOCK_INTERVAL_NORM: usize = 3;

/// Economic features [8..15]
pub const F_GAS_ESCALATION_VEL: usize = 8;
pub const F_GAS_PREMIUM_NORM: usize = 9;
pub const F_PROFIT_DELTA_NORM: usize = 10;
pub const F_FEE_RATIO: usize = 11;

/// Behavioral features [16..23]
pub const F_RELAY_ACCEPT_BIAS: usize = 16;
pub const F_RETRY_CADENCE: usize = 17;
pub const F_BUNDLE_RETRY_COUNT: usize = 18;
pub const F_SEARCHER_AGGRESSION: usize = 19;

/// Structural features [24..31]
pub const F_CALLDATA_SIZE_NORM: usize = 24;
pub const F_HOP_COUNT: usize = 25;
pub const F_PROTOCOL_ID: usize = 26;
pub const F_BUNDLE_COMPLEXITY: usize = 27;

/// Adversarial features [32..39]
pub const F_FRONTRUN_PROBABILITY: usize = 32;
pub const F_LEAK_SUSPICION: usize = 33;
pub const F_CENSORSHIP_SCORE: usize = 34;
pub const F_PATTERN_POISON_SCORE: usize = 35;

/// Execution features [40..47]
pub const F_REVERT_RATE: usize = 40;
pub const F_SIM_DIVERGENCE: usize = 41;
pub const F_GAS_MISCALC_RATE: usize = 42;
pub const F_EXECUTION_LATENCY: usize = 43;

/// Market features [48..55]
pub const F_VOLATILITY_ACCEL: usize = 48;
pub const F_PRICE_IMPACT_NORM: usize = 49;
pub const F_LIQUIDITY_SCORE: usize = 50;
pub const F_HF_VELOCITY: usize = 51;

/// Oracle features [56..63]
pub const F_ORACLE_CORR_DIV: usize = 56;
pub const F_ORACLE_UPDATE_FREQ: usize = 57;
pub const F_PRICE_DEVIATION: usize = 58;
pub const F_ORACLE_MANIPULATION: usize = 59;

// ─────────────────────────────────────────────────────────────────────────────
// Lookup tables
//
// `f32::sqrt()` is not a `const fn` — const_fn_floating_point_arithmetic is
// not stabilised — so a bare `static [f32; 128] = { ... x.sqrt() }` is
// E0015.  `LazyLock::new` IS const, so `LazyLock<[f32; 128]>` is a valid
// static initialiser.  The closure runs exactly once on first dereference;
// `FeatureExtractor::new` forces it at construction time so the hot path
// never pays the one-time cost.
// ─────────────────────────────────────────────────────────────────────────────

/// Precomputed inverse-sqrt lookup for fast normalisation (128 entries).
/// Entry `i` holds `1.0 / sqrt(i + 1)`.
static INV_SQRT_TABLE: LazyLock<[f32; 128]> = LazyLock::new(|| {
    let mut t = [0.0f32; 128];
    for (i, value) in t.iter_mut().enumerate() {
        let x = (i + 1) as f32;
        *value = 1.0 / x.sqrt();
    }
    t
});

// ─────────────────────────────────────────────────────────────────────────────
// Normalisation helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Normalise a gas value (gwei) into [-1, 1] given a baseline (typ. 50 gwei).
#[inline]
fn norm_gas(gwei: u64, baseline_gwei: u64) -> f32 {
    if baseline_gwei == 0 {
        return 0.0;
    }
    ((gwei as f64 / baseline_gwei as f64) - 1.0).clamp(-1.0, 1.0) as f32
}

/// Normalise a latency (µs) into [0, 1] with 10ms ceiling.
#[inline]
fn norm_latency_us(us: u64) -> f32 {
    (us as f32 / 10_000.0).clamp(0.0, 1.0)
}

/// Clamp a probability already in [0, 1].
#[inline]
fn norm_prob(p: f32) -> f32 {
    p.clamp(0.0, 1.0)
}

// ─────────────────────────────────────────────────────────────────────────────
// FeatureExtractor
// ─────────────────────────────────────────────────────────────────────────────

/// Deterministic, allocation-free feature extractor (§6.1).
pub struct FeatureExtractor {
    /// Rolling baseline for gas normalisation (EWMA).
    gas_baseline_gwei: u64,
    /// Rolling baseline for block interval (EWMA, nanos).
    block_interval_baseline_ns: u64,
}

impl FeatureExtractor {
    pub fn new() -> Self {
        // Force table initialisation now so the first hot-path call is free.
        let _ = &*INV_SQRT_TABLE;
        Self {
            gas_baseline_gwei: 50,
            block_interval_baseline_ns: 250_000_000, // 250ms Arbitrum baseline
        }
    }

    /// Extract features from a `PatternEvent` into a stack-allocated
    /// `FeatureVector`.  Returns `None` if the event type has no extractable
    /// features.  No heap allocation.
    #[inline]
    pub fn extract(&self, event: &PatternEvent) -> Option<FeatureVector> {
        let mut fv = FeatureVector::zeroed();
        fv.event_type = event.event_type as u8;
        fv.ts_nanos = event.ts_nanos;

        match event.event_type {
            EventType::OraclePriceUpdate => self.extract_oracle(event, &mut fv),
            EventType::RelayInclusionResult => self.extract_relay(event, &mut fv),
            EventType::BundleIncluded => self.extract_bundle_included(event, &mut fv),
            EventType::BundleDropped => self.extract_bundle_dropped(event, &mut fv),
            EventType::LossRecorded => self.extract_loss(event, &mut fv),
            EventType::GasEscalation => self.extract_gas(event, &mut fv),
            EventType::SequencerRestart => self.extract_sequencer(&mut fv),
            EventType::ReorgDetected => self.extract_reorg(event, &mut fv),
            EventType::SimulationResult => self.extract_simulation(event, &mut fv),
            _ => return None,
        }

        Some(fv)
    }

    /// Update rolling gas baseline (EWMA α = 0.05).
    #[inline]
    pub fn update_baseline_gas(&mut self, gwei: u64) {
        self.gas_baseline_gwei = (self.gas_baseline_gwei as f64 * 0.95 + gwei as f64 * 0.05) as u64;
    }

    /// Update rolling block-interval baseline (EWMA α = 0.05).
    pub fn update_baseline_block_interval(&mut self, ns: u64) {
        self.block_interval_baseline_ns =
            (self.block_interval_baseline_ns as f64 * 0.95 + ns as f64 * 0.05) as u64;
    }

    // ── Per-type extractors ───────────────────────────────────────────────────

    fn extract_oracle(&self, ev: &PatternEvent, fv: &mut FeatureVector) {
        let p = &ev.payload;
        // Payload: [price_raw(8)] [prev_price(8)] [update_interval_ms(4)]
        if ev.payload_len < 20 {
            return;
        }
        let price_raw = u64::from_le_bytes(p[0..8].try_into().unwrap_or([0; 8]));
        let prev_price = u64::from_le_bytes(p[8..16].try_into().unwrap_or([0; 8]));
        let update_ms = u32::from_le_bytes(p[16..20].try_into().unwrap_or([0; 4]));

        let deviation = if prev_price > 0 {
            ((price_raw as f64 - prev_price as f64) / prev_price as f64).abs() as f32
        } else {
            0.0
        };

        fv.set(F_PRICE_DEVIATION, norm_prob(deviation * 100.0));
        fv.set(
            F_ORACLE_UPDATE_FREQ,
            norm_latency_us(update_ms as u64 * 1000),
        );
        fv.set(
            F_ORACLE_MANIPULATION,
            norm_prob(if deviation > 0.05 { 0.8 } else { 0.0 }),
        );
    }

    fn extract_relay(&self, ev: &PatternEvent, fv: &mut FeatureVector) {
        let p = &ev.payload;
        // Payload: [relay_id(4)] [included(1)] [latency_us(4)] [leak_suspected(1)]
        if ev.payload_len < 10 {
            return;
        }
        let included = p[4] != 0;
        let latency_us = u32::from_le_bytes(p[5..9].try_into().unwrap_or([0; 4]));
        let leak_suspected = p[9] != 0;

        fv.set(F_RELAY_ACCEPT_BIAS, if included { 1.0 } else { -1.0 });
        fv.set(F_RELAY_DELAY_US, norm_latency_us(latency_us as u64));
        fv.set(F_LEAK_SUSPICION, if leak_suspected { 1.0 } else { 0.0 });
    }

    fn extract_bundle_included(&self, ev: &PatternEvent, fv: &mut FeatureVector) {
        let p = &ev.payload;
        // Payload: [gas_used(8)] [gas_price_gwei(8)] [profit_wei(8)]
        if ev.payload_len < 24 {
            return;
        }
        let gas_price = u64::from_le_bytes(p[8..16].try_into().unwrap_or([0; 8]));
        fv.set(
            F_GAS_PREMIUM_NORM,
            norm_gas(gas_price, self.gas_baseline_gwei),
        );
    }

    fn extract_bundle_dropped(&self, ev: &PatternEvent, fv: &mut FeatureVector) {
        let p = &ev.payload;
        // Payload: [drop_reason(1)] [gas_at_drop(8)]
        if ev.payload_len < 9 {
            return;
        }
        let gas = u64::from_le_bytes(p[1..9].try_into().unwrap_or([0; 8]));
        fv.set(F_GAS_PREMIUM_NORM, norm_gas(gas, self.gas_baseline_gwei));
        fv.set(F_RELAY_ACCEPT_BIAS, -1.0);
    }

    fn extract_loss(&self, ev: &PatternEvent, fv: &mut FeatureVector) {
        let p = &ev.payload;
        // Payload: [loss_code(1)] [gas_paid(8)] [gas_used(8)] [protocol_id(1)]
        if ev.payload_len < 18 {
            return;
        }
        let gas_paid = u64::from_le_bytes(p[1..9].try_into().unwrap_or([0; 8]));
        let gas_used = u64::from_le_bytes(p[9..17].try_into().unwrap_or([0; 8]));
        let protocol_id = p[17] as f32 / 4.0; // normalise 0..4 → [0,1]

        fv.set(
            F_GAS_MISCALC_RATE,
            if gas_used > gas_paid { 1.0 } else { 0.0 },
        );
        fv.set(F_PROTOCOL_ID, protocol_id);
        fv.set(F_REVERT_RATE, norm_prob(0.5));
    }

    fn extract_gas(&self, ev: &PatternEvent, fv: &mut FeatureVector) {
        let p = &ev.payload;
        // Payload: [prev_gwei(8)] [curr_gwei(8)] [competitor_count(2)]
        if ev.payload_len < 18 {
            return;
        }
        let prev = u64::from_le_bytes(p[0..8].try_into().unwrap_or([0; 8]));
        let curr = u64::from_le_bytes(p[8..16].try_into().unwrap_or([0; 8]));
        let comps = u16::from_le_bytes(p[16..18].try_into().unwrap_or([0; 2]));

        let velocity = if prev > 0 {
            ((curr as f64 - prev as f64) / prev as f64).clamp(-1.0, 1.0) as f32
        } else {
            0.0
        };

        fv.set(F_GAS_ESCALATION_VEL, velocity);
        fv.set(F_GAS_PREMIUM_NORM, norm_gas(curr, self.gas_baseline_gwei));
        fv.set(F_BUNDLE_COMPLEXITY, (comps as f32 / 20.0).clamp(0.0, 1.0));
    }

    /// Sequencer restart — no payload fields used; max latency signal.
    /// Parameter renamed `_ev` to suppress unused-variable warning.
    fn extract_sequencer(&self, fv: &mut FeatureVector) {
        fv.set(F_INTER_BLOCK_LATENCY, 1.0);
        fv.set(F_BLOCK_INTERVAL_NORM, 1.0);
    }

    fn extract_reorg(&self, ev: &PatternEvent, fv: &mut FeatureVector) {
        let p = &ev.payload;
        // Payload: [depth(1)] [blocks_orphaned(2)]
        if ev.payload_len < 3 {
            return;
        }
        let depth = p[0] as f32 / 10.0;
        fv.set(F_INCLUSION_JITTER, depth);
        fv.set(F_BLOCK_INTERVAL_NORM, depth);
    }

    fn extract_simulation(&self, ev: &PatternEvent, fv: &mut FeatureVector) {
        let p = &ev.payload;
        // Payload: [diverged(1)] [sim_profit(8)] [actual_profit(8)]
        if ev.payload_len < 17 {
            return;
        }
        let diverged = p[0] != 0;
        fv.set(F_SIM_DIVERGENCE, if diverged { 1.0 } else { 0.0 });
    }
}

impl Default for FeatureExtractor {
    fn default() -> Self {
        Self::new()
    }
}
