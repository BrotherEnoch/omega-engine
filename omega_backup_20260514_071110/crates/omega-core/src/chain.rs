ï»¿// crates/omega-core/src/chain.rs
//
// ChainId â€” canonical chain identifiers for every chain the Omega Engine
// targets in v12.
//
// Spec references:
//   Â§1.1  â€” Phase table; Arbitrum is the primary execution chain for
//            Phases 1â€“3.  Ethereum mainnet enters in Phase 4 (MEV-OFA).
//   Â§7    â€” Dual-component gas model is Arbitrum-specific.
//   Â§11   â€” 80ms LA window is calibrated for Arbitrum's 250ms block time.
//   Â§12.2 â€” Arbitrum priority fee note; 500 gwei ceiling.
//   Â§12.3 â€” MEV-Boost builder blacklist applies to Ethereum L1 only.
//   Â§18   â€” CHI/GST gas token analysis â€” Ethereum L1 (Phase 4+) only;
//            not applicable on Arbitrum or Base.
//   Â§22.1 â€” Inter-crate dependency graph.  ChainId lives in omega-core
//            so every crate can use it without pulling in omega-rpc.
//
// Versioning: Base (8453) is included because the spec lists it in the
// dependency graph as a future expansion target.  No strategies are
// activated on Base in v12; the variant is present so chain-routing code
// can handle it explicitly (return Unsupported) rather than via a
// catch-all that silently discards unknown IDs.

use serde::{Deserialize, Serialize};
use thiserror::Error;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ChainId
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// EIP-155 chain identifier for every network the Omega Engine targets.
///
/// ## v12 status per chain
///
/// | Chain    | Chain ID | v12 Status                                    |
/// |----------|----------|-----------------------------------------------|
/// | Arbitrum | 42161    | PRIMARY â€” all Phases 1â€“3 execute here         |
/// | Ethereum | 1        | Phase 4 (MEV-OFA, L1 liquidations)            |
/// | Base     | 8453     | Defined; no strategies activated in v12        |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainId {
    /// Arbitrum One â€” primary execution environment for Phases 1â€“3.
    /// 250ms block time, Arbitrum dual-component gas model (Â§7),
    /// direct sequencer submission (no MEV-Boost).
    Arbitrum = 42161,

    /// Ethereum mainnet â€” Phase 4 MEV-OFA and L1 liquidations.
    /// 12s block time, MEV-Boost + builder blacklist (Â§12.3),
    /// CHI/GST gas token analysis (Â§18).
    Ethereum = 1,

    /// Base â€” defined for future expansion; no v12 strategies.
    Base = 8453,
}

impl ChainId {
    /// Average block time in milliseconds.
    ///
    /// Used by the LA tier monitor (Â§11) to compute the 80ms window
    /// budget relative to block production rate, and by the reorg guard
    /// (Â§11.4) to size the 60-block sequencer restart deduplication
    /// window in wall-clock terms.
    #[inline]
    pub fn block_time_ms(self) -> u64 {
        match self {
            ChainId::Arbitrum => 250,
            ChainId::Base     => 2_000,
            ChainId::Ethereum => 12_000,
        }
    }

    /// The 80ms LA execution window budget in blocks (Â§11).
    ///
    /// Arbitrum: 80ms / 250ms = 0.32 â†’ rounds to 1 block (the engine
    /// must land within the *current* block).  Used by the blueprint
    /// expiry calculation: `expiry_block = current_block + la_window_blocks`.
    ///
    /// Ethereum is included for completeness (Phase 4 L1 liquidations)
    /// but the window is narrower relative to block time.
    #[inline]
    pub fn la_window_blocks(self) -> u64 {
        // ceiling(80 / block_time_ms), minimum 1
        let raw = 80_u64.div_ceil(self.block_time_ms());
        raw.max(1)
    }

    /// Sequencer restart deduplication window in blocks (Â§11.3).
    ///
    /// The spec fixes this at 60 blocks for Arbitrum (~15 seconds at
    /// 250ms).  We keep the constant per-chain so the SequencerRestartHandler
    /// does not hard-code Arbitrum assumptions.
    #[inline]
    pub fn sequencer_restart_window_blocks(self) -> u64 {
        match self {
            // Spec Â§11.3: 60 blocks â‰ˆ 15 seconds on Arbitrum.
            ChainId::Arbitrum => 60,
            // Ethereum and Base: same 15-second wall-clock window,
            // scaled to their block times.
            ChainId::Ethereum => 2,   // 2 blocks Ã— 12s = 24s (closest â‰¥ 15s)
            ChainId::Base     => 8,   // 8 blocks Ã— 2s  = 16s
        }
    }

    /// Returns `true` when the Gas War Engine should apply the
    /// Arbitrum dual-component gas model (Â§7).
    ///
    /// `false` on Ethereum mainnet and Base â€” those chains use the
    /// standard EIP-1559 single-component model.
    #[inline]
    pub fn uses_arbitrum_gas_model(self) -> bool {
        matches!(self, ChainId::Arbitrum)
    }

    /// Returns `true` when MEV-Boost builder blacklist filtering applies
    /// (Â§12.3).  Arbitrum uses direct sequencer submission â€” no
    /// MEV-Boost â€” so the blacklist is irrelevant there.
    #[inline]
    pub fn uses_mev_boost(self) -> bool {
        matches!(self, ChainId::Ethereum)
    }

    /// Returns `true` when CHI/GST gas token optimisation is potentially
    /// applicable (Â§18).  Arbitrum and Base do not support EVM storage
    /// refunds.
    #[inline]
    pub fn supports_gas_tokens(self) -> bool {
        matches!(self, ChainId::Ethereum)
    }

    /// Fallible conversion from a raw `u64` chain ID.
    ///
    /// Returns `Err(UnknownChainId)` for any chain not enumerated here
    /// rather than silently discarding it.  Call sites that receive an
    /// unrecognised chain ID must record a `DropCode::WrongChainId` event
    /// (Â§errors.rs) and discard the associated blueprint.
    pub fn from_u64(id: u64) -> Result<Self, UnknownChainId> {
        match id {
            42161 => Ok(ChainId::Arbitrum),
            1     => Ok(ChainId::Ethereum),
            8453  => Ok(ChainId::Base),
            other => Err(UnknownChainId(other)),
        }
    }

    /// Raw numeric chain ID as `u64`.
    #[inline]
    pub fn as_u64(self) -> u64 {
        self as u64
    }
}

impl std::fmt::Display for ChainId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChainId::Arbitrum => f.write_str("arbitrum"),
            ChainId::Ethereum => f.write_str("ethereum"),
            ChainId::Base     => f.write_str("base"),
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// UnknownChainId
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Error returned when a raw chain ID does not map to a known [`ChainId`].
///
/// Callers should record a `DropCode::WrongChainId` metric event (Â§errors.rs)
/// and discard the blueprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("Unknown chain ID: {0}")]
pub struct UnknownChainId(pub u64);

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_from_u64() {
        for (id, expected) in [
            (42161_u64, ChainId::Arbitrum),
            (1_u64,     ChainId::Ethereum),
            (8453_u64,  ChainId::Base),
        ] {
            assert_eq!(ChainId::from_u64(id).unwrap(), expected);
            assert_eq!(expected.as_u64(), id);
        }
    }

    #[test]
    fn unknown_chain_id_returns_err() {
        assert_eq!(
            ChainId::from_u64(999).unwrap_err(),
            UnknownChainId(999),
        );
    }

    #[test]
    fn la_window_blocks_arbitrum() {
        // 80ms / 250ms block time â†’ 1 block (spec Â§11)
        assert_eq!(ChainId::Arbitrum.la_window_blocks(), 1);
    }

    #[test]
    fn sequencer_restart_window_arbitrum() {
        // Spec Â§11.3: 60 blocks
        assert_eq!(ChainId::Arbitrum.sequencer_restart_window_blocks(), 60);
    }

    #[test]
    fn arbitrum_gas_model_flags() {
        assert!(ChainId::Arbitrum.uses_arbitrum_gas_model());
        assert!(!ChainId::Ethereum.uses_arbitrum_gas_model());
        assert!(!ChainId::Base.uses_arbitrum_gas_model());
    }

    #[test]
    fn mev_boost_flags() {
        assert!(!ChainId::Arbitrum.uses_mev_boost());
        assert!(ChainId::Ethereum.uses_mev_boost());
        assert!(!ChainId::Base.uses_mev_boost());
    }

    #[test]
    fn gas_token_flags() {
        assert!(!ChainId::Arbitrum.supports_gas_tokens());
        assert!(ChainId::Ethereum.supports_gas_tokens());
        assert!(!ChainId::Base.supports_gas_tokens());
    }
}