// crates/omega-observability/src/events.rs
//
// OmegaEvent — canonical tagged union of all engine telemetry events (§16).
//
// ## Spec §16 — always-sampled events
//
//   LA events: always-sampled (100% sampling rate), high-priority log channel.
//   All other events: configurable rate, default 100%.
//
//   New v12 events added in spec §16:
//     GasModelReverted         — checkpoint revert after holdout degradation
//     GasModelCeilingEscalation — model paused after 100 consecutive ceiling hits
//     EmergencyBundleSkipped   — emergency bundle not emitted (profit check)
//     ProfitSplit              — DAO fee allocation on every Vault release
//
// ## Serialisation
//
//   Every variant serialises with `kind` as the tag discriminant and
//   `timestamp` as a top-level field (ISO-8601).  This schema is stable
//   across versions — new variants are additive only.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// OmegaEvent
// ─────────────────────────────────────────────────────────────────────────────

/// Canonical telemetry event emitted by any Omega Engine layer.
///
/// All variants are JSON-serialisable.  The `kind` field is the serde tag
/// discriminant and doubles as the ELK index routing key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OmegaEvent {
    // ── Health FSM (§3) ────────────────────────────────────────────────────
    /// A health layer transitioned between FSM states.
    HealthStateChange {
        timestamp: DateTime<Utc>,
        layer_id: String,
        from_state: String,
        to_state: String,
        reason: String,
    },

    /// System-wide emergency halt was issued.
    EmergencyHalt {
        timestamp: DateTime<Utc>,
        issuer: String,
        reason: String,
    },

    // ── Blueprint lifecycle (§13) ──────────────────────────────────────────
    /// A blueprint was admitted to the DAG.
    BlueprintAdmitted {
        timestamp: DateTime<Utc>,
        blueprint_hash: String,
        strategy_id: String,
        lane: String,
        chain_id: u64,
    },

    /// A blueprint was dropped before relay submission.
    BlueprintDropped {
        timestamp: DateTime<Utc>,
        blueprint_hash: String,
        strategy_id: String,
        drop_code: String,
        chain_id: u64,
    },

    /// A blueprint was submitted to a relay.
    BlueprintSubmitted {
        timestamp: DateTime<Utc>,
        blueprint_hash: String,
        strategy_id: String,
        relay: String,
        fee_gwei: u64,
        chain_id: u64,
    },

    /// A blueprint was confirmed on-chain.
    BlueprintConfirmed {
        timestamp: DateTime<Utc>,
        blueprint_hash: String,
        strategy_id: String,
        block_number: u64,
        profit_net_eth: f64,
        chain_id: u64,
    },

    // ── LA (§11, §13, §16) — always-sampled ───────────────────────────────
    /// A position was detected as liquidatable.
    LaPositionDetected {
        timestamp: DateTime<Utc>,
        borrower: String,
        protocol: String,
        hf_e18: String,
        collateral_usd: f64,
        debt_usd: f64,
        chain_id: u64,
    },

    /// A liquidation bundle was submitted (cascade mode).
    LaBundleSubmitted {
        timestamp: DateTime<Utc>,
        blueprint_hash: String,
        relay: String,
        fee_gwei: u64,
        bundle_index: u8,
        total_bundles: u8,
    },

    /// The emergency bundle was skipped — profit check failed (§12.1, fix M2).
    EmergencyBundleSkipped {
        timestamp: DateTime<Utc>,
        blueprint_hash: String,
        emergency_fee_gwei: u64,
        reason: String,
    },

    /// Sequencer restart reorg risk detected on a submitted blueprint (§11.4).
    LaReorgRisk {
        timestamp: DateTime<Utc>,
        tx_hash: String,
        orphaned_block: u64,
        rescore_at: u64,
    },

    // ── Gas model ML (§13, §16) ────────────────────────────────────────────
    /// Gas model reverted to a checkpoint after holdout degradation (fix C1).
    GasModelReverted {
        timestamp: DateTime<Utc>,
        checkpoint_version: u64,
        checkpoint_rate: f64,
        holdout_rate: f64,
        degradation_pct: f64,
    },

    /// Gas model paused — multiplier at ceiling for too many consecutive
    /// losses (§13.3, fix I5).
    GasModelCeilingEscalation {
        timestamp: DateTime<Utc>,
        feature_key: String,
        ceiling_hits: u64,
        threshold: u64,
    },

    // ── Vault / DAO (§15, §16) ─────────────────────────────────────────────
    /// Profit released from the Vault with DAO fee split (§15.1).
    ProfitSplit {
        timestamp: DateTime<Utc>,
        blueprint_hash: String,
        pil_share_eth: f64,
        dao_fee_eth: f64,
        dao_fee_address: String,
        chain_id: u64,
    },

    // ── Oracle (§6, §16) ───────────────────────────────────────────────────
    /// Oracle price resolved successfully.
    OraclePriceResolved {
        timestamp: DateTime<Utc>,
        asset: String,
        price_usd: f64,
        source: String,
        age_seconds: u64,
        chain_id: u64,
    },

    /// Oracle divergence detected between two feeds.
    OracleDiverge {
        timestamp: DateTime<Utc>,
        asset: String,
        price_primary: f64,
        price_secondary: f64,
        diverge_bps: u64,
        chain_id: u64,
    },

    // ── Builder blacklist (§12.3) ──────────────────────────────────────────
    /// Builder blacklist was hot-reloaded.
    BlacklistReloaded {
        timestamp: DateTime<Utc>,
        entry_count: usize,
        path: String,
    },
}

impl OmegaEvent {
    fn is_la_blueprint_event(&self) -> bool {
        match self {
            OmegaEvent::BlueprintAdmitted { strategy_id, .. }
            | OmegaEvent::BlueprintDropped { strategy_id, .. }
            | OmegaEvent::BlueprintSubmitted { strategy_id, .. }
            | OmegaEvent::BlueprintConfirmed { strategy_id, .. } => {
                strategy_id.eq_ignore_ascii_case("LA")
            }
            _ => false,
        }
    }

    /// UTC timestamp of this event.
    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            OmegaEvent::HealthStateChange { timestamp, .. } => *timestamp,
            OmegaEvent::EmergencyHalt { timestamp, .. } => *timestamp,
            OmegaEvent::BlueprintAdmitted { timestamp, .. } => *timestamp,
            OmegaEvent::BlueprintDropped { timestamp, .. } => *timestamp,
            OmegaEvent::BlueprintSubmitted { timestamp, .. } => *timestamp,
            OmegaEvent::BlueprintConfirmed { timestamp, .. } => *timestamp,
            OmegaEvent::LaPositionDetected { timestamp, .. } => *timestamp,
            OmegaEvent::LaBundleSubmitted { timestamp, .. } => *timestamp,
            OmegaEvent::EmergencyBundleSkipped { timestamp, .. } => *timestamp,
            OmegaEvent::LaReorgRisk { timestamp, .. } => *timestamp,
            OmegaEvent::GasModelReverted { timestamp, .. } => *timestamp,
            OmegaEvent::GasModelCeilingEscalation { timestamp, .. } => *timestamp,
            OmegaEvent::ProfitSplit { timestamp, .. } => *timestamp,
            OmegaEvent::OraclePriceResolved { timestamp, .. } => *timestamp,
            OmegaEvent::OracleDiverge { timestamp, .. } => *timestamp,
            OmegaEvent::BlacklistReloaded { timestamp, .. } => *timestamp,
        }
    }

    /// Whether this event is always-sampled (100%) regardless of the
    /// configured sampling rate (§16).
    pub fn is_always_sampled(&self) -> bool {
        if self.is_la_blueprint_event() {
            return true;
        }

        matches!(
            self,
            OmegaEvent::LaPositionDetected { .. }
                | OmegaEvent::LaBundleSubmitted { .. }
                | OmegaEvent::EmergencyBundleSkipped { .. }
                | OmegaEvent::LaReorgRisk { .. }
                | OmegaEvent::GasModelReverted { .. }
                | OmegaEvent::GasModelCeilingEscalation { .. }
                | OmegaEvent::ProfitSplit { .. }
                | OmegaEvent::EmergencyHalt { .. }
        )
    }

    /// ELK index routing key derived from the event kind.
    ///
    /// Pattern: `omega-{category}-{date}` — matches the ELK hot/warm
    /// index lifecycle configured in §16.
    pub fn elk_index(&self) -> &'static str {
        match self {
            OmegaEvent::HealthStateChange { .. } | OmegaEvent::EmergencyHalt { .. } => {
                "omega-health"
            }
            OmegaEvent::BlueprintAdmitted { .. }
            | OmegaEvent::BlueprintDropped { .. }
            | OmegaEvent::BlueprintSubmitted { .. }
            | OmegaEvent::BlueprintConfirmed { .. } => "omega-blueprint",
            OmegaEvent::LaPositionDetected { .. }
            | OmegaEvent::LaBundleSubmitted { .. }
            | OmegaEvent::EmergencyBundleSkipped { .. }
            | OmegaEvent::LaReorgRisk { .. } => "omega-la",
            OmegaEvent::GasModelReverted { .. } | OmegaEvent::GasModelCeilingEscalation { .. } => {
                "omega-gas-model"
            }
            OmegaEvent::ProfitSplit { .. } => "omega-vault",
            OmegaEvent::OraclePriceResolved { .. } | OmegaEvent::OracleDiverge { .. } => {
                "omega-oracle"
            }
            OmegaEvent::BlacklistReloaded { .. } => "omega-security",
        }
    }
}
