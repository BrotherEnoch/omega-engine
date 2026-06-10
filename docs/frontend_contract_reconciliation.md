# frontend_contract_reconciliation.md
# Omega Frontend Contract Reconciliation

Date: 2026-05-30

## Authoritative Backend Sources

- gRPC: `ops/control-plane/proto/omega_control.proto`
- REST routes: `ops/control-plane/src/main.rs` and the modular handler files under `ops/control-plane/src/handlers`
- Realtime stream: `ops/control-plane/src/ws.rs` and `ops/control-plane/src/state.rs`
- Shared core enums: `crates/omega-core/src/types/health.rs`

## Implemented Frontend Architecture

- `crates/omega-control-contracts` is the shared contract crate.
- `crates/omega-control-contracts/src/proto.rs` mirrors the generated `prost` message shape from `omega_control.proto`. Fresh generation is blocked in this local environment because invoking `protoc` returns `Access is denied`.
- `crates/omega-control-contracts/src/rest.rs` owns the JSON REST DTOs used by the frontend.
- `crates/omega-control-contracts/src/ws.rs` owns the realtime event DTOs and rate-limit constants.
- `crates/omega-control-contracts/src/routes.rs` owns canonical route paths, HTTP methods, and auth tiers.
- `crates/omega-frontend` provides WASM-compatible state, command, realtime sync, and deterministic render-frame modules.
- The active control-plane entrypoint imports shared REST DTOs from `omega-control-contracts` for health, config reload, checkpoint revert, DAO fee, blacklist, and API status/error payloads.

## Alignment Matrix

| Backend contract | Frontend/shared implementation | Status |
| --- | --- | --- |
| `omega_control.proto` messages | `omega_control_contracts::proto::*` prost messages matching backend generated output | Aligned |
| `GET /health` | `routes::LIVENESS` | Aligned |
| `GET /api/v1/health` | `routes::HEALTH`, `rest::HealthSnapshot` | Aligned |
| `GET/POST /api/v1/config` | `routes::CONFIG`, `routes::CONFIG_RELOAD`, `rest::ConfigReloadRequest` | Aligned |
| `GET /api/v1/la/gas-model/checkpoints` | `routes::CHECKPOINTS` | Route aligned; response type remains owned by `omega-loss-attribution` |
| `POST /api/v1/la/gas-model/revert/{version}` | `routes::revert_checkpoint_path`, `rest::RevertResponse` | Aligned |
| `GET /api/v1/la/gas-model/ceiling-status` | `routes::CEILING_STATUS` | Route aligned; response type remains owned by `omega-loss-attribution` |
| `POST /api/v1/la/gas-model/unpause` | `routes::UNPAUSE_MODEL`, `rest::ApiOk` | Aligned |
| `GET /api/v1/vault/dao-fee` | `routes::DAO_FEE`, `rest::DaoFeeResponse` | Aligned |
| `GET /api/v1/builders/blacklist` | `routes::BUILDER_BLACKLIST`, `rest::BlacklistResponse` | Aligned |
| `POST /api/v1/builders/blacklist/update` | `routes::BUILDER_BLACKLIST_RELOAD`, `rest::ApiOk` | Aligned |
| `ws://.../ws/events` | `routes::WS_EVENTS`, `ws::WsEvent` | Aligned |

## Ambiguities Detected

- The control-plane has both a monolithic `main.rs` implementation and modular `auth/state/handlers/ws/grpc` files. The binary currently uses the monolithic path, so the modular WebSocket and gRPC files are not fully authoritative until wired into `main.rs`.
- `omega_control.proto` says `HealthReport.layers[].layer_id`, while the REST endpoint returns `layers[].layer`. This is compatible at transport level but not a single field naming convention.
- `omega_control.proto` documents all Get RPCs as L1 authenticated, while REST `GET /api/v1/health` is public in the active backend. The frontend route table preserves the active REST behavior and the proto behavior separately.
- Comments in `main.rs` still mention 14 layers in places, while the current `LayerId` enum and state list include 16 layers.
- The backend spec describes WebSocket `GET /ws/events`, but the active monolithic router does not mount that route. The frontend has the typed sync architecture ready for it.

## Compatibility Notes

- No backend route path was changed.
- Shared frontend commands resolve to the existing backend route strings.
- The active backend REST DTO definitions were reconciled to shared contract imports to reduce schema duplication.
- Realtime state updates are deterministic: every accepted snapshot or event increments a monotonic frontend revision, and render frames are derived from immutable snapshot state.
- Frontend code avoids browser-specific dependencies in the core architecture, keeping it usable from WASM UI frameworks and native test harnesses.
