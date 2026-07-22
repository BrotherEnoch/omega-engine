ï»¿// crates/omega-loss-attribution/src/online_learner.rs
//
// Online gas model learner (spec Â§13, Â§13.1, Â§13.3).
//
// ## Spec Â§13 â€” Online fee multiplier learning
//
//   The Gas War Engine uses per-FeatureKey fee multipliers to adjust the
//   priority fee cap.  These multipliers are updated online from loss
//   events: when we lose because our fee was too low (LostGasLow) the
//   multiplier increases; when we overbid (LostGasOverbid) it decreases.
//
// ## Spec Â§13.1 â€” 80/20 train/validate split (fix C1)
//
//   20% of loss events are deterministically held out for validation
//   (using `LossEvent::is_holdout()`).  Every `checkpoint_interval`
//   events the holdout set is evaluated against the current model.
//   If the holdout win rate drops more than `revert_threshold` below
//   the last checkpoint, the model is reverted to that checkpoint.
//
// ## Spec Â§13.3 â€” Ceiling escalation (fix I5)
//
//   If the multiplier for a FeatureKey is at the ceiling (5.0Ã—) for
//   more than `ceiling_escalation_threshold` consecutive LostGasLow
//   events, the model is paused and a DEGRADED health alert is emitted.
//   Manual governance clearance (L2 fast-approve) is required to unpause
//   via POST /api/v1/la/gas-model/unpause.
//
// ## Holdout win rate computation
//
//   The holdout win rate is the fraction of holdout events where the
//   current model's multiplier for the event's FeatureKey is >= the
//   multiplier that would have been needed to win.
//
//   Proxy used here (matching spec intent): for LostGasLow events, the
//   model "wins" the holdout check if `our_fee_gwei Ã— multiplier >
//   competing_fee_gwei`.  For LostGasOverbid events the model "wins"
//   if `our_fee_gwei Ã— multiplier <= our_fee_gwei` (i.e. multiplier â‰¤
//   1.0, no overbid).  All other loss codes count as "wins" for the
//   holdout check (non-gas losses are not multiplier-attributable).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use omega_core::{HealthState, LayerHealth, MlConfig};

use super::checkpoint::{self, ModelCheckpoint};
use super::classifier::{FeatureKey, LossCode, LossEvent};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// GasModelOnlineLearner
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Online gradient descent fee multiplier model (Â§13, Â§13.1, Â§13.3).
///
/// Not `Clone` â€” there must be exactly one learner instance per engine.
/// Shared state (`paused`, `ceiling_hit_count`) uses atomics so that the
/// governance API can read/clear them without taking a mutable reference.
pub struct GasModelOnlineLearner {
    // â”€â”€ Model state â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    pub fee_multipliers: HashMap<FeatureKey, f64>,

    // â”€â”€ Configuration (from MlConfig) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    pub learning_rate:                f64,
    pub validation_ratio:             f64,
    pub checkpoint_interval:          u64,
    pub revert_threshold:             f64,
    pub multiplier_ceiling:           f64,
    pub multiplier_floor:             f64,
    pub ceiling_escalation_threshold: u64,
    pub checkpoint_dir:               PathBuf,
    pub checkpoint_retention:         usize,

    // â”€â”€ Accounting â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    pub total_losses:      u64,
    pub baseline_win_rate: f64,
    pub checkpoint:        Option<ModelCheckpoint>,

    // â”€â”€ Holdout buffer (20% of events, cleared after each validation) â”€â”€â”€â”€â”€
    pub held_out: Vec<LossEvent>,

    // â”€â”€ Ceiling escalation state â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// True when the model is paused pending governance clearance (Â§13.3).
    pub paused: Arc<AtomicBool>,

    /// Consecutive LostGasLow events at the ceiling, per-FeatureKey
    /// aggregated into a single counter.  Reset to zero on any non-ceiling
    /// update.
    ceiling_hit_count: Arc<AtomicU64>,

    // â”€â”€ Health layer reference (for DEGRADED transition on escalation) â”€â”€â”€â”€
    health: Option<Arc<dyn LayerHealth>>,
}

impl GasModelOnlineLearner {
    // â”€â”€ Constructors â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Create from config.
    pub fn from_config(config: &MlConfig) -> Self {
        Self {
            fee_multipliers:              HashMap::new(),
            learning_rate:                config.learning_rate,
            validation_ratio:             config.validation_ratio,
            checkpoint_interval:          config.checkpoint_interval,
            revert_threshold:             config.revert_threshold,
            multiplier_ceiling:           config.multiplier_ceiling,
            multiplier_floor:             config.multiplier_floor,
            ceiling_escalation_threshold: config.ceiling_escalation_threshold,
            checkpoint_dir:               PathBuf::from(&config.checkpoint_dir),
            checkpoint_retention:         config.checkpoint_retention,
            total_losses:                 0,
            baseline_win_rate:            0.0,
            checkpoint:                   None,
            held_out:                     Vec::new(),
            paused:                       Arc::new(AtomicBool::new(false)),
            ceiling_hit_count:            Arc::new(AtomicU64::new(0)),
            health:                       None,
        }
    }

    /// Wire in the LossAttribution health layer.
    ///
    /// When set, ceiling escalation transitions the layer to DEGRADED
    /// rather than emitting only a tracing event.
    pub fn with_health(mut self, health: Arc<dyn LayerHealth>) -> Self {
        self.health = Some(health);
        self
    }

    /// Attempt to load the latest checkpoint from disk.
    ///
    /// On success, restores `fee_multipliers` and `baseline_win_rate`
    /// from the checkpoint.  Returns `Ok(true)` if a checkpoint was
    /// loaded, `Ok(false)` if none existed.
    pub fn try_load_checkpoint(&mut self) -> anyhow::Result<bool> {
        match checkpoint::load_latest(&self.checkpoint_dir)? {
            Some(ckpt) => {
                tracing::info!(
                    version      = ckpt.version,
                    win_rate     = ckpt.win_rate,
                    sample_count = ckpt.sample_count,
                    "Resumed gas model from checkpoint",
                );
                self.fee_multipliers   = ckpt.multipliers.clone();
                self.baseline_win_rate = ckpt.baseline_win_rate;
                self.checkpoint        = Some(ckpt);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    // â”€â”€ Public API â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Process one loss event.
    ///
    /// - If the model is paused (ceiling escalation), the event is
    ///   silently dropped.
    /// - 20% of events go to the holdout buffer (deterministic by hash).
    /// - 80% update the fee multiplier immediately.
    /// - Every `checkpoint_interval` events, `validate_and_checkpoint`
    ///   is called.
    pub fn on_loss(&mut self, loss: LossEvent) {
        if self.paused.load(Ordering::Acquire) {
            tracing::debug!(
                blueprint_hash = %loss.blueprint_hash,
                "Gas model paused â€” loss event dropped",
            );
            return;
        }

        self.total_losses += 1;

        if loss.is_holdout() {
            self.held_out.push(loss);
        } else {
            self.update_multiplier(loss);
        }

        if self.total_losses % self.checkpoint_interval == 0 {
            self.validate_and_checkpoint();
        }
    }

    /// Current fee multiplier for a `FeatureKey`.
    ///
    /// Returns 1.0 (neutral) for unseen keys.
    pub fn multiplier(&self, key: &FeatureKey) -> f64 {
        *self.fee_multipliers.get(key).unwrap_or(&1.0)
    }

    /// Whether the model is currently paused (Â§13.3).
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    /// Unpause the model after governance clearance
    /// (POST /api/v1/la/gas-model/unpause, Â§17.2).
    pub fn unpause(&self) {
        self.paused.store(false, Ordering::Release);
        self.ceiling_hit_count.store(0, Ordering::Release);
        tracing::info!("Gas model unpaused by governance");
    }

    // â”€â”€ Internal: multiplier update â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    fn update_multiplier(&mut self, loss: LossEvent) {
        if !loss.loss_code.affects_multiplier() {
            return;
        }

        let key = loss.feature_key();
        let m   = self.fee_multipliers.entry(key.clone()).or_insert(1.0);

        match loss.loss_code {
            LossCode::LostGasLow => {
                *m += self.learning_rate;

                // â”€â”€ Ceiling escalation check (Â§13.3, fix I5) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
                if *m >= self.multiplier_ceiling - 1e-6 {
                    let hits = self.ceiling_hit_count.fetch_add(1, Ordering::Relaxed) + 1;

                    if hits > self.ceiling_escalation_threshold {
                        tracing::error!(
                            feature_key  = %key.label(),
                            ceiling_hits = hits,
                            "GAS_MODEL_CEILING_ESCALATION â€” pausing model, L2 governance required",
                        );
                        self.paused.store(true, Ordering::SeqCst);

                        if let Some(ref health) = self.health {
                            health.set_state(
                                HealthState::Degraded,
                                &format!(
                                    "GAS_MODEL_CEILING_REACHED: {hits} consecutive \
                                     LOST_GAS_LOW at {:.1}Ã— ceiling for key {}",
                                    self.multiplier_ceiling,
                                    key.label(),
                                ),
                            );
                        }
                    }
                } else {
                    // Any non-ceiling update resets the consecutive counter
                    self.ceiling_hit_count.store(0, Ordering::Relaxed);
                }
            }

            LossCode::LostGasOverbid => {
                *m -= self.learning_rate;
                // Overbid resets ceiling hit streak
                self.ceiling_hit_count.store(0, Ordering::Relaxed);
            }

            _ => {}
        }

        *m = m.clamp(self.multiplier_floor, self.multiplier_ceiling);
    }

    // â”€â”€ Internal: validation and checkpoint â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    fn validate_and_checkpoint(&mut self) {
        let holdout_rate = self.compute_holdout_win_rate();

        if let Some(ref ckpt) = self.checkpoint {
            let degradation = ckpt.win_rate - holdout_rate;
            if degradation > self.revert_threshold {
                tracing::warn!(
                    checkpoint_version = ckpt.version,
                    checkpoint_rate    = ckpt.win_rate,
                    holdout_rate,
                    degradation_pct    = degradation * 100.0,
                    "GAS_MODEL_REVERTED â€” holdout degraded beyond threshold",
                );
                self.fee_multipliers = ckpt.multipliers.clone();
                // Do not save a new checkpoint after revert
                self.held_out.clear();
                return;
            }
        }

        // â”€â”€ Save new checkpoint â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
        let version  = self.total_losses / self.checkpoint_interval;
        let new_ckpt = ModelCheckpoint {
            version,
            win_rate:          holdout_rate,
            multipliers:       self.fee_multipliers.clone(),
            saved_at:          chrono::Utc::now(),
            sample_count:      self.total_losses,
            baseline_win_rate: self.baseline_win_rate,
        };

        match checkpoint::save(&new_ckpt, &self.checkpoint_dir, self.checkpoint_retention) {
            Ok(()) => {
                tracing::info!(
                    version      = version,
                    win_rate     = holdout_rate,
                    sample_count = self.total_losses,
                    "Gas model checkpoint saved",
                );
                self.checkpoint = Some(new_ckpt);
            }
            Err(e) => {
                tracing::error!(error = %e,
                    "Failed to save gas model checkpoint â€” continuing");
            }
        }

        self.held_out.clear();
    }

    // â”€â”€ Internal: holdout win rate â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Evaluate the current model against the holdout buffer.
    ///
    /// ## Win criteria per loss code
    ///
    ///   LostGasLow:    model multiplier Ã— our_fee > competing_fee (if known)
    ///                  or multiplier > 1.0 (if competing fee unknown).
    ///   LostGasOverbid: model multiplier â‰¤ 1.0 (no overbid with this model).
    ///   Other:          always counted as "win" (non-attributable to fee).
    ///
    /// Returns `baseline_win_rate` when the holdout buffer is empty.
    fn compute_holdout_win_rate(&self) -> f64 {
        if self.held_out.is_empty() {
            return self.baseline_win_rate;
        }

        let wins: u64 = self
            .held_out
            .iter()
            .map(|event| {
                let key = event.feature_key();
                let m   = *self.fee_multipliers.get(&key).unwrap_or(&1.0);

                let win = match event.loss_code {
                    LossCode::LostGasLow => {
                        let adjusted = event.our_fee_gwei as f64 * m;
                        match event.competing_fee_gwei {
                            Some(comp) => adjusted > comp as f64,
                            None       => m > 1.0,
                        }
                    }
                    LossCode::LostGasOverbid => m <= 1.0,
                    _ => true,
                };
                u64::from(win)
            })
            .sum();

        wins as f64 / self.held_out.len() as f64
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use chrono::Utc;
    use omega_core::MlConfig;

    fn make_learner() -> GasModelOnlineLearner {
        GasModelOnlineLearner::from_config(&MlConfig::default())
    }

    fn make_event(hash_byte: u8, code: LossCode, our_fee: u64, comp: Option<u64>) -> LossEvent {
        let mut h = B256::ZERO;
        h.0[0] = hash_byte;
        LossEvent {
            blueprint_hash:       h,
            loss_code:            code,
            our_fee_gwei:         our_fee,
            competing_fee_gwei:   comp,
            asset:                "WETH".into(),
            protocol:             "aave_v3".into(),
            health_factor:        1.005,
            liquidation_size_eth: 50.0,
            timestamp:            Utc::now(),
        }
    }

    // Training-set event (byte % 5 != 0)
    fn train(code: LossCode) -> LossEvent { make_event(1, code, 100, Some(110)) }
    // Holdout event (byte % 5 == 0)
    fn holdout(code: LossCode) -> LossEvent { make_event(0, code, 100, Some(110)) }

    // â”€â”€ Multiplier updates â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn lost_gas_low_raises_multiplier() {
        let mut l = make_learner();
        let key   = train(LossCode::LostGasLow).feature_key();
        l.on_loss(train(LossCode::LostGasLow));
        assert!(l.multiplier(&key) > 1.0, "multiplier must increase on LOST_GAS_LOW");
    }

    #[test]
    fn lost_gas_overbid_lowers_multiplier() {
        let mut l = make_learner();
        let key   = train(LossCode::LostGasOverbid).feature_key();
        *l.fee_multipliers.entry(key.clone()).or_insert(1.0) = 1.05;
        l.on_loss(train(LossCode::LostGasOverbid));
        assert!(l.multiplier(&key) < 1.05, "multiplier must decrease on LOST_GAS_OVERBID");
    }

    #[test]
    fn non_gas_losses_do_not_change_multiplier() {
        let mut l  = make_learner();
        let key    = train(LossCode::LostLatency).feature_key();
        let before = l.multiplier(&key);
        l.on_loss(train(LossCode::LostLatency));
        assert!((l.multiplier(&key) - before).abs() < 1e-9);
    }

    #[test]
    fn multiplier_clamped_to_floor() {
        let mut l = make_learner();
        let key   = train(LossCode::LostGasOverbid).feature_key();
        *l.fee_multipliers.entry(key.clone()).or_insert(1.0) = 0.3 + 1e-10;
        l.on_loss(train(LossCode::LostGasOverbid));
        assert!(l.multiplier(&key) >= l.multiplier_floor);
    }

    #[test]
    fn multiplier_clamped_to_ceiling() {
        let mut l = make_learner();
        let key   = train(LossCode::LostGasLow).feature_key();
        *l.fee_multipliers.entry(key.clone()).or_insert(1.0) = 5.0 - 1e-10;
        l.on_loss(train(LossCode::LostGasLow));
        assert!(l.multiplier(&key) <= l.multiplier_ceiling);
    }

    // â”€â”€ Paused model â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn paused_model_ignores_losses() {
        let mut l = make_learner();
        let key   = train(LossCode::LostGasLow).feature_key();
        l.paused.store(true, Ordering::SeqCst);
        l.on_loss(train(LossCode::LostGasLow));
        assert_eq!(l.total_losses, 0, "paused model must not count losses");
        assert!((l.multiplier(&key) - 1.0).abs() < 1e-9,
            "paused model must not update multiplier");
    }

    #[test]
    fn unpause_resets_ceiling_count() {
        let l = make_learner();
        l.ceiling_hit_count.store(50, Ordering::SeqCst);
        l.paused.store(true, Ordering::SeqCst);
        l.unpause();
        assert!(!l.is_paused());
        assert_eq!(l.ceiling_hit_count.load(Ordering::SeqCst), 0);
    }

    // â”€â”€ Holdout split â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn holdout_events_go_to_held_out_buffer() {
        let mut l = make_learner();
        l.on_loss(holdout(LossCode::LostGasLow));
        assert_eq!(l.held_out.len(), 1);
        assert_eq!(l.total_losses,   1);
    }

    #[test]
    fn training_events_do_not_go_to_holdout() {
        let mut l = make_learner();
        l.on_loss(train(LossCode::LostGasLow));
        assert_eq!(l.held_out.len(), 0);
    }

    // â”€â”€ Win rate computation â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn holdout_win_rate_empty_returns_baseline() {
        let mut l = make_learner();
        l.baseline_win_rate = 0.65;
        assert!((l.compute_holdout_win_rate() - 0.65).abs() < 1e-9);
    }

    #[test]
    fn holdout_win_rate_lost_gas_low_with_multiplier() {
        let mut l = make_learner();
        // our_fee=100, comp=110 â†’ need multiplier > 1.1 to win
        let event = make_event(0, LossCode::LostGasLow, 100, Some(110));
        let key   = event.feature_key();
        // 100 Ã— 1.2 = 120 > 110 â†’ win
        *l.fee_multipliers.entry(key).or_insert(1.0) = 1.2;
        l.held_out.push(event);
        assert!((l.compute_holdout_win_rate() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn holdout_win_rate_lost_gas_low_insufficient_multiplier() {
        let mut l = make_learner();
        // our_fee=100, comp=110 â†’ 100 Ã— 1.05 = 105 < 110 â†’ loss
        let event = make_event(0, LossCode::LostGasLow, 100, Some(110));
        let key   = event.feature_key();
        *l.fee_multipliers.entry(key).or_insert(1.0) = 1.05;
        l.held_out.push(event);
        assert!((l.compute_holdout_win_rate() - 0.0).abs() < 1e-9);
    }
}