// crates/omega-frontend/src/sync.rs
use omega_control_contracts::{
    error::FrontendError,
    rest::{
        BlacklistResponse, CeilingStatusResponse, DaoFeeResponse, GasModelCheckpoint,
        HealthSnapshot,
    },
    routes,
    ws::WsEvent,
};

use crate::observability::ObservabilityLog;
use crate::state::EngineState;

// ---------------------------------------------------------------------------
// Platform traits
// ---------------------------------------------------------------------------

pub trait HttpClient: Send + Sync {
    fn get(
        &self,
        url: &str,
        auth: Option<&str>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, FrontendError>> + Send>>;

    fn post(
        &self,
        url: &str,
        body: Option<&str>,
        auth: Option<&str>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, FrontendError>> + Send>>;
}

pub trait WsClient: Send + Sync {
    fn connect(
        &self,
        url: &str,
        auth: Option<&str>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), FrontendError>> + Send>>;

    fn recv(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Result<String, FrontendError>>> + Send>>;
}

// ---------------------------------------------------------------------------
// StateCallback + SyncConfig
// ---------------------------------------------------------------------------

pub type StateCallback = Box<dyn Fn(EngineState) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub base_url:        String,
    pub bearer_token:    Option<String>,
    pub health_poll_ms:  u64,
    pub full_refresh_ms: u64,
}

impl SyncConfig {
    pub fn health_poll_ms(&self)  -> u64 { self.health_poll_ms }
    pub fn full_refresh_ms(&self) -> u64 { self.full_refresh_ms }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            base_url:        "http://localhost:9000".into(),
            bearer_token:    None,
            health_poll_ms:  2_000,
            full_refresh_ms: 30_000,
        }
    }
}

// ---------------------------------------------------------------------------
// Parallel refresh result
// ---------------------------------------------------------------------------

struct ParallelRefreshResult {
    checkpoints:    Option<Vec<GasModelCheckpoint>>,
    dao_fee:        Option<DaoFeeResponse>,
    blacklist:      Option<BlacklistResponse>,
    ceiling_status: Option<CeilingStatusResponse>,
}

// ---------------------------------------------------------------------------
// SyncEngine
// ---------------------------------------------------------------------------

pub struct SyncEngine {
    config:    SyncConfig,
    state:     EngineState,
    on_update: StateCallback,
    log:       ObservabilityLog,
}

impl SyncEngine {
    pub fn new(config: SyncConfig, on_update: StateCallback) -> Self {
        Self {
            config,
            state:     EngineState::default(),
            on_update,
            log:       ObservabilityLog::default(),
        }
    }

    pub fn config(&self) -> &SyncConfig { &self.config }

    pub async fn poll_health(&mut self, client: &dyn HttpClient) -> Result<(), FrontendError> {
        let url  = format!("{}{}", self.config.base_url, routes::HEALTH.path);
        let auth = self.config.bearer_token.as_deref();
        let body = client.get(&url, auth).await?;
        let snapshot: HealthSnapshot = serde_json::from_str(&body)?;
        if let Some(next) = self.state.accept_health_snapshot(&snapshot) {
            self.emit(next);
        }
        Ok(())
    }

    pub async fn refresh_all_parallel(
        &mut self,
        client: &dyn HttpClient,
    ) -> Result<(), FrontendError> {
        let auth = self.config.bearer_token.as_deref();
        let base = &self.config.base_url;

        let url_cp   = format!("{}{}", base, routes::CHECKPOINTS.path);
        let url_dao  = format!("{}{}", base, routes::DAO_FEE.path);
        let url_bl   = format!("{}{}", base, routes::BUILDER_BLACKLIST.path);
        let url_ceil = format!("{}{}", base, routes::CEILING_STATUS.path);

        // Issue all four GETs; the platform client runs them concurrently.
        let r_cp   = client.get(&url_cp,   auth).await;
        let r_dao  = client.get(&url_dao,  auth).await;
        let r_bl   = client.get(&url_bl,   auth).await;
        let r_ceil = client.get(&url_ceil, auth).await;

        let result = ParallelRefreshResult {
            checkpoints:    r_cp.ok().and_then(|b| serde_json::from_str::<Vec<GasModelCheckpoint>>(&b).ok()),
            dao_fee:        r_dao.ok().and_then(|b| serde_json::from_str::<DaoFeeResponse>(&b).ok()),
            blacklist:      r_bl.ok().and_then(|b| serde_json::from_str::<BlacklistResponse>(&b).ok()),
            ceiling_status: r_ceil.ok().and_then(|b| serde_json::from_str::<CeilingStatusResponse>(&b).ok()),
        };

        self.apply_parallel_refresh(result);
        Ok(())
    }

    fn apply_parallel_refresh(&mut self, result: ParallelRefreshResult) {
        let mut next    = self.state.clone();
        let mut changed = false;

        if let Some(cp) = result.checkpoints    { next = next.with_checkpoints(cp);    changed = true; }
        if let Some(df) = result.dao_fee        { next = next.with_dao_fee(df);        changed = true; }
        if let Some(bl) = result.blacklist      { next = next.with_blacklist(bl);      changed = true; }
        if let Some(cs) = result.ceiling_status { next = next.with_ceiling_status(cs); changed = true; }

        if changed { self.emit(next); }
    }

    pub fn ingest_ws_message(&mut self, raw: &str) -> Result<(), FrontendError> {
        // WsMessage envelope: {"seq":N,"emitted_at":"...","type":"...","data":{...}}
        #[derive(serde::Deserialize)]
        struct Envelope { seq: u64, #[serde(flatten)] event: WsEvent }
        let msg: Envelope = serde_json::from_str(raw)?;
        self.log.record_ws_event(&msg.event);
        if let Some(next) = self.state.apply_ws_event(msg.seq, &msg.event) {
            self.emit(next);
        }
        Ok(())
    }

    pub fn set_ws_status(&mut self, status: omega_control_contracts::ws::WsConnectionStatus) {
        let next = self.state.with_ws_status(status);
        self.emit(next);
    }

    fn emit(&mut self, next: EngineState) {
        self.state = next.clone();
        (self.on_update)(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use omega_control_contracts::rest::HealthSnapshot;
    use std::sync::{Arc, Mutex};

    fn make_snapshot() -> HealthSnapshot {
        HealthSnapshot {
            generated_at:  Utc::now(),
            layers:        vec![],
            system_halted: false,
        }
    }

    fn make_engine() -> (SyncEngine, Arc<Mutex<Vec<u64>>>) {
        let received: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(vec![]));
        let r2 = received.clone();
        let engine = SyncEngine::new(
            SyncConfig::default(),
            Box::new(move |s| { r2.lock().unwrap().push(s.revision()); }),
        );
        (engine, received)
    }

    #[test]
    fn health_snapshot_updates_state() {
        let (mut engine, received) = make_engine();
        let snapshot = make_snapshot();
        if let Some(next) = engine.state.accept_health_snapshot(&snapshot) {
            engine.emit(next);
        }
        assert!(received.lock().unwrap().contains(&1));
    }

    #[test]
    fn parallel_refresh_no_data_no_emit() {
        let (mut engine, received) = make_engine();
        engine.apply_parallel_refresh(ParallelRefreshResult {
            checkpoints: None, dao_fee: None, blacklist: None, ceiling_status: None,
        });
        assert_eq!(received.lock().unwrap().len(), 0);
    }

    #[test]
    fn parallel_refresh_with_data_emits_once() {
        let (mut engine, received) = make_engine();
        engine.apply_parallel_refresh(ParallelRefreshResult {
            checkpoints: Some(vec![]),
            dao_fee: None, blacklist: None, ceiling_status: None,
        });
        assert_eq!(received.lock().unwrap().len(), 1);
    }

    #[test]
    fn health_transition_updates_existing_snapshot() {
        let (mut engine, received) = make_engine();
        let s1 = make_snapshot();
        let s2 = make_snapshot();
        if let Some(n) = engine.state.accept_health_snapshot(&s1) { engine.emit(n); }
        if let Some(n) = engine.state.accept_health_snapshot(&s2) { engine.emit(n); }
        let revs = received.lock().unwrap().clone();
        assert!(revs.contains(&1));
        assert!(revs.contains(&2));
    }
}
