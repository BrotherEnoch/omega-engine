// omega-frontend-arch/crates/omega-control-contracts/src/proto.rs
//! Mirror of generated prost messages from `ops/control-plane/proto/omega_control.proto`.
//!
//! ## Environment Note
//! Fresh `protoc` generation is **blocked** in this environment (`Access is denied`).
//! This module hand-mirrors the prost output shape based on the proto file contents.
//! When protoc access is restored, replace this file entirely with generated output
//! and update `Cargo.toml` to add the `prost` + `tonic` build dependencies.
//!
//! ## Field Naming Ambiguity
//! `omega_control.proto` uses `layers[].layer_id` for the layer field name.
//! The REST endpoint uses `layers[].layer`.
//! See `health::LayerHealth` for the canonical resolution (serde alias).
//!
//! ## Auth Ambiguity
//! Proto spec marks ALL Get RPCs as L1 authenticated.
//! Active REST `GET /api/v1/health` is public.
//! Frontend route table preserves the active REST behaviour; the proto auth
//! tier is a documentation artefact of the planned gRPC interface, not the
//! deployed REST API.
//!
//! This module is gated behind the `proto` feature flag. Enable when building
//! the gRPC client.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// HealthReport (mirrors proto HealthReport message)
// ---------------------------------------------------------------------------

/// Mirrors `omega_control.proto` `HealthReport` message.
///
/// Note: proto field is `layer_id`; use `LayerHealth` (in `health.rs`) for
/// dual-format compatibility when deserialising from either wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtoHealthReport {
    pub layers: Vec<ProtoLayerEntry>,
    pub overall_status: i32,  // maps to HealthStatus enum ordinal
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtoLayerEntry {
    /// Proto field name: `layer_id` (differs from REST `layer`).
    pub layer_id: String,
    pub status: i32,
    #[serde(default)]
    pub message: String,
}

// ---------------------------------------------------------------------------
// Conversion to shared types
// ---------------------------------------------------------------------------

impl TryFrom<ProtoLayerEntry> for crate::health::LayerHealth {
    type Error = crate::error::FrontendError;

    fn try_from(p: ProtoLayerEntry) -> Result<Self, Self::Error> {
        // Re-serialise as {"layer_id": "..."} so the alias in LayerHealth fires.
        let json = serde_json::json!({ "layer_id": p.layer_id, "status": map_proto_status(p.status) });
        serde_json::from_value(json).map_err(crate::error::FrontendError::Deserialise)
    }
}

fn map_proto_status(ordinal: i32) -> &'static str {
    match ordinal {
        0 => "OK",
        1 => "DEGRADED",
        2 => "HALTED",
        3 => "RECOVERING",
        _ => "UNKNOWN",
    }
}