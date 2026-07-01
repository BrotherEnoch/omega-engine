// omega-frontend-arch/crates/omega-ui/src/app.rs
#![forbid(unsafe_code)]

use std::{cell::RefCell, rc::Rc};

use gloo_timers::callback::Interval;
use leptos::*;
use wasm_bindgen_futures::spawn_local;

use omega_control_contracts::rest::{
    BlacklistResponse, CeilingStatusResponse, DaoFeeResponse,
    GasModelCheckpoint, HealthSnapshot,
};
use omega_frontend::{render::{derive_frame, RenderFrame}, state::EngineState};

use crate::{
    components::{
        alerts::Alerts, footer::Footer, header::Header,
        layer_grid::LayerGrid, obs_panel::ObsPanel,
    },
    sync_adapter::FetchClient,
    ws_client,
};

// Points at ops/control-plane's HTTP API (§17). Previously defaulted to
// the mock omega-runtime's HTTP port (9001) — repointed to the real
// control-plane (8080) now that ws_client.rs also targets it for the
// /ws/events stream. Override at build time with OMEGA_API_URL.
const API_URL: &str = match option_env!("OMEGA_API_URL") {
    Some(url) => url,
    None      => "http://127.0.0.1:8080",
};

// IMPORTANT: option_env! is resolved at COMPILE time, by reading the
// environment of the machine/shell that runs `trunk build` / `trunk serve`.
// It does NOT read ops/control-plane's .env file — that file is only ever
// read by the control-plane server process at ITS runtime, in a totally
// separate process. If OMEGA_API_TOKEN isn't exported in the shell that
// invokes trunk, this constant silently bakes in as "", bearer_token
// becomes None, no Authorization header is ever sent, and every
// authenticated endpoint will 401 (checkpoints, dao-fee, blacklist,
// ceiling-status, etc.) — which is exactly the failure mode this guards
// against below.
//
// Set it before building, e.g.:
//   PowerShell:  $env:OMEGA_API_TOKEN = "your-secret-here"; trunk serve
//   bash/zsh:    OMEGA_API_TOKEN=your-secret-here trunk serve
//
// The value must match ops/control-plane's CONTROL_PLANE_BEARER_SECRET.
//
// NOTE: this constant gets compiled directly into the public .wasm binary
// shipped to the browser, and is recoverable via `strings` on that file.
// That's an acceptable tradeoff for a local-only dev dashboard bound to
// 127.0.0.1, but it is not a safe pattern if this UI is ever served
// somewhere reachable by anyone other than the operator. A login-once
// flow that stores the token only in memory (never compiled in) is the
// correct fix before this UI is exposed beyond localhost.
const API_TOKEN: &str = match option_env!("OMEGA_API_TOKEN") {
    Some(tok) => tok,
    None      => "",
};

const HEALTH_POLL_MS:  u32 = 10_000;
const FULL_REFRESH_MS: u32 = 30_000;

pub type SharedState = Rc<RefCell<EngineState>>;

#[component]
pub fn App() -> impl IntoView {
    let (frame, set_frame) = create_signal(derive_frame(&EngineState::default()));
    let state: SharedState = Rc::new(RefCell::new(EngineState::default()));

    ws_client::start(Rc::clone(&state), set_frame);

    if !API_URL.is_empty() {
        if API_TOKEN.is_empty() {
            // Loud, visible warning instead of silent 401s on every poll.
            // If you see this in the browser console, OMEGA_API_TOKEN was
            // not set in the environment that ran `trunk build`/`trunk serve`.
            web_sys::console::warn_1(
                &"omega-ui: OMEGA_API_TOKEN was empty at build time — \
                  all authenticated /api/v1/* requests will be sent \
                  without an Authorization header and will 401. \
                  Rebuild with OMEGA_API_TOKEN set to match \
                  CONTROL_PLANE_BEARER_SECRET.".into(),
            );
        }

        let client = Rc::new(FetchClient {
            base_url:     API_URL.to_string(),
            bearer_token: if API_TOKEN.is_empty() { None } else { Some(API_TOKEN.to_string()) },
        });
        start_health_polling(Rc::clone(&state), Rc::clone(&client), set_frame);
        start_full_refresh_polling(Rc::clone(&state), Rc::clone(&client), set_frame);
    }

    view! {
        <div class="app">
            <div class="scanlines" aria-hidden="true"></div>
            <Header frame=frame />
            <Alerts frame=frame />
            <Footer frame=frame />
            <LayerGrid frame=frame />
            <ObsPanel frame=frame />
        </div>
    }
}

fn start_health_polling(
    state: SharedState, client: Rc<FetchClient>, set_frame: WriteSignal<RenderFrame>,
) {
    Interval::new(HEALTH_POLL_MS, move || {
        let state  = Rc::clone(&state);
        let client = Rc::clone(&client);
        spawn_local(async move { poll_health_once(state, client, set_frame).await; });
    }).forget();
}

async fn poll_health_once(
    state: SharedState, client: Rc<FetchClient>, set_frame: WriteSignal<RenderFrame>,
) {
    let url = format!("{}/api/v1/health", client.base_url);
    let body = match client.get_local(&url, None).await {
        Ok(b)  => b,
        Err(e) => { web_sys::console::warn_1(&format!("omega-ui: health fetch: {e}").into()); return; }
    };
    let snap = match serde_json::from_str::<HealthSnapshot>(&body) {
        Ok(s)  => s,
        Err(e) => { web_sys::console::warn_1(&format!("omega-ui: health parse: {e}").into()); return; }
    };
    let mut cur = state.borrow_mut();
    if let Some(next) = cur.accept_health_snapshot(&snap) {
        let f = derive_frame(&next);
        *cur = next;
        set_frame.set(f);
    }
}

fn start_full_refresh_polling(
    state: SharedState, client: Rc<FetchClient>, set_frame: WriteSignal<RenderFrame>,
) {
    Interval::new(FULL_REFRESH_MS, move || {
        spawn_checkpoints(Rc::clone(&state), Rc::clone(&client), set_frame);
        spawn_dao_fee(Rc::clone(&state),     Rc::clone(&client), set_frame);
        spawn_blacklist(Rc::clone(&state),   Rc::clone(&client), set_frame);
        spawn_ceiling(Rc::clone(&state),     Rc::clone(&client), set_frame);
    }).forget();
}

fn spawn_checkpoints(state: SharedState, client: Rc<FetchClient>, set_frame: WriteSignal<RenderFrame>) {
    spawn_local(async move {
        let url = format!("{}/api/v1/la/gas-model/checkpoints", client.base_url);
        let body = match client.get_local(&url, None).await {
            Ok(b) => b, Err(e) => { web_sys::console::warn_1(&format!("omega-ui: checkpoints: {e}").into()); return; }
        };
        let resp: Vec<GasModelCheckpoint> = match serde_json::from_str(&body) {
            Ok(r) => r, Err(e) => { web_sys::console::warn_1(&format!("omega-ui: checkpoints parse: {e}").into()); return; }
        };
        let mut cur = state.borrow_mut();
        let next = cur.with_checkpoints(resp);
        let f = derive_frame(&next); *cur = next; set_frame.set(f);
    });
}

fn spawn_dao_fee(state: SharedState, client: Rc<FetchClient>, set_frame: WriteSignal<RenderFrame>) {
    spawn_local(async move {
        let url = format!("{}/api/v1/vault/dao-fee", client.base_url);
        let body = match client.get_local(&url, None).await {
            Ok(b) => b, Err(e) => { web_sys::console::warn_1(&format!("omega-ui: dao-fee: {e}").into()); return; }
        };
        let resp: DaoFeeResponse = match serde_json::from_str(&body) {
            Ok(r) => r, Err(e) => { web_sys::console::warn_1(&format!("omega-ui: dao-fee parse: {e}").into()); return; }
        };
        let mut cur = state.borrow_mut();
        let next = cur.with_dao_fee(resp);
        let f = derive_frame(&next); *cur = next; set_frame.set(f);
    });
}

fn spawn_blacklist(state: SharedState, client: Rc<FetchClient>, set_frame: WriteSignal<RenderFrame>) {
    spawn_local(async move {
        let url = format!("{}/api/v1/builders/blacklist", client.base_url);
        let body = match client.get_local(&url, None).await {
            Ok(b) => b, Err(e) => { web_sys::console::warn_1(&format!("omega-ui: blacklist: {e}").into()); return; }
        };
        let resp: BlacklistResponse = match serde_json::from_str(&body) {
            Ok(r) => r, Err(e) => { web_sys::console::warn_1(&format!("omega-ui: blacklist parse: {e}").into()); return; }
        };
        let mut cur = state.borrow_mut();
        let next = cur.with_blacklist(resp);
        let f = derive_frame(&next); *cur = next; set_frame.set(f);
    });
}

fn spawn_ceiling(state: SharedState, client: Rc<FetchClient>, set_frame: WriteSignal<RenderFrame>) {
    spawn_local(async move {
        let url = format!("{}/api/v1/la/gas-model/ceiling-status", client.base_url);
        let body = match client.get_local(&url, None).await {
            Ok(b) => b, Err(e) => { web_sys::console::warn_1(&format!("omega-ui: ceiling: {e}").into()); return; }
        };
        let resp: CeilingStatusResponse = match serde_json::from_str(&body) {
            Ok(r) => r, Err(e) => { web_sys::console::warn_1(&format!("omega-ui: ceiling parse: {e}").into()); return; }
        };
        let mut cur = state.borrow_mut();
        let next = cur.with_ceiling_status(resp);
        let f = derive_frame(&next); *cur = next; set_frame.set(f);
    });
}