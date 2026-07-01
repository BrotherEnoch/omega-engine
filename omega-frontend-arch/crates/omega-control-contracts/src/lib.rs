// omega-frontend-arch/crates/omega-control-contracts/src/lib.rs
#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod error;
pub mod health;
#[cfg(feature = "proto")]
pub mod proto;
pub mod rest;
pub mod routes;
pub mod ws;

pub use error::{ApiError, ApiErrorCode, FrontendError};
pub use health::{HealthStatus, LayerId, LayerHealth};
pub use rest::{HealthSnapshot, ApiOk};
pub use routes::{ApiRoute, AuthTier};
pub use ws::{WsEvent, WsRateLimit, WsConnectionStatus, layer_id_from_wire};