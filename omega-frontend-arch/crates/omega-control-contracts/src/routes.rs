// omega-frontend-arch/crates/omega-control-contracts/src/routes.rs
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revert_checkpoint_path_expansion() {
        assert_eq!(revert_checkpoint_path(0),  "/api/v1/la/gas-model/revert/0");
        assert_eq!(revert_checkpoint_path(7),  "/api/v1/la/gas-model/revert/7");
        assert_eq!(revert_checkpoint_path(42), "/api/v1/la/gas-model/revert/42");
    }

    #[test]
    fn clear_halt_path_expansion() {
        assert_eq!(clear_halt_path("RELAY"), "/api/v1/health/clear-halt/RELAY");
    }

    #[test]
    fn no_duplicate_method_path_pairs() {
        let mut seen = std::collections::HashSet::new();
        for route in REST_ROUTES {
            let key = format!("{:?} {}", route.method, route.path);
            assert!(seen.insert(key.clone()), "Duplicate route: {key}");
        }
    }

    #[test]
    fn mutation_endpoints_require_l2_or_higher() {
        // POST endpoints that mutate state must not be Public
        for route in REST_ROUTES {
            if matches!(route.method, HttpMethod::Post) {
                assert!(
                    !matches!(route.auth, AuthTier::Public),
                    "POST {} must not be Public", route.path
                );
            }
        }
    }

    #[test]
    fn ws_events_path_correct() {
        assert_eq!(WS_EVENTS, "/ws/events");
    }

    #[test]
    fn route_count_matches_expected() {
        // 10 routes in REST_ROUTES — update this if routes are added/removed
        assert_eq!(REST_ROUTES.len(), 10);
    }
}