use omega_control_contracts::ws::{WsAuthFrame, WsClientFrame, WsEvent};

use crate::state::{EngineStore, RealtimeStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncAction {
    Connect,
    SendAuth(WsClientFrame),
    MarkAuthenticated,
    MarkAnonymous,
    MarkLagged,
    Disconnect,
}

pub fn auth_frame(token: impl Into<String>) -> WsClientFrame {
    WsClientFrame::Auth {
        token: token.into(),
    }
}

pub fn apply_auth_frame(store: &mut EngineStore, frame: WsAuthFrame) {
    match frame {
        WsAuthFrame::AuthOk { .. } => store.set_realtime_status(RealtimeStatus::Authenticated),
        WsAuthFrame::AuthFailed { .. } => store.set_realtime_status(RealtimeStatus::Anonymous),
    }
}

pub fn apply_ws_event(store: &mut EngineStore, event: WsEvent) {
    store.apply_event(event);
}
