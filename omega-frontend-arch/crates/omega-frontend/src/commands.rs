// crates/omega-frontend/src/commands.rs
use omega_control_contracts::{
    rest::ConfigReloadRequest,
    routes::{self, ApiRoute, AuthTier},
};

#[derive(Debug, Clone)]
pub struct CommandDescriptor {
    pub path:   String,
    pub method: &'static str,
    pub body:   Option<String>,
    pub auth:   AuthTier,
}

#[derive(Debug, Clone)]
pub enum OmegaCommand {
    ReloadConfig(ConfigReloadRequest),
    RevertGasModel     { version: u64 },
    UnpauseGasModel,
    RefreshHealth,
    RefreshCheckpoints,
    RefreshDaoFee,
    RefreshBlacklist,
    RefreshCeilingStatus,
}

impl OmegaCommand {
    pub fn resolve(&self) -> Result<CommandDescriptor, serde_json::Error> {
        match self {
            OmegaCommand::ReloadConfig(req) => Ok(CommandDescriptor {
                path:   routes::CONFIG_RELOAD.path.into(),
                method: routes::CONFIG_RELOAD.method_str(),
                body:   Some(serde_json::to_string(req)?),
                auth:   AuthTier::L2,
            }),
            OmegaCommand::RevertGasModel { version } => Ok(CommandDescriptor {
                path:   routes::revert_checkpoint_path(*version),
                method: "POST",
                body:   None,
                auth:   AuthTier::L2,
            }),
            OmegaCommand::UnpauseGasModel => Ok(CommandDescriptor {
                path:   routes::UNPAUSE_MODEL.path.into(),
                method: routes::UNPAUSE_MODEL.method_str(),
                body:   None,
                auth:   AuthTier::L2,
            }),
            OmegaCommand::RefreshHealth => Ok(CommandDescriptor {
                path:   routes::HEALTH.path.into(),
                method: routes::HEALTH.method_str(),
                body:   None,
                auth:   AuthTier::Public,
            }),
            OmegaCommand::RefreshCheckpoints => Ok(CommandDescriptor {
                path:   routes::CHECKPOINTS.path.into(),
                method: routes::CHECKPOINTS.method_str(),
                body:   None,
                auth:   AuthTier::L1,
            }),
            OmegaCommand::RefreshDaoFee => Ok(CommandDescriptor {
                path:   routes::DAO_FEE.path.into(),
                method: routes::DAO_FEE.method_str(),
                body:   None,
                auth:   AuthTier::L1,
            }),
            OmegaCommand::RefreshBlacklist => Ok(CommandDescriptor {
                path:   routes::BUILDER_BLACKLIST.path.into(),
                method: routes::BUILDER_BLACKLIST.method_str(),
                body:   None,
                auth:   AuthTier::L1,
            }),
            OmegaCommand::RefreshCeilingStatus => Ok(CommandDescriptor {
                path:   routes::CEILING_STATUS.path.into(),
                method: routes::CEILING_STATUS.method_str(),
                body:   None,
                auth:   AuthTier::L1,
            }),
        }
    }
}

// Add method_str() helper to ApiRoute via an extension trait so we don't
// modify the contracts crate.
trait ApiRouteExt {
    fn method_str(&self) -> &'static str;
}

impl ApiRouteExt for ApiRoute {
    fn method_str(&self) -> &'static str {
        match self.method {
            omega_control_contracts::routes::HttpMethod::Get  => "GET",
            omega_control_contracts::routes::HttpMethod::Post => "POST",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_config_resolves() {
        let cmd = OmegaCommand::ReloadConfig(ConfigReloadRequest {
            from_disk: true,
            body: None,
        });
        let desc = cmd.resolve().unwrap();
        assert_eq!(desc.path, "/api/v1/config");
        assert!(matches!(desc.auth, AuthTier::L2));
    }

    #[test]
    fn revert_path_expansion() {
        let cmd = OmegaCommand::RevertGasModel { version: 7 };
        let desc = cmd.resolve().unwrap();
        assert_eq!(desc.path, "/api/v1/la/gas-model/revert/7");
    }

    #[test]
    fn l2_commands_are_marked_l2() {
        let cmds: Vec<OmegaCommand> = vec![
            OmegaCommand::ReloadConfig(ConfigReloadRequest { from_disk: false, body: None }),
            OmegaCommand::RevertGasModel { version: 0 },
            OmegaCommand::UnpauseGasModel,
        ];
        for cmd in cmds {
            let desc = cmd.resolve().unwrap();
            assert!(matches!(desc.auth, AuthTier::L2), "{:?} must be L2", desc.path);
        }
    }
}