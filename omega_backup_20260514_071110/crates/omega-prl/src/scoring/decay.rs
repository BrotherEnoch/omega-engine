// omega-prl/src/scoring/decay.rs
//! Confidence decay function â€” Â§15.2
//!
//! Formula: base Ã— exp(âˆ’0.0001 Ã— age_ms)
//! Half-life â‰ˆ 6.93 seconds.  Effectively zero after ~100 seconds.

/// Â§15.2 â€” Exponential confidence decay.
///
/// At age 1 s  (1 000 ms):  base Ã— 0.905
/// At age 10 s (10 000 ms): base Ã— 0.368
/// At age 1 m  (60 000 ms): base Ã— 0.002
#[inline]
pub fn decay_confidence(base: f64, age_ms: u64) -> f64 {
    if base <= 0.0 { return 0.0; }
    (base * (-0.0001 * age_ms as f64).exp()).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_age_is_identity() {
        assert!((decay_confidence(1.0, 0) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn approaches_zero_at_100s() {
        assert!(decay_confidence(1.0, 100_000) < 0.01);
    }

    #[test]
    fn linear_in_base() {
        let d1 = decay_confidence(0.5, 1_000);
        let d2 = decay_confidence(1.0, 1_000);
        assert!((d2 - 2.0 * d1).abs() < 1e-12);
    }

    #[test]
    fn negative_base_returns_zero() {
        assert_eq!(decay_confidence(-0.5, 100), 0.0);
    }
}