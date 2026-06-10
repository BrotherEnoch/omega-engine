// omega-frontend-arch/crates/omega-runtime/src/health.rs
// Production-grade FSM + transport-safe DTO model
//
// Design goals:
// - zero transport/runtime leakage
// - no Instant serialization
// - minimal steady-state allocations
// - branch-predictable FSM
// - cache-friendly DTOs
// - websocket/snapshot safe

use serde::{Deserialize, Serialize};
use std::{
borrow::Cow,
collections::HashMap,
time::{Duration, Instant},
};

/// ============================================================
/// FSM STATUS
/// ============================================================

#[derive(
Debug,
Clone,
Copy,
PartialEq,
Eq,
Serialize,
Deserialize,
)]
#[repr(u8)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HealthStatus {
Unknown = 0,
Starting = 1,
Healthy = 2,
Degraded = 3,
Stale = 4,
Failed = 5,
Stopped = 6,
}

impl HealthStatus {
#[inline(always)]
pub const fn is_healthy(self) -> bool {
matches!(self, Self::Healthy)
}

#[inline(always)]
pub const fn is_terminal(self) -> bool {
    matches!(self, Self::Failed | Self::Stopped)
}

}

impl std::fmt::Display for HealthStatus {
#[inline(always)]
fn fmt(
&self,
f: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
f.write_str(match self {
Self::Unknown => "UNKNOWN",
Self::Starting => "STARTING",
Self::Healthy => "HEALTHY",
Self::Degraded => "DEGRADED",
Self::Stale => "STALE",
Self::Failed => "FAILED",
Self::Stopped => "STOPPED",
})
}
}

/// ============================================================
/// STATIC FSM MESSAGES
/// ============================================================

const MSG_EMPTY: &str = "";
const MSG_HEALTHY: &str = "healthy";
const MSG_STALE: &str = "heartbeat stale";
const MSG_TIMEOUT: &str = "heartbeat timeout";

/// ============================================================
/// INTERNAL RUNTIME FSM STATE
/// ============================================================
///
/// INTERNAL ONLY.
/// NEVER SERIALIZE DIRECTLY.
///
#[derive(Debug, Clone)]
pub struct LayerHealth {
pub status: HealthStatus,

/// process-local monotonic clock
pub last_heartbeat: Option<Instant>,

/// nanoseconds
pub latency_ns: u64,

/// allocation-free steady-state messages
pub message: Cow<'static, str>,

}

impl Default for LayerHealth {
#[inline(always)]
fn default() -> Self {
Self {
status: HealthStatus::Unknown,
last_heartbeat: None,
latency_ns: 0,
message: Cow::Borrowed(MSG_EMPTY),
}
}
}

impl LayerHealth {
#[inline(always)]
pub fn is_healthy(&self) -> bool {
self.status.is_healthy()
}

#[inline(always)]
pub fn is_terminal(&self) -> bool {
    self.status.is_terminal()
}

/// Fast-path heartbeat update.
///
/// Optimized for extremely high event rates.
#[inline(always)]
pub fn record_heartbeat(
    &mut self,
    latency_ns: u64,
) {
    self.last_heartbeat = Some(Instant::now());
    self.latency_ns = latency_ns;

    self.status = HealthStatus::Healthy;
    self.message = Cow::Borrowed(MSG_HEALTHY);
}

/// Manual subsystem degradation.
#[inline(always)]
pub fn mark_degraded(
    &mut self,
    msg: impl Into<Cow<'static, str>>,
) {
    self.status = HealthStatus::Degraded;
    self.message = msg.into();
}

#[inline(always)]
pub fn stop(&mut self) {
    self.status = HealthStatus::Stopped;
}

/// Automatic FSM progression.
///
/// Zero allocation in steady-state path.
#[inline(always)]
pub fn tick(
    &mut self,
    stale_after: Duration,
    fail_after: Duration,
) {
    if self.is_terminal() {
        return;
    }

    let Some(last) = self.last_heartbeat else {
        return;
    };

    let age = last.elapsed();

    if age >= fail_after {
        if self.status != HealthStatus::Failed {
            self.status = HealthStatus::Failed;
            self.message = Cow::Borrowed(MSG_TIMEOUT);
        }

        return;
    }

    if age >= stale_after {
        if self.status != HealthStatus::Stale {
            self.status = HealthStatus::Stale;
            self.message = Cow::Borrowed(MSG_STALE);
        }

        return;
    }

    if self.status != HealthStatus::Healthy {
        self.status = HealthStatus::Healthy;
        self.message = Cow::Borrowed(MSG_HEALTHY);
    }
}

/// Runtime FSM -> transport DTO
#[inline(always)]
pub fn to_dto(&self) -> LayerHealthDto {
    LayerHealthDto {
        status: self.status,
        latency_ns: self.latency_ns,
        message: self.message.clone().into_owned(),
    }
}

}

/// ============================================================
/// TRANSPORT DTOs
/// ============================================================

#[derive(
Debug,
Clone,
Serialize,
Deserialize,
)]
pub struct LayerHealthDto {
pub status: HealthStatus,
pub latency_ns: u64,
pub message: String,
}

#[derive(
Debug,
Clone,
Serialize,
Deserialize,
)]
pub struct HealthResponse {
pub status: HealthStatus,
}

#[derive(
Debug,
Clone,
Serialize,
Deserialize,
)]
pub struct SnapshotResponse {
pub version: u64,
pub layers: HashMap<String, LayerHealthDto>,
}

impl SnapshotResponse {
    /// Build a full snapshot from runtime FSM state.
    /// This is pure transformation (no side effects).
    #[inline(always)]
    pub fn from_runtime(
        version: u64,
        layers: &HashMap<String, LayerHealth>,
    ) -> Self {
        let mut out = HashMap::with_capacity(layers.len());

        for (id, layer) in layers {
            out.insert(id.clone(), layer.to_dto());
        }

        Self {
            version,
            layers: out,
        }
    }
}