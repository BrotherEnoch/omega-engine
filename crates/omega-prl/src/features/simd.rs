// omega-prl/src/features/simd.rs
//! SIMD acceleration for feature vector operations — §6.3
//!
//! Uses the `wide` crate for portable SIMD (compiles to AVX2/AVX512 via
//! autovectorisation).  All functions operate on `[f32; 64]` feature vectors.
//!
//! Operations provided:
//!   - dot product (scoring)
//!   - L2 distance (similarity matching)
//!   - element-wise threshold comparison
//!   - weighted sum
//!   - cosine similarity
//!   - element-wise max

// `CmpGt` is a trait in `wide` — must be in scope for `f32x8::cmp_gt` to
// resolve.  Without this import the compiler sees the method on f32x8 but
// cannot call it (E0599 "items from traits can only be used if the trait is
// in scope").
use wide::{CmpGt, f32x8};

use crate::features::extractor::{FEATURE_DIM, FeatureVector};

// FEATURE_DIM (64) must be divisible by SIMD lane width (8). Compile-time check.
const _: () = assert!(FEATURE_DIM.is_multiple_of(8));
const LANES: usize = 8;
const CHUNKS: usize = FEATURE_DIM / LANES;

/// Compute dot product of two feature vectors.
/// Uses 8-wide f32 SIMD. Result is the raw (unscaled) inner product.
#[inline]
pub fn dot_product(a: &FeatureVector, b: &FeatureVector) -> f32 {
    let mut acc = f32x8::ZERO;
    for i in 0..CHUNKS {
        let off = i * LANES;
        let va = f32x8::from(&a.values[off..off + LANES]);
        let vb = f32x8::from(&b.values[off..off + LANES]);
        acc += va * vb;
    }
    acc.reduce_add()
}

/// Compute L2 (Euclidean) distance between two feature vectors.
#[inline]
pub fn l2_distance(a: &FeatureVector, b: &FeatureVector) -> f32 {
    let mut acc = f32x8::ZERO;
    for i in 0..CHUNKS {
        let off = i * LANES;
        let va = f32x8::from(&a.values[off..off + LANES]);
        let vb = f32x8::from(&b.values[off..off + LANES]);
        let diff = va - vb;
        acc += diff * diff;
    }
    acc.reduce_add().sqrt()
}

/// Weighted dot product: dot(fv.values, weights).
#[inline]
pub fn weighted_score(fv: &FeatureVector, weights: &[f32; FEATURE_DIM]) -> f32 {
    let mut acc = f32x8::ZERO;
    for i in 0..CHUNKS {
        let off = i * LANES;
        let va = f32x8::from(&fv.values[off..off + LANES]);
        let vw = f32x8::from(&weights[off..off + LANES]);
        acc += va * vw;
    }
    acc.reduce_add()
}

/// Count features exceeding `threshold` (for anomaly scoring).
///
/// `f32x8::cmp_gt` is provided by `wide::CmpGt` and returns an `f32x8`
/// where matching lanes are all-bits-set (to_bits() != 0) and non-matching
/// lanes are 0.0.
#[inline]
pub fn count_above_threshold(fv: &FeatureVector, threshold: f32) -> u32 {
    let vt = f32x8::splat(threshold);
    let mut count = 0u32;
    for i in 0..CHUNKS {
        let off = i * LANES;
        let va = f32x8::from(&fv.values[off..off + LANES]);
        let mask: [f32; 8] = va.cmp_gt(vt).into();
        for v in mask {
            if v.to_bits() != 0 {
                count += 1;
            }
        }
    }
    count
}

/// Cosine similarity: dot(a, b) / (|a| * |b|).
#[inline]
pub fn cosine_similarity(a: &FeatureVector, b: &FeatureVector) -> f32 {
    let dot = dot_product(a, b);
    let na = dot_product(a, a).sqrt();
    let nb = dot_product(b, b).sqrt();
    let denom = na * nb;
    if denom < 1e-9 {
        return 0.0;
    }
    (dot / denom).clamp(-1.0, 1.0)
}

/// Element-wise max of two vectors — useful for merging signal vectors.
#[inline]
pub fn elementwise_max(a: &FeatureVector, b: &FeatureVector) -> [f32; FEATURE_DIM] {
    let mut out = [0.0f32; FEATURE_DIM];
    for i in 0..CHUNKS {
        let off = i * LANES;
        let va = f32x8::from(&a.values[off..off + LANES]);
        let vb = f32x8::from(&b.values[off..off + LANES]);
        let vm: [f32; 8] = va.max(vb).into();
        out[off..off + LANES].copy_from_slice(&vm);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::extractor::FeatureVector;

    fn make_fv(val: f32) -> FeatureVector {
        let mut fv = FeatureVector::zeroed();
        for i in 0..FEATURE_DIM {
            fv.set(i, val);
        }
        fv
    }

    #[test]
    fn dot_product_unit_vectors() {
        let a = make_fv(1.0);
        let b = make_fv(1.0);
        assert!((dot_product(&a, &b) - FEATURE_DIM as f32).abs() < 1e-3);
    }

    #[test]
    fn l2_distance_identical_is_zero() {
        let a = make_fv(0.5);
        assert!(l2_distance(&a, &a) < 1e-6);
    }

    #[test]
    fn cosine_similarity_identical() {
        let a = make_fv(0.7);
        assert!((cosine_similarity(&a, &a) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn count_above_threshold_correct() {
        let mut fv = FeatureVector::zeroed();
        for i in 0..32 {
            fv.set(i, 0.9);
        } // first 32 above 0.5
        assert_eq!(count_above_threshold(&fv, 0.5), 32);
    }
}
