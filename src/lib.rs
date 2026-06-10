// src/lib.rs
// OmegaEngine v12 root module
//
// Design goals:
// - Stable module graph (CI-safe)
// - Explicit feature boundaries
// - No implicit imports or hidden dependencies
// - Safe for large workspace orchestration
//
// This crate acts as the orchestration root,
// not a logic-heavy module container.

#![deny(clippy::all)]
#![deny(unsafe_code)]

/// =========================
/// CORE ENGINE LAYERS
/// =========================

pub mod core;
pub mod runtime;

/// =========================
/// PROTOCOL + CHAIN INTEGRATION
/// =========================

pub mod rpc;
pub mod relay;
pub mod oracle;

/// =========================
/// STRATEGY + EXECUTION LAYERS
/// =========================

pub mod strategies;
pub mod risk;
pub mod gas;
pub mod execution;

/// =========================
/// ANALYTICS + OBSERVABILITY
/// =========================

pub mod health;
pub mod observability;
pub mod metrics;

/// =========================
/// SECURITY + COMPLIANCE
/// =========================

pub mod security;
pub mod compliance;

/// =========================
/// LOSS / ATTRIBUTION SYSTEM
/// =========================

pub mod loss_attribution;

/// =========================
/// CROSS-CHAIN + DAG SYSTEM
/// =========================

pub mod cross_chain;
pub mod dag;

/// =========================
/// CHAOS / SIMULATION LAYER
/// =========================

pub mod chaos;

/// =========================
/// ZK / CRYPTO PRIMITIVES
/// =========================

pub mod zk;

/// =========================
/// HOT PATH (LOW LATENCY EXECUTION)
/// =========================

pub mod hot_path;

/// =========================
/// PUBLIC ENGINE ENTRYPOINTS
/// =========================

pub mod engine;
pub mod prelude;