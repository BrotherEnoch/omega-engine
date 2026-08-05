// crates/omega-core/src/types/signal.rs
//
// Signal types — the data contract between the oracle layer (omega-oracle)
// and the strategy layer (omega-strategies).
//
// Signals are the market observations that drive strategy scoring.
// They cross the EIL (Execution Isolation Layer) double-buffer boundary
// (§6) via a lock-free arc-swap channel.  Every signal carries a
// monotonic `state_version` and a `state_hash` so the EIL can detect
// stale state before permitting simulation (§6, §13.4).
//
// ## Audit finding fixed in this pass
//
// Nothing here previously handled clock skew or out-of-order arrival:
// a naive `now - signal.received_at_unix_ms` computed by a downstream
// consumer is a real `u64` underflow risk if `received_at_unix_ms` is,
// for any reason (clock skew between hosts, an out-of-order/replayed
// signal), later than the caller's `now` — that PANICS in a debug build
// and silently wraps to a huge, wrong value in release, either of which
// is a worse outcome than reporting age zero. Added `age_ms()` with
// saturating subtraction as the canonical way to compute this.
//
// ## Design note, not changed in this pass
//
// `payload: serde_json::Value` is untyped by design (documented
// per-SignalKind schemas in a comment, not enforced by the type
// system). This means ANY valid JSON deserializes successfully
// regardless of whether its shape matches its `kind` — a malformed or
// wrong-shaped payload is only caught later, wherever each strategy
// crate parses its own payload, and if that parsing code isn't
// defensive (e.g. `.unwrap()`s a missing field instead of returning an
// error), a malformed signal could propagate further than ideal or
// panic a strategy's scoring path. A stricter fix — replacing this
// with a tagged enum (`SignalPayload::SpotPrice { .. }`, etc.) so serde
// enforces the shape at deserialize time — would close that gap
// structurally, but it's a bigger change than this pass should make
// blind, since every consumer of `OracleSignal::payload` across the
// workspace would need updating and none of that code is visible from
// omega-core. Flagged here rather than silently reshaped.
//
// Spec references:
//   §2   — L0/L1 channel map (oracle → EIL → strategy)
//   §6   — EIL double-buffer; state_version staleness detection
//   §10  — MSA Bellman-Ford signal (PriceSignal::PoolReserves)
//   §11  — LA position health factor (PriceSignal::HealthFactor)
//   §13  — signal_state_hash is the join key for loss attribution

use alloy_primitives::B256;
use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// SignalKind
// ─────────────────────────────────────────────────────────────────────────────

/// Discriminates the market signal type carried in an [`OracleSignal`].
///
/// Used by strategy routers to filter signals relevant to their
/// execution path without deserialising the full payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    /// Spot price update from a DEX oracle (Uniswap v3 TWAP, Chainlink,
    /// Pyth).  Relevant to SA (Phase 1) and MSA (Phase 2).
    SpotPrice,

    /// AMM pool reserve update.  Used by MSA Bellman-Ford graph
    /// construction (§10).
    PoolReserves,

    /// Lending position health factor update.  The primary signal for
    /// LA hot/warm/cold tier classification (§11.1).
    HealthFactor,

    /// On-chain mempool / order-flow signal.  Used by MEV-OFA (Phase 4).
    OrderFlow,

    /// Block-level fee oracle update (base fee, L1 data fee, priority
    /// fee market).  Fed into the dual-component gas model (§7).
    FeeOracle,
}

// ─────────────────────────────────────────────────────────────────────────────
// OracleSignal
// ─────────────────────────────────────────────────────────────────────────────

/// A single oracle observation delivered to the strategy layer.
///
/// The EIL double-buffer swaps a `Arc<Vec<OracleSignal>>` snapshot
/// atomically — strategies receive a consistent set of signals for the
/// same `state_version` (§6).
///
/// ## Versioning invariant
///
/// `state_version` strictly increases per chain.  A strategy that
/// receives a signal with `state_version ≤ blueprint.state_version`
/// must treat its blueprint as stale and abort submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleSignal {
    /// Discriminant — lets routers skip irrelevant signals cheaply.
    pub kind: SignalKind,

    /// EIP-155 chain ID this signal originates from.
    pub chain_id: u64,

    /// Block number at which this signal was observed on-chain.
    pub block_number: u64,

    /// Timestamp (Unix seconds) when the oracle layer received this
    /// signal.  Used for latency tracking in the Observability layer (§16).
    pub received_at_unix_ms: u64,

    /// Monotonically increasing state snapshot version (per chain).
    /// Shared with [`crate::types::strategy::SignalState::state_version`].
    pub state_version: u64,

    /// keccak256 of the canonical serialised oracle state at this
    /// snapshot.  Recorded on blueprints as `signal_state_hash` and
    /// validated by the EIL before simulation (§6).
    pub state_hash: B256,

    /// Signal payload — strategy-specific.  Encoded as JSON for
    /// cross-crate flexibility; each strategy deserialises into its
    /// own domain types.
    ///
    /// Payload schemas per SignalKind:
    ///   SpotPrice    → `{ "token": "0x…", "price_usd_e18": "…" }`
    ///   PoolReserves → `{ "pool": "0x…", "reserve0": "…", "reserve1": "…" }`
    ///   HealthFactor → `{ "position": "0x…", "hf_e18": "…", "protocol": "aave_v3" }`
    ///   OrderFlow    → `{ "tx_hash": "0x…", "decoded_swap": { … } }`
    ///   FeeOracle    → `{ "base_fee_gwei": N, "l1_data_fee_gwei": N, "priority_fee_gwei": N }`
    pub payload: serde_json::Value,
}

impl OracleSignal {
    /// Age of this signal in milliseconds relative to `now_unix_ms`.
    ///
    /// Saturates to 0 rather than underflowing/panicking if
    /// `received_at_unix_ms` is somehow in the future relative to
    /// `now_unix_ms` (clock skew between the oracle-layer host and the
    /// caller, or an out-of-order/replayed signal) — a naive `now -
    /// self.received_at_unix_ms` would panic on underflow in a debug
    /// build and silently wrap to a huge, wrong value in release,
    /// either of which is worse than reporting age 0 for a signal that,
    /// from its own timestamp's perspective, isn't stale at all.
    ///
    /// This crate deliberately does not hardcode a staleness threshold
    /// here — that belongs to whichever config governs the consuming
    /// layer (e.g. an `OmegaConfig`-owned max-age setting), not to the
    /// signal type itself. This method supplies the safe age
    /// computation; the threshold comparison is the caller's job.
    #[inline]
    pub fn age_ms(&self, now_unix_ms: u64) -> u64 {
        now_unix_ms.saturating_sub(self.received_at_unix_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_signal(received_at_unix_ms: u64) -> OracleSignal {
        OracleSignal {
            kind: SignalKind::SpotPrice,
            chain_id: 42161,
            block_number: 100,
            received_at_unix_ms,
            state_version: 1,
            state_hash: B256::ZERO,
            payload: serde_json::json!({ "token": "0x0", "price_usd_e18": "1" }),
        }
    }

    #[test]
    fn age_ms_normal_case() {
        let s = sample_signal(1_000);
        assert_eq!(s.age_ms(1_500), 500);
    }

    #[test]
    fn age_ms_zero_when_now_equals_received() {
        let s = sample_signal(1_000);
        assert_eq!(s.age_ms(1_000), 0);
    }

    #[test]
    fn age_ms_saturates_instead_of_underflowing() {
        // received_at is AFTER "now" — clock skew / out-of-order arrival.
        // A naive subtraction here would panic (debug) or wrap to a huge
        // u64 (release); age_ms must instead report 0.
        let s = sample_signal(2_000);
        assert_eq!(s.age_ms(1_000), 0, "must saturate to 0, not underflow");
    }
}
