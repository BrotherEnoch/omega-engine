// omega-prl/src/ml/inference.rs
//! ONNX Runtime inference engine â€” Â§16.3
//!
//! Runtime:        ONNX Runtime via `ort` crate (2.0.0-rc.12)
//! Precision:      FP32 input; FP16 model weights where supported
//! Latency budget: 50 Âµs hot path (Â§17.3 â€” exceeded â†’ fallback)
//! Batch size:     1 on hot path (Â§16.3)
//! Fallback:       deterministic heuristics (Â§16.3, Â§17.2)
//!
//! ## Thread-safety
//! `OnnxInferenceEngine` is `Send + Sync`.  Each PRL shard holds an
//! `Arc<OnnxInferenceEngine>` and calls `infer()` concurrently.
//!
//! ## ort 2.x API notes
//! - `Session` is at `ort::session::Session`, not `ort::Session`.
//! - `Session::builder()` returns `ort::Result<SessionBuilder>`.
//! - `SessionBuilder::commit_from_file` takes `&mut self`.
//! - `Session::run()` takes `&mut self` â€” `RealOrtSession` wraps `Session`
//!   in a `parking_lot::Mutex` so the `&self` `SessionRun` trait is satisfied.
//!   The mutex is uncontended per-shard because each shard owns its engine.
//! - Tensor input: use `([usize; 2], &[f32])` which implements `TensorArrayData`
//!   without requiring any `ndarray` types.  This eliminates the multiple-ndarray-
//!   version conflict (ort bundles its own ndarray; we must not mix).
//! - Tensor output: `try_extract_tensor::<T>()` returns `(&Shape, &[T])`.
//!   Scalar outputs are read with `.first().copied()`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use ort::session::Session;
use ort::value::TensorRef;

use crate::features::extractor::FeatureVector;
use crate::ml::checkpoints::ModelCheckpointStore;
use crate::ml::fallback::DeterministicFallback;

/// Known model names (Â§16.4).
pub const MODEL_RELAY:       &str = "relay-model";
pub const MODEL_GAS_WAR:     &str = "gas-war-model";
pub const MODEL_LIQUIDATION: &str = "liquidation-risk";
pub const MODEL_SEARCHER:    &str = "searcher-fingerprint";

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// InferenceResult
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Output of one ML inference call.
#[derive(Debug, Clone, Copy)]
pub struct InferenceResult {
    /// Primary probability output [0, 1].
    pub probability: f32,
    /// Secondary class index (model-specific).
    pub class_index: u8,
    /// Wall-clock inference latency in microseconds.
    pub latency_us:  u64,
    /// `true` if result came from the ONNX model; `false` if from heuristic fallback.
    pub from_ml:     bool,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// SessionRun trait
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Internal abstraction over a loaded ONNX session.
///
/// Takes `&self` because the engine exposes a `&self` inference API;
/// `RealOrtSession` satisfies this by wrapping `Session` in a `Mutex`.
trait SessionRun: Send + Sync {
    fn run(&self, input: &[f32; 64]) -> (f32, u8);
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// RealOrtSession
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Production ONNX Runtime session.
///
/// `Session::run` takes `&mut self` in ort 2.x, so the session is wrapped in
/// a `parking_lot::Mutex`.  Per-shard ownership means this is uncontended.
struct RealOrtSession {
    session: parking_lot::Mutex<Session>,
}

impl SessionRun for RealOrtSession {
    fn run(&self, input: &[f32; 64]) -> (f32, u8) {
        // Build tensor input using the ndarray-free (shape, &[T]) form.
        // ([usize; 2], &[f32]) implements TensorArrayData<f32> in ort 2.x
        // without touching ort's internal ndarray dependency â€” eliminates
        // the multiple-ndarray-version conflict (E0277 from ndarray mismatch).
        let shape: [usize; 2] = [1, 64];
        let tensor_ref = TensorRef::from_array_view((shape, input.as_slice()))
            .expect("TensorRef from contiguous [1,64] f32 slice cannot fail");

        // inputs! returns Vec<(Cow<str>, SessionInputValue)> â€” no Result wrapping.
        let inputs = ort::inputs!["input" => tensor_ref];

        let mut session = self.session.lock();
        let outputs = session.run(inputs).expect("ort inference failed");

        // try_extract_tensor returns (&Shape, &[T]) â€” flat slice, not ndarray.
        let (_, prob_data) = outputs["probability"]
            .try_extract_tensor::<f32>()
            .expect("probability output must be f32");
        let (_, class_data) = outputs["class_index"]
            .try_extract_tensor::<i64>()
            .expect("class_index output must be i64");

        let probability = prob_data.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
        let class_index = class_data.first().copied().unwrap_or(0) as u8;
        (probability, class_index)
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// OrtSession wrapper
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

struct OrtSession {
    #[allow(dead_code)]
    name:  String,
    inner: Box<dyn SessionRun>,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// OnnxInferenceEngine
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// ONNX Runtime inference engine (Â§16.3).
///
/// Each PRL shard worker holds an `Arc<OnnxInferenceEngine>`.  The engine is
/// constructed once at startup; hot-reload of model weights is handled via
/// `rollback_to()` which atomically replaces all active sessions under the
/// write lock of `sessions`.
pub struct OnnxInferenceEngine {
    #[allow(dead_code)]
    model_dir:      PathBuf,
    /// `true` when ONNX model inference is active; `false` â†’ heuristic only.
    ml_active:      AtomicBool,
    /// Consecutive latency-exceeded streak.  At â‰¥3: ML is disabled (Â§17.3).
    timeout_streak: AtomicU64,
    /// Max allowed inference latency in Âµs before fallback triggers (Â§17.3).
    max_latency_us: u64,
    fallback:       DeterministicFallback,
    checkpoints:    Arc<ModelCheckpointStore>,
    /// Loaded sessions â€” one per model name.
    /// `parking_lot::RwLock` for minimal contention on reads.
    sessions: parking_lot::RwLock<std::collections::HashMap<String, OrtSession>>,
}

impl OnnxInferenceEngine {
    /// Load ONNX models from `model_dir`.
    ///
    /// Returns `Err` only if the directory is missing.  Individual model load
    /// failures emit `warn!` and fall back to heuristics for that model â€”
    /// the engine is never returned in a fully broken state.
    pub fn load(model_dir: &Path) -> anyhow::Result<Self> {
        if !model_dir.exists() {
            anyhow::bail!("Model directory not found: {}", model_dir.display());
        }

        let checkpoints  = Arc::new(ModelCheckpointStore::load(model_dir)?);
        let mut sessions_map = std::collections::HashMap::new();

        for name in [MODEL_RELAY, MODEL_GAS_WAR, MODEL_LIQUIDATION, MODEL_SEARCHER] {
            match checkpoints.active_path(name) {
                Some(p) if p.exists() => {
                    match Session::builder()
                        .and_then(|mut b| b.commit_from_file(&p))
                    {
                        Ok(session) => {
                            sessions_map.insert(name.to_string(), OrtSession {
                                name:  name.to_string(),
                                inner: Box::new(RealOrtSession {
                                    session: parking_lot::Mutex::new(session),
                                }),
                            });
                            tracing::info!(model = name, path = %p.display(),
                                "ONNX model loaded");
                        }
                        Err(e) => {
                            tracing::warn!(model = name, error = %e,
                                "ONNX model failed to load â€” heuristic fallback active");
                        }
                    }
                }
                _ => {
                    tracing::warn!(model = name,
                        "ONNX model file not found â€” heuristic fallback active");
                }
            }
        }

        let any_loaded = !sessions_map.is_empty();
        tracing::info!(loaded = sessions_map.len(), total = 4,
            "ONNX model loading complete");

        Ok(Self {
            model_dir:      model_dir.to_path_buf(),
            ml_active:      AtomicBool::new(any_loaded),
            timeout_streak: AtomicU64::new(0),
            max_latency_us: 50,
            fallback:       DeterministicFallback::new(),
            checkpoints,
            sessions:       parking_lot::RwLock::new(sessions_map),
        })
    }

    /// Construct an engine that runs heuristic fallback only.
    /// Used when the model directory is absent at startup (Â§17.2).
    pub fn heuristic_fallback() -> Self {
        Self {
            model_dir:      PathBuf::new(),
            ml_active:      AtomicBool::new(false),
            timeout_streak: AtomicU64::new(0),
            max_latency_us: 50,
            fallback:       DeterministicFallback::new(),
            checkpoints:    Arc::new(ModelCheckpointStore::empty()),
            sessions:       parking_lot::RwLock::new(std::collections::HashMap::new()),
        }
    }

    /// Run inference.  Lock-free on the ML-disabled fast path.
    ///
    /// If ML is active: times the ONNX call.  Three consecutive timeouts
    /// (>50 Âµs) disable ML globally and trigger the DEGRADED path (Â§17.3).
    /// If ML is disabled: returns the deterministic heuristic instantly.
    #[inline]
    pub fn infer(&self, model_name: &str, fv: &FeatureVector) -> InferenceResult {
        if !self.ml_active.load(Ordering::Relaxed) {
            return self.fallback.infer(model_name, fv);
        }

        let t0     = Instant::now();
        let result = self.run_onnx(model_name, fv);
        let lat    = t0.elapsed().as_micros() as u64;

        if lat > self.max_latency_us {
            let streak = self.timeout_streak.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::warn!(model = model_name, latency_us = lat, streak,
                "PRL inference exceeded latency budget");
            if streak >= 3 {
                self.ml_active.store(false, Ordering::SeqCst);
                tracing::warn!(
                    "PRL ML engine auto-disabled after {} consecutive timeouts", streak
                );
            }
            return self.fallback.infer(model_name, fv);
        }

        self.timeout_streak.store(0, Ordering::Relaxed);
        result
    }

    /// Re-enable ML path after governance review (Â§17.3).
    pub fn reenable_ml(&self) {
        self.ml_active.store(true, Ordering::SeqCst);
        self.timeout_streak.store(0, Ordering::Relaxed);
        tracing::info!("PRL ML engine re-enabled via governance");
    }

    pub fn is_ml_active(&self) -> bool {
        self.ml_active.load(Ordering::Relaxed)
    }

    /// Roll back all active model sessions to checkpoint `version` (Â§16.5).
    pub fn rollback_to(&self, version: u32) -> anyhow::Result<()> {
        self.checkpoints.rollback_to(version)?;
        let mut map = self.sessions.write();
        map.clear();
        for name in [MODEL_RELAY, MODEL_GAS_WAR, MODEL_LIQUIDATION, MODEL_SEARCHER] {
            if let Some(p) = self.checkpoints.active_path(name) {
                if p.exists() {
                    if let Ok(session) = Session::builder()
                        .and_then(|mut b| b.commit_from_file(&p))
                    {
                        map.insert(name.to_string(), OrtSession {
                            name:  name.to_string(),
                            inner: Box::new(RealOrtSession {
                                session: parking_lot::Mutex::new(session),
                            }),
                        });
                    }
                }
            }
        }
        tracing::info!(version, "PRL model sessions rolled back");
        Ok(())
    }

    // â”€â”€ Internal â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    fn run_onnx(&self, model_name: &str, fv: &FeatureVector) -> InferenceResult {
        let guard = self.sessions.read();
        if let Some(session) = guard.get(model_name) {
            let (probability, class_index) = session.inner.run(&fv.values);
            InferenceResult { probability, class_index, latency_us: 0, from_ml: true }
        } else {
            let mut r = self.fallback.infer(model_name, fv);
            r.from_ml = false;
            r
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::extractor::FeatureVector;

    #[test]
    fn heuristic_fallback_engine_is_not_ml() {
        let engine = OnnxInferenceEngine::heuristic_fallback();
        assert!(!engine.is_ml_active());
        let fv = FeatureVector::zeroed();
        let r  = engine.infer(MODEL_GAS_WAR, &fv);
        assert!(!r.from_ml);
        assert!(r.probability >= 0.0 && r.probability <= 1.0);
    }

    #[test]
    fn reenable_resets_streak() {
        let engine = OnnxInferenceEngine::heuristic_fallback();
        engine.timeout_streak.store(3, Ordering::SeqCst);
        engine.reenable_ml();
        assert_eq!(engine.timeout_streak.load(Ordering::Relaxed), 0);
        assert!(engine.is_ml_active());
    }
}