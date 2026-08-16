// crates/omega-security/src/exposure.rs
//
// Per-strategy account exposure tracker — backs `CheckContext::
// current_account_exposure_wei` for `omega_risk::checks::
// check_account_exposure` (check 14, `DropCode::MissExposureLimit`).
//
// ## What this tracks
//
// Check 14 compares "capital already at risk from admitted-but-not-yet-
// resolved blueprints" (`current_account_exposure_wei`) plus this
// blueprint's own `flashloan_amount` against a configured cap. Nothing
// in this codebase tracked the first quantity before this file — every
// prior revision's `CheckContext::current_account_exposure_wei` was
// `u128::MAX`, an unconditional fail-closed sentinel, not a real value.
//
// ## Design: TTL-by-expiry-block, not a release hook
//
// The natural signal for "this blueprint is no longer outstanding" is
// its DAG slot being released — but that release happens inside
// `omega_execution::pipeline::DagSlotGuard`, a different crate's
// private mechanism, with no event this crate (or `src/main.rs`, which
// owns this tracker) currently observes. Hooking that cleanly would
// mean changing `omega-execution`'s public surface specifically to
// expose a release callback — a real option, but a cross-crate design
// change, not something to do blind inside a same-session pass on a
// different crate.
//
// Instead, this tracker uses each blueprint's own real `expiry_block`
// field (already present on `ExecutionBlueprint`, already the same
// value check 2 / `MissExpiry` enforces) as a TTL: an admitted
// blueprint's `flashloan_amount` counts as outstanding exposure until
// its own expiry block passes, then `current_exposure_wei` prunes it
// automatically on the next read.
//
// This is a CONSERVATIVE APPROXIMATION, not a reconciled true value:
//   - It can OVERCOUNT — a blueprint that confirms on-chain well before
//     its nominal expiry still counts as "exposed" until that expiry
//     block, even though the capital may already be safely returned.
//   - It CANNOT UNDERCOUNT relative to what it knows about — every
//     recorded entry is counted until its own stated expiry, never
//     dropped early.
// Overcounting exposure is the safe error direction for a risk cap
// (rejects more than strictly necessary); undercounting would not be.
// A real reconciled tracker (driven by actual on-chain confirmation,
// e.g. via Stage 7 reconciliation once that exists) would replace this
// with something tighter, not looser.
//
// ## Scope
//
// Keyed by `scope: &str` — this file follows the exact same convention
// `KillSwitchRegistry`/`NonceRegistry` already use elsewhere in this
// workspace (`strategy_id.to_string()`, per `src/main.rs`'s existing
// call sites for both), not a separate global-account concept that
// doesn't exist anywhere else in this codebase.
//
// ## Persistence
//
// In-memory only — resets on process restart, same as `NonceRegistry`
// and `KillSwitchRegistry`. A strategy that goes permanently inactive
// leaves a (typically already-empty, since entries prune on read) entry
// behind for the life of the process; not addressed here, same category
// of known limitation as those two types' own eviction gaps.

use std::sync::Arc;

use dashmap::DashMap;

/// One recorded flashloan exposure, pruned once `expiry_block` has
/// passed. `expiry_block` uses the same "expired when current_block >=
/// expiry_block" convention as `omega_risk::checks::check_expiry`
/// (check 2) — i.e. an entry is still counted while
/// `current_block < expiry_block`, matching how the blueprint that
/// produced it would itself still be considered non-expired.
#[derive(Debug, Clone, Copy)]
struct ExposureEntry {
    amount_wei: u128,
    expiry_block: u64,
}

/// Per-strategy account exposure tracker (check 14 input).
///
/// Internally `Arc<DashMap<..>>`-backed and `Clone`, same cheap-clone
/// pattern `omega_security::replay::NonceRegistry` already establishes
/// — no external `Arc` wrap needed at call sites.
#[derive(Clone)]
pub struct AccountExposureTracker {
    entries: Arc<DashMap<String, Vec<ExposureEntry>>>,
}

impl AccountExposureTracker {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(DashMap::new()),
        }
    }

    /// Record a newly-admitted blueprint's flashloan exposure.
    ///
    /// `amount_wei == 0` (the common case today — SA/MSA/MEV never use
    /// flashloans by design, not as a placeholder) is a deliberate
    /// no-op: recording a zero-exposure entry would only grow the
    /// tracker's memory for every non-flashloan blueprint with nothing
    /// to show for it, since it contributes 0 to every future sum
    /// regardless.
    pub fn record(&self, scope: &str, amount_wei: u128, expiry_block: u64) {
        if amount_wei == 0 {
            return;
        }
        self.entries
            .entry(scope.to_string())
            .or_default()
            .push(ExposureEntry {
                amount_wei,
                expiry_block,
            });
    }

    /// Current outstanding exposure for `scope`, in wei — the value
    /// `CheckContext::current_account_exposure_wei` should be set to.
    ///
    /// Prunes expired entries (those with `expiry_block <=
    /// current_block`) as a side effect of reading, so the tracker's
    /// memory stays bounded by "how many blueprints are within their
    /// expiry window right now," not by total historical volume. Given
    /// every strategy's `*_EXPIRY_BLOCKS` constant is 1-2 blocks (a
    /// fraction of a second on Arbitrum), this window is small in
    /// practice.
    ///
    /// Returns `0` for a scope that has never recorded a nonzero-amount
    /// entry (e.g. every strategy today except LA) — correctly, not as
    /// a fail-open shortcut: zero real exposure is the honest value for
    /// a strategy that has never taken on flashloan principal.
    pub fn current_exposure_wei(&self, scope: &str, current_block: u64) -> u128 {
        match self.entries.get_mut(scope) {
            Some(mut entries) => {
                entries.retain(|e| e.expiry_block > current_block);
                entries
                    .iter()
                    .fold(0u128, |acc, e| acc.saturating_add(e.amount_wei))
            }
            None => 0,
        }
    }
}

impl Default for AccountExposureTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unseen_scope_has_zero_exposure() {
        let t = AccountExposureTracker::new();
        assert_eq!(t.current_exposure_wei("SA", 100), 0);
    }

    #[test]
    fn zero_amount_is_not_recorded() {
        let t = AccountExposureTracker::new();
        t.record("SA", 0, 200);
        assert_eq!(
            t.current_exposure_wei("SA", 100), 0,
            "a zero-amount blueprint contributes nothing and shouldn't grow the tracker"
        );
        assert!(
            t.entries.get("SA").is_none(),
            "recording a zero amount must not create an entry at all"
        );
    }

    #[test]
    fn single_entry_counted_before_expiry() {
        let t = AccountExposureTracker::new();
        t.record("LA", 1_000, 105);
        assert_eq!(t.current_exposure_wei("LA", 100), 1_000);
        assert_eq!(
            t.current_exposure_wei("LA", 104),
            1_000,
            "still counted one block before expiry"
        );
    }

    #[test]
    fn entry_pruned_at_and_after_expiry() {
        let t = AccountExposureTracker::new();
        t.record("LA", 1_000, 105);
        assert_eq!(
            t.current_exposure_wei("LA", 105),
            0,
            "current_block == expiry_block must be treated as expired, matching \
             check_expiry's own current_block >= expiry_block convention"
        );
        // Re-record and confirm it stays pruned further past expiry too.
        t.record("LA", 500, 105);
        assert_eq!(t.current_exposure_wei("LA", 200), 0);
    }

    #[test]
    fn multiple_entries_sum_correctly() {
        let t = AccountExposureTracker::new();
        t.record("LA", 1_000, 110);
        t.record("LA", 2_000, 120);
        t.record("LA", 3_000, 90); // already expired relative to read below
        assert_eq!(
            t.current_exposure_wei("LA", 100),
            3_000,
            "only the two non-expired entries (1000 + 2000) should count"
        );
    }

    #[test]
    fn scopes_are_independent() {
        let t = AccountExposureTracker::new();
        t.record("LA", 1_000, 200);
        t.record("MEV", 5_000, 200);
        assert_eq!(t.current_exposure_wei("LA", 100), 1_000);
        assert_eq!(t.current_exposure_wei("MEV", 100), 5_000);
    }

    #[test]
    fn sum_saturates_on_overflow_rather_than_panicking() {
        let t = AccountExposureTracker::new();
        t.record("LA", u128::MAX - 10, 200);
        t.record("LA", 1_000, 200);
        assert_eq!(
            t.current_exposure_wei("LA", 100),
            u128::MAX,
            "must saturate, not panic or wrap, on pathological input"
        );
    }

    #[test]
    fn reading_prunes_even_without_new_recordings() {
        let t = AccountExposureTracker::new();
        t.record("SA", 1_000, 105);
        assert_eq!(t.current_exposure_wei("SA", 50), 1_000);
        // Read again, well past expiry, with no intervening record() —
        // confirms pruning happens at READ time, not only on write.
        assert_eq!(t.current_exposure_wei("SA", 500), 0);
    }
}