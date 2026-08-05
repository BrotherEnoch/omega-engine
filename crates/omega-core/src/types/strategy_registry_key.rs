// crates/omega-core/src/types/strategy_registry_key.rs
//
// On-chain registry-key derivation for `StrategyId`.
//
// `OmegaOrchestrator.sol`'s `registerStrategy(bytes32 strategyId, address
// implementation)` accepts an arbitrary bytes32 chosen at registration
// time — nothing in the contract derives it from anything. This file is
// the single canonical derivation, used both by the (not-yet-written)
// blueprint encoder and by deploy/registration tooling, so the two sides
// can never independently disagree about what a given `StrategyId` maps
// to on-chain.
//
// Derivation: keccak256 of the UTF-8 bytes of `StrategyId`'s `Display`
// output ("SA", "CNRY", "MSA", "LA", "MEV").
//
// STABILITY WARNING: this is a load-bearing invariant, not a convenience
// function. Changing `StrategyId`'s `Display` impl changes every registry
// key this function produces, silently breaking any strategy already
// registered on-chain under the old key — every future blueprint for
// that strategy would revert with `UnknownStrategy` until
// `registerStrategy` is called again under the new key. The pinned test
// vectors below exist specifically to catch that class of change; do not
// delete or "fix" them to match a changed `Display` impl without also
// confirming a re-registration plan for whatever is already deployed.
//
// DEPENDENCY NOTE: this file assumes `StrategyId` already implements
// `std::fmt::Display` (referenced as already present in the locked
// design spec this implements). That impl lives in `blueprint.rs` and
// has not been directly viewed in this thread — if it does not exist,
// or produces different strings than "SA"/"CNRY"/"MSA"/"LA"/"MEV", the
// `display_strings_are_pinned` test below will fail to compile (missing
// `Display`) or fail at runtime (wrong strings) respectively, rather
// than silently producing wrong keys.

use crate::types::blueprint::StrategyId;
use alloy_primitives::{keccak256, B256};

impl StrategyId {
    /// On-chain registry key for this strategy — the `bytes32` value
    /// that must have been passed to `OmegaOrchestrator.registerStrategy`
    /// for this strategy to be callable, and that the blueprint encoder
    /// must use to fill the `strategyId` slot of the 9-tuple.
    pub fn to_registry_key(self) -> B256 {
        keccak256(self.to_string().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins `Display` output itself — deliberately separate from the
    /// hash-stability test below. This is the test that actually catches
    /// a `Display` wording change (e.g. "SA" becoming "STRATEGY_SA" or
    /// "sa"); `registry_keys_are_stable` alone cannot catch that, since a
    /// version of it written as `assert_eq!(id.to_registry_key(),
    /// keccak256(id.to_string().as_bytes()))` would trivially pass no
    /// matter what `Display` produces. This test's expected values are
    /// literal string constants for exactly that reason.
    #[test]
    fn display_strings_are_pinned() {
        assert_eq!(StrategyId::Sa.to_string(), "SA");
        assert_eq!(StrategyId::Cnry.to_string(), "CNRY");
        assert_eq!(StrategyId::Msa.to_string(), "MSA");
        assert_eq!(StrategyId::La.to_string(), "LA");
        assert_eq!(StrategyId::Mev.to_string(), "MEV");
    }

    /// Pins the derived registry keys against literal byte strings, not
    /// against `StrategyId::X.to_string()` — so this test and
    /// `display_strings_are_pinned` together cover both halves of the
    /// derivation (Display correctness, hashing correctness)
    /// independently of each other.
    #[test]
    fn registry_keys_are_stable() {
        assert_eq!(StrategyId::Sa.to_registry_key(), keccak256(b"SA"));
        assert_eq!(StrategyId::Cnry.to_registry_key(), keccak256(b"CNRY"));
        assert_eq!(StrategyId::Msa.to_registry_key(), keccak256(b"MSA"));
        assert_eq!(StrategyId::La.to_registry_key(), keccak256(b"LA"));
        assert_eq!(StrategyId::Mev.to_registry_key(), keccak256(b"MEV"));
    }

    /// Registry keys must be pairwise distinct — a collision between two
    /// strategy names would let one strategy's blueprint execute against
    /// another's registry entry. Vanishingly unlikely with keccak256 over
    /// these five short, hand-chosen strings, but cheap to assert
    /// directly rather than leave implicit.
    #[test]
    fn registry_keys_are_pairwise_distinct() {
        let keys = [
            StrategyId::Sa.to_registry_key(),
            StrategyId::Cnry.to_registry_key(),
            StrategyId::Msa.to_registry_key(),
            StrategyId::La.to_registry_key(),
            StrategyId::Mev.to_registry_key(),
        ];
        for i in 0..keys.len() {
            for j in (i + 1)..keys.len() {
                assert_ne!(
                    keys[i], keys[j],
                    "registry key collision at indices {i}/{j}"
                );
            }
        }
    }
}
