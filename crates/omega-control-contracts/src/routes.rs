use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HttpMethod {
    Get,
    Post,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthTier {
    Public,
    L1,
    L2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiRoute {
    pub method: HttpMethod,
    pub path: &'static str,
    pub auth: AuthTier,
}

pub const LIVENESS: ApiRoute = ApiRoute {
    method: HttpMethod::Get,
    path: "/health",
    auth: AuthTier::Public,
};

pub const HEALTH: ApiRoute = ApiRoute {
    method: HttpMethod::Get,
    path: "/api/v1/health",
    auth: AuthTier::Public,
};

pub const CONFIG: ApiRoute = ApiRoute {
    method: HttpMethod::Get,
    path: "/api/v1/config",
    auth: AuthTier::L1,
};

pub const CONFIG_RELOAD: ApiRoute = ApiRoute {
    method: HttpMethod::Post,
    path: "/api/v1/config",
    auth: AuthTier::L1,
};

pub const CHECKPOINTS: ApiRoute = ApiRoute {
    method: HttpMethod::Get,
    path: "/api/v1/la/gas-model/checkpoints",
    auth: AuthTier::L1,
};

pub const REVERT_CHECKPOINT_PREFIX: &str = "/api/v1/la/gas-model/revert/";

pub const CEILING_STATUS: ApiRoute = ApiRoute {
    method: HttpMethod::Get,
    path: "/api/v1/la/gas-model/ceiling-status",
    auth: AuthTier::L1,
};

pub const UNPAUSE_MODEL: ApiRoute = ApiRoute {
    method: HttpMethod::Post,
    path: "/api/v1/la/gas-model/unpause",
    auth: AuthTier::L2,
};

pub const DAO_FEE: ApiRoute = ApiRoute {
    method: HttpMethod::Get,
    path: "/api/v1/vault/dao-fee",
    auth: AuthTier::L1,
};

pub const BUILDER_BLACKLIST: ApiRoute = ApiRoute {
    method: HttpMethod::Get,
    path: "/api/v1/builders/blacklist",
    auth: AuthTier::L1,
};

pub const BUILDER_BLACKLIST_RELOAD: ApiRoute = ApiRoute {
    method: HttpMethod::Post,
    path: "/api/v1/builders/blacklist/update",
    auth: AuthTier::L2,
};

pub const HEALTH_CLEAR_HALT_PREFIX: &str = "/api/v1/health/clear-halt/";
pub const WS_EVENTS: &str = "/ws/events";

pub const REST_ROUTES: &[ApiRoute] = &[
    LIVENESS,
    HEALTH,
    CONFIG,
    CONFIG_RELOAD,
    CHECKPOINTS,
    CEILING_STATUS,
    UNPAUSE_MODEL,
    DAO_FEE,
    BUILDER_BLACKLIST,
    BUILDER_BLACKLIST_RELOAD,
];

pub fn revert_checkpoint_path(version: u64) -> String {
    format!("{REVERT_CHECKPOINT_PREFIX}{version}")
}

pub fn clear_halt_path(layer: &str) -> String {
    format!("{HEALTH_CLEAR_HALT_PREFIX}{layer}")
}
