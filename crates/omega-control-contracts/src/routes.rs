// omega-engine\crates\omega-control-contracts\src\routes.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HttpMethod {
    Get,
    Post,
}

/// CHANGE: added `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]`, matching
/// `HttpMethod` above. Before this, `ApiRoute` (which holds both an
/// `HttpMethod` and an `AuthTier` field) would serialize as
/// `{"method":"GET","auth":"Public"}` — two different casing conventions in
/// the same object. Now both are `SCREAMING_SNAKE_CASE`
/// (`"PUBLIC"`/`"L1"`/`"L2"`). Verified against a real serialize call, not
/// just written. BREAKING for any JSON consumer expecting the old
/// `"Public"`/`"L1"`/`"L2"` PascalCase casing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
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

/// Static routes only. `REVERT_CHECKPOINT_PREFIX` and
/// `HEALTH_CLEAR_HALT_PREFIX` are deliberately not included here — both are
/// dynamically-parameterized (`/revert/{version}`, `/clear-halt/{layer}`) and
/// don't fit the static-`ApiRoute`-const pattern the rest of this list uses.
/// If you're consuming this array to generate docs or mount routes, you need
/// those two prefixes separately, not just this array.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_and_auth_use_matching_casing() {
        let json = serde_json::to_string(&LIVENESS).unwrap();
        assert!(json.contains("\"method\":\"GET\""));
        assert!(json.contains("\"auth\":\"PUBLIC\""));
    }

    #[test]
    fn l2_route_casing() {
        let json = serde_json::to_string(&UNPAUSE_MODEL).unwrap();
        assert!(json.contains("\"auth\":\"L2\""));
    }
}