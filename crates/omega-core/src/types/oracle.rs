// crates/omega-core/src/types/oracle.rs
//
// Oracle domain types used across the Omega crate graph.
//
// These types represent the processed, validated oracle data that the
// oracle layer (omega-oracle) exposes to strategy scoring.  They are
// distinct from raw OracleSignal payloads (types/signal.rs) — signals
// are the wire format; these are the domain model.
//
// ## Audit finding fixed in this pass
//
// `OraclePrice`'s doc comment already stated the invariant "Zero
// indicates the price feed is stale or unavailable — strategies must
// reject zero-priced tokens" — but nothing enforced it anywhere in this
// type. A documented-but-unenforced invariant means every call site has
// to independently remember to check `price_usd_e18 != 0` itself, with
// no shared, correct, single implementation to call instead — exactly
// the kind of gap where one call site eventually gets the check
// backwards or forgets it entirely. Added `OraclePrice::is_valid()` as
// the single canonical implementation of that check.
//
// ## Clippy fix: too_many_arguments on PositionSnapshot::new (8/7)
//
// `cargo clippy --workspace --all-targets -- -D warnings` fails on this
// constructor: it took 8 positional parameters, over clippy's default
// threshold of 7. Rather than `#[allow(clippy::too_many_arguments)]`,
// the three USD/bps financial fields (`collateral_usd_e18`,
// `debt_usd_e18`, `liquidation_bonus_bps`) are grouped into a new
// `PositionFinancials` struct — they're already a natural unit (the
// numbers a caller reads off one lending-protocol query, e.g. Aave's
// `getUserAccountData`), so this also makes call sites slightly more
// self-documenting, not just quieter under lint. `PositionSnapshot`'s
// own field layout and every other type/method in this file is
// unchanged.
//
// ## Debt/collateral token fields (this revision)
//
// `PositionSnapshot` previously carried no ERC20 token identity at
// all — `debt_usd_e18`/`collateral_usd_e18` are aggregate USD values,
// not asset addresses. This blocked `omega-strategies::la.rs`'s
// `build_blueprint` from ever populating `ExecutionBlueprint::
// flashloan_token` (LA's own explicit guard there refuses to build
// rather than fabricate a token address — see that file's "Known
// incomplete: flashloan_token" comment) and, separately, blocks
// encoding `LiquidationArb.execute()`'s real calldata layout, which
// requires both `collateral` and `debt` token addresses.
//
// Added `debt_token: Address` / `collateral_token: Address` directly on
// `PositionSnapshot`, sourced via a new `PositionTokens` grouping
// struct passed into `PositionSnapshot::new`.
//
// DELIBERATELY NOT folded into `PositionFinancials`: that struct's own
// doc comment specifically scopes it to "the values a caller typically
// reads off one lending-protocol account query in one shot" (e.g. Aave
// v3 `getUserAccountData`) — an aggregate, USD-denominated,
// no-per-asset-detail call. Token addresses do NOT come from that same
// call; per real Aave/Compound/Morpho interfaces, identifying which
// specific reserve/asset a borrower's debt sits in requires either a
// separate per-reserve read or comes from the Borrow/Supply event that
// flagged the position as a candidate in the first place. Merging the
// two into one struct would misrepresent where this data actually
// originates, and risks a caller assuming both groups can be filled
// from the same query when they cannot.
//
// `PositionSnapshot::new`'s parameter count goes from 6 to 7 with this
// change (still under clippy's too-many-arguments threshold of 7 — see
// the clippy-fix note above for why that threshold matters here).
//
// Spec references:
//   §7   — dual-component gas model: FeeSnapshot fields
//   §11  — LA tier classification: PositionSnapshot health factor
//   §11.1 — hot/warm/cold/archived tier thresholds
//   §11.4 — reorg guard: PositionSnapshot.block_number used to detect
//            orphaned blueprints

use alloy_primitives::{Address, U256};
use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// OraclePrice
// ─────────────────────────────────────────────────────────────────────────────

/// Spot price for a single token, sourced and validated by omega-oracle.
///
/// Prices are expressed as 18-decimal fixed-point integers (e18) to
/// avoid floating-point precision loss in profit calculations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OraclePrice {
    /// Token contract address.
    pub token: Address,

    /// Price in USD × 10^18.  Zero indicates the price feed is stale
    /// or unavailable — strategies must reject zero-priced tokens.
    pub price_usd_e18: U256,

    /// Block number at which this price was last observed.
    pub block_number: u64,

    /// Unix timestamp (ms) when oracle-layer received this update.
    pub received_at_unix_ms: u64,

    /// Whether this price came from a primary (on-chain TWAP) or
    /// fallback (Chainlink / Pyth) source.  Strategies may apply a
    /// confidence discount on fallback-sourced prices.
    pub is_fallback: bool,
}

impl OraclePrice {
    /// Returns `true` when this price is usable — i.e. non-zero. A
    /// zero `price_usd_e18` is documented as meaning "feed is stale or
    /// unavailable"; this is the single canonical place that check
    /// should be made, rather than every call site re-implementing
    /// `price.price_usd_e18 != U256::ZERO` independently (and risking
    /// getting the comparison direction wrong).
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.price_usd_e18 != U256::ZERO
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// LaTier
// ─────────────────────────────────────────────────────────────────────────────

/// Liquidation Arbitrage monitoring tier for a lending position (§11.1).
///
/// Tier drives the recompute frequency and resource allocation:
///
/// | Tier     | HF range      | Update trigger                          |
/// |----------|---------------|-----------------------------------------|
/// | Hot      | < 1.01        | Every oracle update — immediate         |
/// | Warm     | 1.01 – 1.05   | Batched every 200ms OR move > 0.5%     |
/// | Cold     | 1.05 – 1.20   | Lazy every 2 s                          |
/// | Archived | > 1.20        | Lazy on 500-block cycle; eviction cand. |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaTier {
    Hot,
    Warm,
    Cold,
    Archived,
}

impl LaTier {
    /// Classify a position by its health factor (18-decimal fixed-point).
    ///
    /// `hf_e18` is the health factor × 10^18 as stored on Aave/Compound/
    /// Morpho.  Thresholds match §11.1 exactly:
    ///   Hot      < 1.01 × 10^18
    ///   Warm     1.01–1.05 × 10^18
    ///   Cold     1.05–1.20 × 10^18
    ///   Archived > 1.20 × 10^18
    pub fn from_hf_e18(hf_e18: U256) -> Self {
        // 1e18 = 1.0 as a fixed-point multiplier
        const E18: u128 = 1_000_000_000_000_000_000;

        // Thresholds in e18 notation
        let hot_threshold = U256::from(E18 + E18 / 100); // 1.01e18
        let warm_threshold = U256::from(E18 + 5 * E18 / 100); // 1.05e18
        let cold_threshold = U256::from(E18 + 20 * E18 / 100); // 1.20e18

        if hf_e18 < hot_threshold {
            LaTier::Hot
        } else if hf_e18 < warm_threshold {
            LaTier::Warm
        } else if hf_e18 < cold_threshold {
            LaTier::Cold
        } else {
            LaTier::Archived
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PositionSnapshot
// ─────────────────────────────────────────────────────────────────────────────

/// Grouped financial inputs for `PositionSnapshot::new` — the values a
/// caller typically reads off a single lending-protocol account query
/// (e.g. Aave v3 `getUserAccountData`) in one shot. Grouped into a
/// struct so `PositionSnapshot::new` stays under clippy's
/// too-many-arguments threshold; see this file's own header comment.
#[derive(Debug, Clone, Copy)]
pub struct PositionFinancials {
    /// Total collateral value in USD × 10^18.
    pub collateral_usd_e18: U256,

    /// Total debt value in USD × 10^18.
    pub debt_usd_e18: U256,

    /// Liquidation bonus in basis points (e.g. 500 = 5%).
    pub liquidation_bonus_bps: u16,
}

/// Grouped token-identity inputs for `PositionSnapshot::new` — the
/// ERC20 addresses this position's debt and collateral are denominated
/// in. See this file's own header comment ("Debt/collateral token
/// fields") for why this is a separate struct from `PositionFinancials`
/// rather than folded into it: the two groups come from genuinely
/// different real-world data sources (an aggregate account-level query
/// vs. per-reserve/event-level asset identification), and collapsing
/// them would misrepresent that.
#[derive(Debug, Clone, Copy)]
pub struct PositionTokens {
    /// ERC20 address the borrower's debt is denominated in. This is
    /// the token a flashloan must be denominated in to repay the debt
    /// on liquidation (see `omega_core::types::blueprint::
    /// ExecutionBlueprint::flashloan_token`), and the `debt` field
    /// `LiquidationArb.execute()` expects in its ABI-encoded calldata.
    pub debt_token: Address,

    /// ERC20 address of the collateral asset to be seized on
    /// liquidation. Corresponds to `LiquidationArb.execute()`'s
    /// `collateral` calldata field.
    pub collateral_token: Address,
}

/// Snapshot of a single lending position for LA scoring (§11).
///
/// Produced by omega-oracle from on-chain position data (Aave v3
/// `getUserAccountData`, Compound `getAccountLiquidity`, etc.) and
/// cached in the EIL double-buffer.
///
/// ## Key invariant
///
/// A blueprint built from a PositionSnapshot is only valid while the
/// snapshot's `block_number` is within the revm trust window (§6).
/// The LA reorg guard (§11.4) tracks `block_number` to detect orphaned
/// blueprints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionSnapshot {
    /// Borrower's wallet address.
    pub borrower: Address,

    /// Lending protocol contract address (Aave v3 pool, Compound
    /// comptroller, Morpho, Euler v2).
    pub protocol: Address,

    /// Health factor × 10^18.  Below 1e18 → liquidatable.
    pub hf_e18: U256,

    /// Total collateral value in USD × 10^18.
    pub collateral_usd_e18: U256,

    /// Total debt value in USD × 10^18.
    pub debt_usd_e18: U256,

    /// Liquidation bonus in basis points (e.g. 500 = 5%).
    pub liquidation_bonus_bps: u16,

    /// ERC20 address the borrower's debt is denominated in (this
    /// revision). See `PositionTokens::debt_token`'s doc comment for
    /// what this feeds downstream.
    pub debt_token: Address,

    /// ERC20 address of the collateral asset to be seized on
    /// liquidation (this revision). See
    /// `PositionTokens::collateral_token`'s doc comment.
    pub collateral_token: Address,

    /// Monitoring tier derived from `hf_e18` at snapshot time (§11.1).
    pub tier: LaTier,

    /// Block number at which this snapshot was taken.
    /// Used by the reorg guard (§11.4) and sequencer restart handler
    /// (§11.3) as the deduplication anchor.
    pub block_number: u64,

    /// Monotonic state version from the EIL (§6).
    pub state_version: u64,
}

impl PositionSnapshot {
    /// Stable deduplication key for the sequencer restart DashMap (§11.3).
    ///
    /// Key = keccak256(borrower ++ protocol) encoded as hex.
    /// Does NOT include block_number so that the same position is
    /// deduplicated across blocks within the 60-block restart window.
    pub fn dedup_key(&self) -> String {
        use alloy_primitives::keccak256;
        let mut buf = Vec::with_capacity(40);
        buf.extend_from_slice(self.borrower.as_slice());
        buf.extend_from_slice(self.protocol.as_slice());
        hex::encode(keccak256(&buf).as_slice())
    }

    /// Returns `true` when this position is currently liquidatable
    /// (health factor below 1.0 × 10^18).
    ///
    /// Boundary is strict `<` (HF == 1.0 exactly is NOT liquidatable),
    /// matching standard lending-protocol semantics (e.g. Aave requires
    /// healthFactor strictly below 1e18) — confirmed consistent with
    /// real on-chain behavior, not changed in this pass.
    #[inline]
    pub fn is_liquidatable(&self) -> bool {
        const E18: u128 = 1_000_000_000_000_000_000;
        self.hf_e18 < U256::from(E18)
    }

    /// Construct a functional PositionSnapshot from oracle-observed data.
    ///
    /// The monitoring tier is derived directly from `hf_e18` at
    /// construction time, ensuring the snapshot cannot carry a tier
    /// inconsistent with its health factor.
    ///
    /// `financials` bundles the three USD/bps values a caller normally
    /// reads off one lending-protocol account query in a single shot
    /// (see `PositionFinancials`'s own doc comment). `tokens` bundles
    /// the debt/collateral token addresses — a DIFFERENT real-world data
    /// source from `financials` (see `PositionTokens`'s own doc comment
    /// and this file's header comment for why these are not merged).
    ///
    /// All numeric values use the same fixed-point conventions as the
    /// PositionSnapshot fields:
    ///   - hf_e18: health factor × 10^18
    ///   - financials.collateral_usd_e18: USD × 10^18
    ///   - financials.debt_usd_e18: USD × 10^18
    ///   - financials.liquidation_bonus_bps: basis points
    ///
    /// `block_number` identifies the chain block from which the position
    /// data was observed. `state_version` identifies the corresponding
    /// EIL state version.
    #[inline]
    pub fn new(
        borrower: Address,
        protocol: Address,
        hf_e18: U256,
        financials: PositionFinancials,
        tokens: PositionTokens,
        block_number: u64,
        state_version: u64,
    ) -> Self {
        Self {
            borrower,
            protocol,
            hf_e18,
            collateral_usd_e18: financials.collateral_usd_e18,
            debt_usd_e18: financials.debt_usd_e18,
            liquidation_bonus_bps: financials.liquidation_bonus_bps,
            debt_token: tokens.debt_token,
            collateral_token: tokens.collateral_token,
            tier: LaTier::from_hf_e18(hf_e18),
            block_number,
            state_version,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// FeeSnapshot
// ─────────────────────────────────────────────────────────────────────────────

/// Current Arbitrum fee oracle reading (§7, §12.2).
///
/// Both components are required for the dual-component gas model:
///   total_gas_cost = (l2_exec_gas × base_fee) + (l1_data_gas × l1_data_fee)
///
/// Priority fee (tip) is submitted to the Arbitrum sequencer separately
/// and does not affect the gas cost calculation — it affects inclusion
/// probability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeSnapshot {
    /// EIP-1559 base fee in gwei (L2 execution cost component).
    pub base_fee_gwei: u64,

    /// L1 data fee per gas unit in gwei (calldata cost component).
    /// Sourced from Arbitrum's ArbGasInfo precompile.
    pub l1_data_fee_gwei: u64,

    /// Current competitive priority fee in gwei (§12.2).
    /// The Gas War Engine uses this to set `priority_fee_gwei` on
    /// blueprints, bounded by the 500 gwei ceiling.
    pub priority_fee_gwei: u64,

    /// Block number at which this fee oracle was sampled.
    pub block_number: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_position(hf_e18: u128) -> PositionSnapshot {
        PositionSnapshot {
            borrower: Address::ZERO,
            protocol: Address::from([0x11u8; 20]),
            hf_e18: U256::from(hf_e18),
            collateral_usd_e18: U256::from(2_000_000_000_000_000_000u128),
            debt_usd_e18: U256::from(1_000_000_000_000_000_000u128),
            liquidation_bonus_bps: 500,
            // Non-zero, distinct test values — chosen so a test that
            // checks "not the zero address" or "debt_token !=
            // collateral_token" isn't trivially true by coincidence.
            debt_token: Address::from([0x22u8; 20]),
            collateral_token: Address::from([0x33u8; 20]),
            tier: LaTier::Hot,
            block_number: 1,
            state_version: 1,
        }
    }

    #[test]
    fn oracle_price_zero_is_invalid() {
        let p = OraclePrice {
            token: Address::ZERO,
            price_usd_e18: U256::ZERO,
            block_number: 1,
            received_at_unix_ms: 0,
            is_fallback: false,
        };
        assert!(!p.is_valid());
    }

    #[test]
    fn oracle_price_nonzero_is_valid() {
        let p = OraclePrice {
            token: Address::ZERO,
            price_usd_e18: U256::from(3_000_000_000_000_000_000_000u128),
            block_number: 1,
            received_at_unix_ms: 0,
            is_fallback: false,
        };
        assert!(p.is_valid());
    }

    #[test]
    fn la_tier_boundaries() {
        const E18: u128 = 1_000_000_000_000_000_000;
        assert_eq!(LaTier::from_hf_e18(U256::from(E18)), LaTier::Hot); // 1.0 exactly -> Hot
        assert_eq!(
            LaTier::from_hf_e18(U256::from(E18 + E18 / 100)),
            LaTier::Warm
        ); // 1.01 -> Warm (boundary inclusive)
        assert_eq!(
            LaTier::from_hf_e18(U256::from(E18 + 5 * E18 / 100)),
            LaTier::Cold
        ); // 1.05 -> Cold
        assert_eq!(
            LaTier::from_hf_e18(U256::from(E18 + 20 * E18 / 100)),
            LaTier::Archived
        ); // 1.20 -> Archived
    }

    #[test]
    fn is_liquidatable_boundary() {
        const E18: u128 = 1_000_000_000_000_000_000;
        assert!(sample_position(E18 - 1).is_liquidatable());
        assert!(
            !sample_position(E18).is_liquidatable(),
            "HF == 1.0 exactly is not liquidatable"
        );
        assert!(!sample_position(E18 + 1).is_liquidatable());
    }

    #[test]
    fn dedup_key_is_stable_and_ignores_block_number() {
        let mut p1 = sample_position(1_000_000_000_000_000_000);
        let mut p2 = p1.clone();
        p2.block_number = 999; // different block, same borrower/protocol
        assert_eq!(p1.dedup_key(), p2.dedup_key());

        p1.protocol = Address::from([0x22u8; 20]);
        assert_ne!(p1.dedup_key(), p2.dedup_key());
    }

    // ── Debt/collateral token fields (this revision) ──────────────────────

    #[test]
    fn new_populates_token_fields_from_position_tokens() {
        let debt_token = Address::from([0xAAu8; 20]);
        let collateral_token = Address::from([0xBBu8; 20]);
        let snap = PositionSnapshot::new(
            Address::from([0x01u8; 20]),
            Address::from([0x02u8; 20]),
            U256::from(1_000_000_000_000_000_000u128 - 1), // liquidatable
            PositionFinancials {
                collateral_usd_e18: U256::from(2_000_000_000_000_000_000u128),
                debt_usd_e18: U256::from(1_000_000_000_000_000_000u128),
                liquidation_bonus_bps: 500,
            },
            PositionTokens {
                debt_token,
                collateral_token,
            },
            12_345,
            7,
        );
        assert_eq!(snap.debt_token, debt_token);
        assert_eq!(snap.collateral_token, collateral_token);
    }

    #[test]
    fn new_still_derives_tier_from_hf_e18_with_tokens_present() {
        // Regression guard: adding PositionTokens must not disturb the
        // existing tier-derivation invariant documented on `new()`.
        const E18: u128 = 1_000_000_000_000_000_000;
        let snap = PositionSnapshot::new(
            Address::ZERO,
            Address::from([0x11u8; 20]),
            U256::from(E18 + E18 / 100), // 1.01e18 -> Warm
            PositionFinancials {
                collateral_usd_e18: U256::ZERO,
                debt_usd_e18: U256::ZERO,
                liquidation_bonus_bps: 0,
            },
            PositionTokens {
                debt_token: Address::from([0x44u8; 20]),
                collateral_token: Address::from([0x55u8; 20]),
            },
            1,
            1,
        );
        assert_eq!(snap.tier, LaTier::Warm);
    }
}