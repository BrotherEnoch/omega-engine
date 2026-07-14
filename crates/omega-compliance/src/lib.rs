// crates/omega-compliance/src/lib.rs
// crates/omega-compliance/src/lib.rs
//
// omega-compliance — Comprehensive policy + OFA compliance validation library (v12).
// 
// Enforces both OFA requirements and broad operational policies to prevent
// unintended trades, limit breaches, bypasses, and financial loss.

pub mod ofa;
pub mod rules;
pub mod policy;

pub use ofa::{OfaCheckError, OfaChecker, OfaConsentRecord, OfaOrder, OfaRuleSet};
pub use rules::{RegistryError, RuleRegistry, RuleSetEntry};
pub use policy::{ComplianceChecker, ComplianceError, CompliancePolicy};