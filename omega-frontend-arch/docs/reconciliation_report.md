# omega-frontend-arch/docs/reconciliation_report.md
# Omega Frontend Contract Reconciliation Report
**Date:** 2026-05-30  
**Spec Version:** OmegaEngine v12.0 — Final Edition  
**Report Version:** 1.0

---

## 1. Alignment Matrix

The table below maps every backend contract element to its frontend
implementation in `omega-control-contracts` and `omega-frontend`.

| # | Backend Contract | Frontend Module | Status | Notes |
|---|-----------------|-----------------|--------|-------|
| 1 | `omega_control.proto` messages | `contracts::proto::ProtoHealthReport` + conversion to `LayerHealth` | ✅ Aligned | Feature-gated (`proto`); `protoc` blocked in env — hand-mirrored |
| 2 | `GET /health` | `routes::LIVENESS` → `AuthTier::Public` | ✅ Aligned | |
| 3 | `GET /api/v1/health` | `routes::HEALTH`, `rest::HealthSnapshot` | ✅ Aligned | Auth: Public (REST active behaviour) |
| 4 | `GET /api/v1/config` | `routes::CONFIG`, `commands::OmegaCommand::RefreshHealth` | ✅ Aligned | Read path |
| 5 | `POST /api/v1/config` | `routes::CONFIG_RELOAD`, `rest::ConfigReloadRequest` | ✅ Aligned | L2FastSig |
| 6 | `GET /api/v1/la/gas-model/checkpoints` | `routes::CHECKPOINTS`, `rest::CheckpointsResponse` | ✅ Aligned | Response type owned here (not omega-loss-attribution) |
| 7 | `POST /api/v1/la/gas-model/revert/{version}` | `routes::revert_checkpoint_path(v)`, `rest::RevertResponse` | ✅ Aligned | Path helper tested |
| 8 | `GET /api/v1/la/gas-model/ceiling-status` | `routes::CEILING_STATUS`, `rest::CeilingStatusResponse` | ✅ Aligned | |
| 9 | `POST /api/v1/la/gas-model/unpause` | `routes::UNPAUSE_MODEL`, `rest::ApiOk` | ✅ Aligned | |
| 10 | `GET /api/v1/vault/dao-fee` | `routes::DAO_FEE`, `rest::DaoFeeResponse` | ✅ Aligned | |
| 11 | `GET /api/v1/builders/blacklist` | `routes::BUILDER_BLACKLIST`, `rest::BlacklistResponse` | ✅ Aligned | |
| 12 | `POST /api/v1/builders/blacklist/update` | `routes::BUILDER_BLACKLIST_RELOAD`, `rest::ApiOk` | ✅ Aligned | |
| 13 | `ws://.../ws/events` | `routes::WS_EVENTS`, `ws::WsEvent` | ⚠️ Partially Aligned | Endpoint not yet mounted — see Ambiguity A1 |

**Summary:** 12/13 contracts fully aligned. 1 pending backend wiring.

---

## 2. Ambiguities Detected and Resolved

### A1 — WebSocket Endpoint Not Mounted (OPEN)

**Source:** Reconciliation doc §"Ambiguities Detected"; `main.rs` analysis.

**Finding:** The WebSocket endpoint `GET /ws/events` is defined in the modular
`ops/control-plane/src/ws.rs` handler but is **not mounted** in the active
monolithic `main.rs` router. The modular handler files (`auth/`, `state/`,
`handlers/`, `ws/`, `grpc/`) are not authoritative until wired.

**Frontend Resolution:**
- `routes::WS_EVENTS` documents the spec-intended path.
- `sync::SyncEngine` attempts connection on startup and handles `FrontendError::WsEndpointNotMounted` by setting `WsConnectionStatus::Unavailable`.
- REST polling (`poll_health` every 2s) continues regardless as the fallback.
- The UI renders `ws_status_label = "WS unavailable — polling"` until the backend wires the handler.

**Required Backend Action:** Wire `ws.rs` into `main.rs` router. No frontend
changes needed — the typed sync architecture is ready.

---

### A2 — Proto vs REST Field Name: `layer_id` vs `layer` (RESOLVED)

**Source:** Reconciliation doc §"Ambiguities Detected".

**Finding:** `omega_control.proto` uses `layers[].layer_id`. REST `GET
/api/v1/health` returns `layers[].layer`. Incompatible field names for what
is semantically the same field.

**Frontend Resolution:** `health::LayerHealth` serialises as `"layer"` (REST
canonical) and accepts `"layer_id"` as a `#[serde(alias)]`. Both wire formats
deserialise correctly into the same type. No runtime branching.

**Recommendation:** Align the proto field name to `layer` in the next proto
revision, or add a `json_name` option. The serde alias can be removed once
aligned.

---

### A3 — Auth Tier Mismatch: Proto L1 vs REST Public (RESOLVED)

**Source:** Reconciliation doc §"Ambiguities Detected".

**Finding:** `omega_control.proto` documents all Get RPCs as L1 authenticated.
Active REST `GET /api/v1/health` is public (no Bearer token required).

**Frontend Resolution:** `routes::HEALTH` is set to `AuthTier::Public`,
matching the active REST behaviour. `routes.rs` contains an inline comment
documenting the divergence. The proto auth tier is treated as a future gRPC
concern, not the deployed REST API.

**Recommendation:** Decide authoritatively: is `/api/v1/health` public by
design (K8s probe compatibility), or should it require auth? Document in v12
control-plane spec. If public is intentional, update the proto comment.

---

### A4 — 14-Layer vs 16-Layer Count in `main.rs` (RESOLVED)

**Source:** Reconciliation doc §"Ambiguities Detected".

**Finding:** Comments in `main.rs` reference 14 layers. The authoritative
`LayerId` enum and state list include 16 layers.

**Frontend Resolution:** `health::LayerHealth` implements 16 layers.
A compile-time test `all_16_layers_present()` asserts `LayerId::iter().count() == 16`.
The stale comments are noted in `health.rs` with the exact explanation.

**Required Backend Action:** Update stale `main.rs` comments to reference 16
layers. No functional change needed.

---

### A5 — Monolithic vs Modular Control-Plane Entrypoint (OPEN)

**Source:** Reconciliation doc §"Ambiguities Detected".

**Finding:** The binary currently uses the monolithic `main.rs` path. Modular
handler files are not fully authoritative until wired. This affects WebSocket
and gRPC availability (see A1).

**Frontend Impact:** REST routes in `routes.rs` map to the monolithic path
(confirmed active). The frontend makes no assumptions about gRPC availability
— the `proto` feature is opt-in. When the modular path is activated,
`proto::ProtoHealthReport` and the TryFrom conversion to `LayerHealth` are
already in place.

**Required Backend Action:** Wire modular handlers into `main.rs`. Document
which is the canonical entrypoint going forward.

---

## 3. Schema Duplication Eliminated

The following duplications from the pre-reconciliation state have been resolved:

| Previous Duplication | Resolution |
|----------------------|-----------|
| REST DTO definitions scattered across control-plane entrypoint | All DTOs centralised in `omega-control-contracts::rest` |
| Route strings defined inline in fetch calls | All paths defined once in `omega-control-contracts::routes` |
| `LayerId`/`HealthStatus` enums reimplemented per consumer | Single canonical source in `omega-control-contracts::health` |
| WebSocket event shapes undocumented | Fully typed in `omega-control-contracts::ws::WsEvent` |

---

## 4. Compile-Time Validation Coverage

| Invariant | Enforcement |
|-----------|-------------|
| All 16 layers present | `#[test] fn all_16_layers_present()` — fails at compile/test if layer added without updating enum |
| No duplicate route (path, method) pairs | `#[test] fn no_duplicate_paths_for_same_method()` |
| Revert path expands correctly | `#[test] fn revert_path_expansion()` |
| All wire types round-trip through serde | One round-trip test per DTO in `rest.rs`, `ws.rs`, `health.rs` |
| Render frame is deterministic | `#[test] fn frame_is_deterministic()` — derives two frames from same state, asserts equality |
| Stale snapshots are dropped | `#[test] fn stale_snapshot_returns_none()` |
| Ping WS events produce no state change | `#[test] fn ping_event_returns_none()` |
| Rate limit constants match spec | `#[test] fn rate_limit_constants_match_spec()` — asserts 300/100 |

---

## 5. Outstanding Backend Actions Required

| Priority | Action | Blocking Frontend? |
|----------|--------|--------------------|
| High | Wire `ws.rs` modular handler into `main.rs` | No — frontend degrades gracefully to REST polling |
| Medium | Decide authoritatively on `GET /api/v1/health` auth tier | No — Public works today |
| Medium | Align proto `layer_id` field name to `layer` | No — serde alias handles both |
| Low | Update stale 14-layer comments in `main.rs` | No |

---

## 6. Architecture Invariants (Compile-Enforced)

1. **No unsafe code** — `#![forbid(unsafe_code)]` in both crates.
2. **No browser APIs in core** — `omega-frontend` core modules have zero
   platform dependencies. WASM and native build identically.
3. **Deterministic rendering** — `render::derive_frame` is a pure function.
   Same state → same frame, always.
4. **Monotonic revision** — `EngineState::accept_health_snapshot` drops
   snapshots with revision ≤ current. `apply_ws_event` increments revision
   on every accepted event.
5. **Single route source** — `routes.rs` is the only file that defines path
   strings. All other code imports constants.
6. **No inline JSON shapes** — all wire types are imported from
   `omega-control-contracts`. No ad-hoc `serde_json::json!` in business logic.