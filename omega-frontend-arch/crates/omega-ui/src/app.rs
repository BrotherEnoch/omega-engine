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
    components::{alerts::Alerts, footer::Footer, header::Header, layer_grid::LayerGrid},
    sync_adapter::FetchClient,
    ws_client,
};

// ---------------------------------------------------------------------------
// Compile-time config
// ---------------------------------------------------------------------------

const API_URL: &str = match option_env!("OMEGA_API_URL") {
    Some(url) => url,
    None      => "http://127.0.0.1:9001",
};

const API_TOKEN: &str = match option_env!("OMEGA_API_TOKEN") {
    Some(tok) => tok,
    None      => "",
};

// REST polling is kept as a fallback — WS is the primary live path.
// Health poll is slowed to 10s since WS pushes deltas sub-second.
const HEALTH_POLL_MS:  u32 = 10_000;
const FULL_REFRESH_MS: u32 = 30_000;

// ---------------------------------------------------------------------------
// Shared state handle — pub so ws_client.rs can name the type
// ---------------------------------------------------------------------------

pub type SharedState = Rc<RefCell<EngineState>>;

// ---------------------------------------------------------------------------
// App component
// ---------------------------------------------------------------------------

#[component]
pub fn App() -> impl IntoView {
    let (frame, set_frame) = create_signal(derive_frame(&EngineState::default()));
    let state: SharedState = Rc::new(RefCell::new(EngineState::default()));

    // ── Primary: WebSocket live feed ────────────────────────────────────────
    // Connects immediately; reconnects with backoff on disconnect.
    ws_client::start(Rc::clone(&state), set_frame);

    if !API_URL.is_empty() {
        let client = Rc::new(FetchClient {
            base_url:     API_URL.to_string(),
            bearer_token: if API_TOKEN.is_empty() { None } else { Some(API_TOKEN.to_string()) },
        });

        // ── Fallback: health REST poll (10s — WS is primary) ───────────────
        start_health_polling(Rc::clone(&state), Rc::clone(&client), set_frame);

        // ── Cold path: full refresh every 30s (4 independent spawns) ───────
        start_full_refresh_polling(Rc::clone(&state), Rc::clone(&client), set_frame);
    }

    view! {
        <div class="app">
            <div class="scanlines" aria-hidden="true"></div>
            <Header frame=frame />
            <main style="flex:1;display:flex;flex-direction:column;overflow:hidden;gap:0.4rem;padding:0.5rem 0 0">
                <Alerts frame=frame />
                <LayerGrid frame=frame />
            </main>
            <Footer frame=frame />
        </div>
    }
}

// ---------------------------------------------------------------------------
// Health polling — fallback REST path
// ---------------------------------------------------------------------------

fn start_health_polling(
    state:     SharedState,
    client:    Rc<FetchClient>,
    set_frame: WriteSignal<RenderFrame>,
) {
    Interval::new(HEALTH_POLL_MS, move || {
        let state  = Rc::clone(&state);
        let client = Rc::clone(&client);
        spawn_local(async move {
            poll_health_once(state, client, set_frame).await;
        });
    })
    .forget();
}

async fn poll_health_once(
    state:     SharedState,
    client:    Rc<FetchClient>,
    set_frame: WriteSignal<RenderFrame>,
) {
    let url = format!("{}/api/v1/health", client.base_url);
    let body = match client.get_local(&url, None).await {
        Ok(b)  => b,
        Err(e) => {
            web_sys::console::warn_1(&format!("omega-ui: health fetch: {e}").into());
            return;
        }
    };

    let snap = match serde_json::from_str::<HealthSnapshot>(&body) {
        Ok(s)  => s,
        Err(e) => {
            web_sys::console::warn_1(&format!("omega-ui: health parse: {e}").into());
            return;
        }
    };

    let mut cur = state.borrow_mut();
    if let Some(next) = cur.accept_health_snapshot(&snap) {
        let f = derive_frame(&next);
        *cur = next;
        set_frame.set(f);
    }
}

// ---------------------------------------------------------------------------
// Full refresh — cold path, 30s
// ---------------------------------------------------------------------------

fn start_full_refresh_polling(
    state:     SharedState,
    client:    Rc<FetchClient>,
    set_frame: WriteSignal<RenderFrame>,
) {
    Interval::new(FULL_REFRESH_MS, move || {
        spawn_checkpoints(Rc::clone(&state), Rc::clone(&client), set_frame);
        spawn_dao_fee(Rc::clone(&state),     Rc::clone(&client), set_frame);
        spawn_blacklist(Rc::clone(&state),   Rc::clone(&client), set_frame);
        spawn_ceiling(Rc::clone(&state),     Rc::clone(&client), set_frame);
    })
    .forget();
}

fn spawn_checkpoints(
    state:     SharedState,
    client:    Rc<FetchClient>,
    set_frame: WriteSignal<RenderFrame>,
) {
    spawn_local(async move {
        let url = format!("{}/api/v1/la/gas-model/checkpoints", client.base_url);
        let body = match client.get_local(&url, None).await {
            Ok(b)  => b,
            Err(e) => { web_sys::console::warn_1(&format!("omega-ui: checkpoints: {e}").into()); return; }
        };
        let resp: Vec<GasModelCheckpoint> = match serde_json::from_str(&body) {
            Ok(r)  => r,
            Err(e) => { web_sys::console::warn_1(&format!("omega-ui: checkpoints parse: {e}").into()); return; }
        };
        let mut cur = state.borrow_mut();
        let next = cur.with_checkpoints(resp);
        let f = derive_frame(&next);
        *cur = next;
        set_frame.set(f);
    });
}

fn spawn_dao_fee(
    state:     SharedState,
    client:    Rc<FetchClient>,
    set_frame: WriteSignal<RenderFrame>,
) {
    spawn_local(async move {
        let url = format!("{}/api/v1/vault/dao-fee", client.base_url);
        let body = match client.get_local(&url, None).await {
            Ok(b)  => b,
            Err(e) => { web_sys::console::warn_1(&format!("omega-ui: dao-fee: {e}").into()); return; }
        };
        let resp: DaoFeeResponse = match serde_json::from_str(&body) {
            Ok(r)  => r,
            Err(e) => { web_sys::console::warn_1(&format!("omega-ui: dao-fee parse: {e}").into()); return; }
        };
        let mut cur = state.borrow_mut();
        let next = cur.with_dao_fee(resp);
        let f = derive_frame(&next);
        *cur = next;
        set_frame.set(f);
    });
}

fn spawn_blacklist(
    state:     SharedState,
    client:    Rc<FetchClient>,
    set_frame: WriteSignal<RenderFrame>,
) {
    spawn_local(async move {
        let url = format!("{}/api/v1/builders/blacklist", client.base_url);
        let body = match client.get_local(&url, None).await {
            Ok(b)  => b,
            Err(e) => { web_sys::console::warn_1(&format!("omega-ui: blacklist: {e}").into()); return; }
        };
        let resp: BlacklistResponse = match serde_json::from_str(&body) {
            Ok(r)  => r,
            Err(e) => { web_sys::console::warn_1(&format!("omega-ui: blacklist parse: {e}").into()); return; }
        };
        let mut cur = state.borrow_mut();
        let next = cur.with_blacklist(resp);
        let f = derive_frame(&next);
        *cur = next;
        set_frame.set(f);
    });
}

fn spawn_ceiling(
    state:     SharedState,
    client:    Rc<FetchClient>,
    set_frame: WriteSignal<RenderFrame>,
) {
    spawn_local(async move {
        let url = format!("{}/api/v1/la/gas-model/ceiling-status", client.base_url);
        let body = match client.get_local(&url, None).await {
            Ok(b)  => b,
            Err(e) => { web_sys::console::warn_1(&format!("omega-ui: ceiling: {e}").into()); return; }
        };
        let resp: CeilingStatusResponse = match serde_json::from_str(&body) {
            Ok(r)  => r,
            Err(e) => { web_sys::console::warn_1(&format!("omega-ui: ceiling parse: {e}").into()); return; }
        };
        let mut cur = state.borrow_mut();
        let next = cur.with_ceiling_status(resp);
        let f = derive_frame(&next);
        *cur = next;
        set_frame.set(f);
    });
}