// crates/omega-compliance/src/lib.rs
//
// omega-compliance — Comprehensive policy + OFA compliance validation library (v12).
//
// Enforces both OFA requirements and broad operational policies to prevent
// unintended trades, limit breaches, bypasses, and financial loss.

pub mod ofa;
pub mod policy;
pub mod rules;

pub use ofa::{OfaCheckError, OfaChecker, OfaConsentRecord, OfaOrder, OfaRuleSet};
pub use policy::{ComplianceChecker, ComplianceError, CompliancePolicy};
pub use rules::{RegistryError, RuleRegistry, RuleSetEntry};
