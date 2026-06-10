// crates/omega-compliance/src/lib.rs
// crates/omega-compliance/src/lib.rs
//
// omega-compliance — OFA compliance validation library (spec §8).
//
// ## Purpose
//
//   Order Flow Agreements (OFA) require that bundles which include
//   user-initiated order flow:
//     1. Have explicit consent from the order's originator (§8).
//     2. Protect the user from slippage beyond the agreed bound (§8).
//     3. Carry a valid, unexpired order signed by the originator (§8).
//
//   Blueprints that set `ofa_compliant = true` MUST pass all three
//   checks before relay submission.  The compliance layer is the
//   authoritative gate — relays may additionally enforce their own
//   checks, but the engine never submits a non-compliant OFA bundle.
//
// ## Architectural role (§22.1)
//
//   omega-compliance ← omega-core
//
//   It is a pure synchronous validation library with no I/O.  Callers
//   (omega-strategies, omega-relay) hold an `Arc<OfaChecker>` and call
//   `validate_blueprint` in the hot path without blocking.
//
// ## Rule versioning (§8)
//
//   OFA compliance rules are versioned via `OfaRuleSet` and stored in
//   `RuleRegistry`.  Rules are updated via L3 governance (48h timelock).
//   The registry always returns the currently active rule set; the hot
//   path never reads stale rules across a governance update.
//
// ## Module map
//
//   ofa.rs    — `OfaChecker`, `OfaConsentRecord`, `OfaOrder`,
//               `OfaRuleSet`, `OfaCheckError`.
//               Core validation: check_consent, check_slippage,
//               check_order, validate_blueprint.
//
//   rules.rs  — `RuleRegistry`, `RuleSetEntry`, `RegistryError`.
//               Versioned rule storage with L3-governance update path.

pub mod ofa;
pub mod rules;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use ofa::{OfaCheckError, OfaChecker, OfaConsentRecord, OfaOrder, OfaRuleSet};
pub use rules::{RegistryError, RuleRegistry, RuleSetEntry};
