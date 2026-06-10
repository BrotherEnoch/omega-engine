use omega_control_contracts::routes;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrontendCommand {
    RefreshHealth,
    RefreshConfig,
    ReloadConfig,
    ListCheckpoints,
    RevertCheckpoint { version: u64 },
    RefreshCeilingStatus,
    UnpauseModel,
    RefreshDaoFee,
    RefreshBuilderBlacklist,
    ReloadBuilderBlacklist,
    ClearHalt { layer: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestSpec {
    pub method: routes::HttpMethod,
    pub path: String,
    pub auth: routes::AuthTier,
}

impl FrontendCommand {
    pub fn request(&self) -> RequestSpec {
        match self {
            Self::RefreshHealth => route(routes::HEALTH),
            Self::RefreshConfig => route(routes::CONFIG),
            Self::ReloadConfig => route(routes::CONFIG_RELOAD),
            Self::ListCheckpoints => route(routes::CHECKPOINTS),
            Self::RevertCheckpoint { version } => RequestSpec {
                method: routes::HttpMethod::Post,
                path: routes::revert_checkpoint_path(*version),
                auth: routes::AuthTier::L2,
            },
            Self::RefreshCeilingStatus => route(routes::CEILING_STATUS),
            Self::UnpauseModel => route(routes::UNPAUSE_MODEL),
            Self::RefreshDaoFee => route(routes::DAO_FEE),
            Self::RefreshBuilderBlacklist => route(routes::BUILDER_BLACKLIST),
            Self::ReloadBuilderBlacklist => route(routes::BUILDER_BLACKLIST_RELOAD),
            Self::ClearHalt { layer } => RequestSpec {
                method: routes::HttpMethod::Post,
                path: routes::clear_halt_path(layer),
                auth: routes::AuthTier::L2,
            },
        }
    }
}

fn route(route: routes::ApiRoute) -> RequestSpec {
    RequestSpec {
        method: route.method,
        path: route.path.to_string(),
        auth: route.auth,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_commands_are_marked_l2() {
        assert_eq!(
            FrontendCommand::UnpauseModel.request().auth,
            routes::AuthTier::L2
        );
        assert_eq!(
            FrontendCommand::RevertCheckpoint { version: 7 }.request().auth,
            routes::AuthTier::L2
        );
    }
}
