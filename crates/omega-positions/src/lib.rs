// crates/omega-positions/src/lib.rs
//
// omega-positions — Live lending-position registry for LA scoring (spec §11).
//
// ## Overview
//
// `PositionRegistry` mirrors `omega_flashloan::LiquidityRegistry`'s shape
// (DashMap-backed, Arc-shared, written by omega-oracle background tasks,
// read synchronously by strategy blueprint construction, no I/O
// performed inside this crate) but for a distinct real-world domain:
// per-borrower lending-position health (`PositionSnapshot`, omega-core),
// not flashloan provider liquidity/premiums.
//
// ## Why a separate crate from omega-flashloan
//
// Both ultimately serve spec §11 (Liquidation Arbitrage) — omega-flashloan's
// own module doc comment cites §11 too. But omega-flashloan's documented
// scope is specifically "Flashloan provider registry and premium
// calculator": provider selection and premium math. Position health
// tracking is a different real-world data source (per-position
// on-chain reads — Aave `getUserAccountData` and equivalents — plus a
// separate per-reserve/event-level token-identity source; see
// `omega_core::types::oracle::PositionTokens`'s own doc comment) that
// omega-flashloan's header never claimed to cover. Folding it in would
// be scope creep on a crate whose name and documented purpose are about
// something else. This crate exists so that distinction stays visible,
// the same way `PositionTokens` was kept separate from
// `PositionFinancials` in omega-core for an analogous reason.
//
// ## Staleness — deliberately NOT modeled after LiquidityRegistry's
//
// `omega_flashloan::LiquiditySnapshot::is_fresh()` uses a flat
// wall-clock threshold (`LIQUIDITY_STALE_SECS = 30`s). `PositionSnapshot`'s
// own doc comment (omega-core) documents a DIFFERENT freshness model: a
// blueprint built from a snapshot "is only valid while the snapshot's
// `block_number` is within the revm trust window (§6)" — a block-based
// window, not a time-based one. The actual §6 trust-window size has not
// been supplied to this crate, so NO staleness enforcement is
// implemented here. `PositionRegistry` only stores and returns
// `block_number` alongside each snapshot (already a `PositionSnapshot`
// field); whatever already implements the real §6 trust-window check is
// responsible for deciding whether a given snapshot is still
// trustworthy at the point it's used. Inventing a threshold here would
// fabricate a rule this domain may not actually use — contrast with
// `LIQUIDITY_STALE_SECS`, which IS a real, derived, owned constant in
// its own crate.
//
// ## Key shape
//
// `PositionSnapshot` carries no `chain_id` field — the same gap
// `omega_flashloan::LiquiditySnapshot` has for provider liquidity, which
// is why `omega_flashloan::ProviderKey` carries `chain_id` explicitly
// rather than relying on the snapshot itself. `PositionKey` below does
// the same, for the same reason: this registry should not assume
// single-chain operation any more than `LiquidityRegistry` does.
//
// `PositionSnapshot::dedup_key()` (a `String`, hashing `borrower ++
// protocol` only — deliberately excluding `block_number`, per its own
// doc comment, for the sequencer-restart DashMap's 60-block dedup
// window) is NOT reused as this registry's key: that existing mechanism
// solves a different problem (restart dedup) from this one (live
// lookup by identity), and it also excludes `chain_id` the same way the
// snapshot itself does — reusing it here would lose the
// chain-disambiguation this crate's `PositionKey` explicitly provides.
//
// ## Selection ordering
//
// `liquidatable_positions` returns positions sorted ascending by health
// factor (lowest / most urgent first) — the position-tracking analogue
// of `LiquidityRegistry::available_contracts`'s "highest liquidity
// first" ordering, adapted to this domain's own urgency signal (per
// `LaTier`'s documented tiering: Hot positions need recomputation
// "every oracle update — immediate", i.e. they are the most
// time-sensitive tier). This ordering is a DESIGN CHOICE made in this
// crate, not a value derived from anything in the spec sections shown
// to this crate's author — flag for review against whatever real LA
// position-selection policy exists before relying on it in production.

use std::sync::Arc;

use alloy_primitives::Address;
use dashmap::DashMap;

use omega_core::types::oracle::PositionSnapshot;

// ─────────────────────────────────────────────────────────────────────────────
// PositionKey
// ─────────────────────────────────────────────────────────────────────────────

/// Composite key for the position registry: (chain_id, borrower, protocol).
///
/// `PositionSnapshot` itself carries no `chain_id` (same gap
/// `omega_flashloan::LiquiditySnapshot` has for provider liquidity) —
/// this struct supplies it explicitly so one registry instance can
/// safely track positions across multiple chains without collision,
/// mirroring `omega_flashloan::ProviderKey`'s identical role.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PositionKey {
    chain_id: u64,
    borrower: Address,
    protocol: Address,
}

// ─────────────────────────────────────────────────────────────────────────────
// PositionRegistry
// ─────────────────────────────────────────────────────────────────────────────

/// Concurrent map of live lending-position snapshots, keyed by
/// `(chain_id, borrower, protocol)`.
///
/// Written by omega-oracle background tasks from real on-chain position
/// reads; read synchronously by LA's blueprint construction path. All
/// reads are O(1) (`get`) or an O(n) scan-and-filter
/// (`liquidatable_positions`), lock-free via DashMap — same concurrency
/// shape as `omega_flashloan::LiquidityRegistry`.
///
/// Shared via `Arc<PositionRegistry>`.
#[derive(Debug)]
pub struct PositionRegistry {
    /// (chain_id, borrower, protocol) → snapshot
    snapshots: DashMap<PositionKey, PositionSnapshot>,
}

impl PositionRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            snapshots: DashMap::new(),
        })
    }

    /// Record a fresh position snapshot, keyed by
    /// `(chain_id, snapshot.borrower, snapshot.protocol)`.
    ///
    /// Called by omega-oracle after a real on-chain position read (an
    /// aggregate account query for the HF/USD fields — see
    /// `PositionFinancials`'s own doc comment — plus a SEPARATE
    /// per-reserve or event-level read for the debt/collateral token
    /// addresses — see `PositionTokens`'s own doc comment for why that
    /// is not the same call). Overwrites any existing snapshot for the
    /// same key — same last-write-wins semantics as
    /// `LiquidityRegistry::update`.
    pub fn update(&self, chain_id: u64, snapshot: PositionSnapshot) {
        let key = PositionKey {
            chain_id,
            borrower: snapshot.borrower,
            protocol: snapshot.protocol,
        };

        tracing::debug!(
            borrower = %snapshot.borrower,
            protocol = %snapshot.protocol,
            chain_id,
            hf_e18 = %snapshot.hf_e18,
            tier = ?snapshot.tier,
            block_number = snapshot.block_number,
            "Position snapshot updated",
        );

        self.snapshots.insert(key, snapshot);
    }

    /// Look up the current snapshot for a specific
    /// `(chain_id, borrower, protocol)` triplet, if one has been
    /// recorded.
    ///
    /// No staleness filtering — see this file's module-level
    /// "Staleness" note for why this crate does not implement a
    /// trust-window check itself. Callers that need a
    /// block-number-bounded read should check the returned snapshot's
    /// `block_number` themselves against whatever the real §6 trust
    /// window is.
    pub fn get(
        &self,
        chain_id: u64,
        borrower: Address,
        protocol: Address,
    ) -> Option<PositionSnapshot> {
        let key = PositionKey {
            chain_id,
            borrower,
            protocol,
        };
        self.snapshots.get(&key).map(|e| e.value().clone())
    }

    /// All currently-liquidatable positions
    /// (`PositionSnapshot::is_liquidatable() == true`) tracked for
    /// `chain_id`, sorted ascending by health factor — lowest (most
    /// urgent / deepest underwater) first.
    ///
    /// See this file's module-level "Selection ordering" note: this
    /// ordering is a design choice made in this crate, not a value
    /// derived from spec.
    pub fn liquidatable_positions(&self, chain_id: u64) -> Vec<PositionSnapshot> {
        let mut positions: Vec<PositionSnapshot> = self
            .snapshots
            .iter()
            .filter(|e| e.key().chain_id == chain_id)
            .map(|e| e.value().clone())
            .filter(|snap| snap.is_liquidatable())
            .collect();

        // `sort_by_key` rather than `sort_by(|a, b| a.hf_e18.cmp(&b.hf_e18))` —
        // `cargo clippy -D warnings` (clippy::unnecessary_sort_by) flagged the
        // comparator form since it only ever compares a single field. `hf_e18` is
        // `alloy_primitives::U256`, which is `Copy`, so extracting it as a key per
        // element is cheap; no behavior change versus the comparator form.
        positions.sort_by_key(|p| p.hf_e18);
        positions
    }

    /// Removes a position's snapshot entirely — e.g. once omega-oracle
    /// observes the position has been repaid or fully liquidated and
    /// should no longer be tracked.
    ///
    /// Not currently called by anything in this crate or shown to its
    /// author; provided so a real oracle-side eviction path has an
    /// actual method to call rather than letting stale, no-longer-real
    /// positions accumulate in the map forever. Flagging this rather
    /// than silently omitting it, since an ever-growing map with no
    /// eviction path is a real operational concern for a long-running
    /// process.
    pub fn remove(&self, chain_id: u64, borrower: Address, protocol: Address) {
        let key = PositionKey {
            chain_id,
            borrower,
            protocol,
        };
        self.snapshots.remove(&key);
    }
}

impl Default for PositionRegistry {
    fn default() -> Self {
        Self {
            snapshots: DashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use omega_core::types::oracle::{PositionFinancials, PositionTokens};

    fn addr(b: u8) -> Address {
        Address::from([b; 20])
    }

    /// Builds a real `PositionSnapshot` via its own canonical
    /// constructor (not a struct literal) — same reasoning as this
    /// module's peers elsewhere in this codebase preferring the real
    /// constructor over hand-built literals: it exercises the same
    /// tier-derivation path a real caller would go through.
    fn sample_snapshot(borrower: u8, protocol: u8, hf_e18: u128) -> PositionSnapshot {
        PositionSnapshot::new(
            addr(borrower),
            addr(protocol),
            U256::from(hf_e18),
            PositionFinancials {
                collateral_usd_e18: U256::from(2_000_000_000_000_000_000u128),
                debt_usd_e18: U256::from(1_000_000_000_000_000_000u128),
                liquidation_bonus_bps: 500,
            },
            PositionTokens {
                debt_token: addr(0xD0),
                collateral_token: addr(0xC0),
            },
            1_000,
            1,
        )
    }

    const E18: u128 = 1_000_000_000_000_000_000;

    #[test]
    fn update_and_get_roundtrip() {
        let reg = PositionRegistry::new();
        let snap = sample_snapshot(0x01, 0x02, E18 - 1);
        reg.update(42161, snap.clone());

        let fetched = reg.get(42161, addr(0x01), addr(0x02));
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().hf_e18, snap.hf_e18);
    }

    #[test]
    fn get_unknown_position_returns_none() {
        let reg = PositionRegistry::new();
        assert!(reg.get(42161, addr(0x99), addr(0x98)).is_none());
    }

    #[test]
    fn get_respects_chain_id_even_with_same_borrower_protocol() {
        // Same rationale as omega_flashloan's own chain-scoping —
        // PositionSnapshot carries no chain_id, so the registry must
        // supply the disambiguation itself.
        let reg = PositionRegistry::new();
        let snap = sample_snapshot(0x01, 0x02, E18 - 1);
        reg.update(42161, snap.clone());

        assert!(reg.get(42161, addr(0x01), addr(0x02)).is_some());
        assert!(
            reg.get(1, addr(0x01), addr(0x02)).is_none(),
            "a snapshot recorded for chain 42161 must not leak into chain 1's lookup"
        );
    }

    #[test]
    fn update_overwrites_existing_entry_for_same_key() {
        let reg = PositionRegistry::new();
        reg.update(42161, sample_snapshot(0x01, 0x02, E18 - 1));
        reg.update(42161, sample_snapshot(0x01, 0x02, E18 + 1));

        let fetched = reg.get(42161, addr(0x01), addr(0x02)).unwrap();
        assert_eq!(fetched.hf_e18, U256::from(E18 + 1));
    }

    #[test]
    fn liquidatable_positions_excludes_healthy_ones() {
        let reg = PositionRegistry::new();
        reg.update(42161, sample_snapshot(0x01, 0x02, E18 - 1)); // liquidatable
        reg.update(42161, sample_snapshot(0x03, 0x04, E18 + 1)); // healthy

        let liquidatable = reg.liquidatable_positions(42161);
        assert_eq!(liquidatable.len(), 1);
        assert_eq!(liquidatable[0].borrower, addr(0x01));
    }

    #[test]
    fn liquidatable_positions_sorted_ascending_by_health_factor() {
        let reg = PositionRegistry::new();
        reg.update(42161, sample_snapshot(0x01, 0x02, E18 - 100));
        reg.update(42161, sample_snapshot(0x03, 0x04, E18 - 500)); // most urgent
        reg.update(42161, sample_snapshot(0x05, 0x06, E18 - 10));

        let liquidatable = reg.liquidatable_positions(42161);
        assert_eq!(liquidatable.len(), 3);
        assert_eq!(
            liquidatable[0].borrower,
            addr(0x03),
            "lowest health factor (most urgent) must sort first"
        );
        assert_eq!(liquidatable[2].borrower, addr(0x05));
    }

    #[test]
    fn liquidatable_positions_scoped_to_chain_id() {
        let reg = PositionRegistry::new();
        reg.update(42161, sample_snapshot(0x01, 0x02, E18 - 1));
        reg.update(1, sample_snapshot(0x03, 0x04, E18 - 1));

        assert_eq!(reg.liquidatable_positions(42161).len(), 1);
        assert_eq!(reg.liquidatable_positions(1).len(), 1);
        assert_eq!(reg.liquidatable_positions(10).len(), 0);
    }

    #[test]
    fn remove_deletes_the_entry() {
        let reg = PositionRegistry::new();
        reg.update(42161, sample_snapshot(0x01, 0x02, E18 - 1));
        assert!(reg.get(42161, addr(0x01), addr(0x02)).is_some());

        reg.remove(42161, addr(0x01), addr(0x02));
        assert!(reg.get(42161, addr(0x01), addr(0x02)).is_none());
    }

    #[test]
    fn remove_unknown_position_is_a_harmless_no_op() {
        let reg = PositionRegistry::new();
        reg.remove(42161, addr(0x99), addr(0x98)); // must not panic
    }
}