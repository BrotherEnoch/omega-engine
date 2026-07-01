// omega-frontend-arch/crates/omega-ui/src/ws_client.rs

use std::{cell::RefCell, rc::Rc};

use leptos::{SignalSet, WriteSignal};
use serde::Deserialize;
use strum::IntoEnumIterator;
use wasm_bindgen::{closure::Closure, JsCast};
use web_sys::{CloseEvent, ErrorEvent, MessageEvent, WebSocket};

use omega_control_contracts::{
    health::LayerId,
    rest::{HealthSnapshot, LayerHealthEntry},
    ws::{WsConnectionStatus, WsEvent},
};
use omega_frontend::render::{derive_frame, RenderFrame};

use crate::app::SharedState;

// Default points at the real control-plane's WebSocket event stream
// (ops/control-plane, §17.1). Override at build time with OMEGA_WS_URL
// to point at the mock omega-runtime feed (ws://127.0.0.1:9001/ws)
// for frontend-only development without the full backend running.
const WS_URL: &str = match option_env!("OMEGA_WS_URL") {
    Some(u) => u,
    None => "ws://127.0.0.1:8080/ws/events",
};

// Bearer token sent in the post-connect auth frame (§17.1). Must match
// the control-plane's --api-token / OMEGA_API_TOKEN. Override at build
// time with OMEGA_API_TOKEN; falls back to the same default used in
// local dev runs of the control-plane binary.
const WS_AUTH_TOKEN: &str = match option_env!("OMEGA_API_TOKEN") {
    Some(t) => t,
    None => "test-token",
};

const BACKOFF_MS: &[u32] = &[500, 1_000, 2_000, 5_000, 10_000];

// ── Wire types ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WireEnvelope {
    #[serde(rename = "type")]
    kind:    String,
    payload: serde_json::Value,
}

#[derive(Deserialize)]
struct WireLayerStatus {
    status: String,
    #[allow(dead_code)]
    latency_ns: u64,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct WireSnapshot {
    #[allow(dead_code)]
    version: u64,
    layers:  std::collections::HashMap<String, WireLayerStatus>,
}

#[derive(Deserialize)]
struct WireLayerEvent {
    layer:      String,
    status:     String,
    #[allow(dead_code)]
    latency_ns: u64,
    #[serde(default)]
    message:    String,
    #[allow(dead_code)]
    version:    u64,
}

// ── Status translation ──────────────────────────────────────────────────────

fn runtime_status_to_backend(s: &str) -> &'static str {
    match s {
        "HEALTHY"  => "HEALTHY",
        "STARTING" => "RECOVERING",
        "DEGRADED" => "DEGRADED",
        "STALE"    => "DEGRADED",
        "FAILED"   => "HALTED",
        "STOPPED"  => "HALTED",
        _          => "UNKNOWN",
    }
}

/// Maps wire layer tags ("L00"…"L15") to canonical v12 backend layer
/// identifier strings, matching the order in the canonical LayerId enum
/// (health.rs) and what ops/control-plane's get_health handler sends.
///
/// L00=HEALTH, L01=RPC, L02=ORACLE, L03=RISK, L04=SECURITY,
/// L05=COMPLIANCE, L06=DAG, L07=ZK, L08=HOT_PATH, L09=STRATEGIES,
/// L10=FLASH_LOAN, L11=GAS_WAR, L12=RELAY, L13=ADDRESS_ROTATION,
/// L14=OBSERVABILITY, L15=LOSS_ATTRIBUTION
fn runtime_layer_id_to_backend(s: &str) -> Option<&'static str> {
    match s {
        "L00" => Some("HEALTH"),
        "L01" => Some("RPC"),
        "L02" => Some("ORACLE"),
        "L03" => Some("RISK"),
        "L04" => Some("SECURITY"),
        "L05" => Some("COMPLIANCE"),
        "L06" => Some("DAG"),
        "L07" => Some("ZK"),
        "L08" => Some("HOT_PATH"),
        "L09" => Some("STRATEGIES"),
        "L10" => Some("FLASH_LOAN"),
        "L11" => Some("GAS_WAR"),
        "L12" => Some("RELAY"),
        "L13" => Some("ADDRESS_ROTATION"),
        "L14" => Some("OBSERVABILITY"),
        "L15" => Some("LOSS_ATTRIBUTION"),
        _     => None,
    }
}

/// Maps a LayerId variant to the canonical v12 backend string used in
/// LayerHealthEntry.layer / HealthSnapshot.layers[].layer. Must match
/// LayerId::backend_str() in health.rs exactly.
fn layer_backend_key(layer: LayerId) -> &'static str {
    match layer {
        LayerId::SystemHealth    => "HEALTH",
        LayerId::ExternalData    => "RPC",
        LayerId::Oracle          => "ORACLE",
        LayerId::Risk            => "RISK",
        LayerId::Security        => "SECURITY",
        LayerId::Eil             => "COMPLIANCE",
        LayerId::Dag             => "DAG",
        LayerId::Zk              => "ZK",
        LayerId::HotPath         => "HOT_PATH",
        LayerId::Strategy        => "STRATEGIES",
        LayerId::Flashloan       => "FLASH_LOAN",
        LayerId::Orchestrator    => "GAS_WAR",
        LayerId::Relay           => "RELAY",
        LayerId::Vault           => "ADDRESS_ROTATION",
        LayerId::Observability   => "OBSERVABILITY",
        LayerId::LossAttribution => "LOSS_ATTRIBUTION",
    }
}

// ── Per-connection layer cache ───────────────────────────────────────────────

type LayerEntry = (String, bool, Option<String>);

struct ConnState {
    layers: std::collections::HashMap<String, LayerEntry>,
}

impl ConnState {
    fn new() -> Self {
        let mut layers = std::collections::HashMap::with_capacity(16);
        for layer in LayerId::iter() {
            layers.insert(
                layer_backend_key(layer).to_string(),
                ("UNKNOWN".to_string(), false, None),
            );
        }
        Self { layers }
    }

    fn apply_snapshot(&mut self, snap: WireSnapshot) {
        for (runtime_id, ws) in &snap.layers {
            if let Some(backend_id) = runtime_layer_id_to_backend(runtime_id) {
                let state = runtime_status_to_backend(&ws.status).to_string();
                let is_op = ws.status == "HEALTHY";
                let msg   = if ws.message.is_empty() { None } else { Some(ws.message.clone()) };
                self.layers.insert(backend_id.to_string(), (state, is_op, msg));
            }
        }
        web_sys::console::log_1(
            &format!("omega-ws: snapshot ({} layers mapped)", self.layers.len()).into(),
        );
    }

    fn apply_event(&mut self, ev: &WireLayerEvent) -> bool {
        if let Some(backend_id) = runtime_layer_id_to_backend(&ev.layer) {
            let new_state = runtime_status_to_backend(&ev.status).to_string();
            let is_op     = ev.status == "HEALTHY";
            let msg       = if ev.message.is_empty() { None } else { Some(ev.message.clone()) };
            let prev      = self.layers.insert(backend_id.to_string(), (new_state.clone(), is_op, msg));
            return prev.map(|(s, _, _)| s != new_state).unwrap_or(true);
        }
        false
    }

    fn to_health_snapshot(&self) -> HealthSnapshot {
        let layers: Vec<LayerHealthEntry> = self
            .layers
            .iter()
            .map(|(id, (state, is_op, msg))| LayerHealthEntry {
                layer:          id.clone(),
                state:          state.clone(),
                is_operational: *is_op,
                message:        msg.clone(),
            })
            .collect();
        let system_halted = layers.iter().any(|l| l.state == "HALTED");
        // Use MIN_UTC so the first real REST snapshot (with an actual
        // backend-generated timestamp) always passes accept_health_snapshot's
        // staleness check (snap.generated_at > current.generated_at). Using
        // MAX_UTC here permanently blocked every REST health poll, since every
        // real timestamp is smaller than MAX_UTC and was silently rejected.
        HealthSnapshot {
            generated_at:  chrono::DateTime::<chrono::Utc>::MIN_UTC,
            layers,
            system_halted,
        }
    }
}

// ── Public entry point ───────────────────────────────────────────────────────

pub fn start(state: SharedState, set_frame: WriteSignal<RenderFrame>) {
    connect(state, set_frame, 0);
}

// ── Connection lifecycle ─────────────────────────────────────────────────────

fn connect(state: SharedState, set_frame: WriteSignal<RenderFrame>, attempt: u32) {
    let ws = match WebSocket::new(WS_URL) {
        Ok(w)  => w,
        Err(_) => { schedule_reconnect(state, set_frame, attempt); return; }
    };

    let conn = Rc::new(RefCell::new(ConnState::new()));

    // Push initial all-UNKNOWN snapshot so UI shows 16 rows immediately.
    push_frame(&state, set_frame, &conn.borrow());

    // ── onmessage ─────────────────────────────────────────────────────────
    {
        let state = Rc::clone(&state);
        let conn  = Rc::clone(&conn);

        let on_msg = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let text = match e.data().as_string() {
                Some(t) => t,
                None    => return,
            };

            web_sys::console::log_1(
                &format!("omega-ws: raw message: {}", &text[..text.len().min(500)]).into(),
            );

            // The control-plane sends a one-off auth_ok / auth_failed frame
            // immediately after the auth handshake (§17.1); it has no
            // "kind"/"type" matching our snapshot/layer_event/WsEvent
            // shapes, so just log and skip it rather than warning.
            if text.contains("\"auth_ok\"") || text.contains("\"auth_failed\"") {
                web_sys::console::log_1(&format!("omega-ws: auth response: {text}").into());
                return;
            }

            let envelope: WireEnvelope = match serde_json::from_str(&text) {
                Ok(v)   => v,
                Err(je) => {
                    web_sys::console::warn_1(&format!("omega-ws: parse error: {je}").into());
                    return;
                }
            };

            match envelope.kind.as_str() {
                "snapshot" => {
                    let snap: WireSnapshot = match serde_json::from_value(envelope.payload) {
                        Ok(s)  => s,
                        Err(je) => {
                            web_sys::console::warn_1(
                                &format!("omega-ws: snapshot parse error: {je}").into());
                            return;
                        }
                    };
                    conn.borrow_mut().apply_snapshot(snap);
                    push_frame(&state, set_frame, &conn.borrow());
                }

                "layer_event" => {
                    let ev: WireLayerEvent = match serde_json::from_value(envelope.payload) {
                        Ok(v)  => v,
                        Err(je) => {
                            web_sys::console::warn_1(
                                &format!("omega-ws: layer_event parse error: {je}").into());
                            return;
                        }
                    };
                    let changed = conn.borrow_mut().apply_event(&ev);
                    if changed {
                        push_frame(&state, set_frame, &conn.borrow());
                    }
                }

                // Observability events — route to obs_log then re-render.
                other => {
                    let event: Option<WsEvent> = serde_json::from_str(&text).ok();
                    if let Some(ev) = event {
                        let mut cur = state.borrow_mut();
                        cur.record_obs_event(&ev);
                        let f = derive_frame(&*cur);
                        drop(cur);
                        set_frame.set(f);
                    } else {
                        web_sys::console::warn_1(
                            &format!("omega-ws: unknown message type: {other}").into());
                    }
                }
            }
        });

        ws.set_onmessage(Some(on_msg.as_ref().unchecked_ref()));
        on_msg.forget();
    }

    // ── onopen ────────────────────────────────────────────────────────────
    {
        let state    = Rc::clone(&state);
        let ws_clone = ws.clone();
        let on_open = Closure::<dyn FnMut()>::new(move || {
            web_sys::console::log_1(&"omega-ws: connected to control-plane".into());

            // Send the auth frame expected by ops/control-plane's
            // negotiate_auth (§17.1): { "type": "auth", "token": "..." }.
            // Without this the connection is treated as anonymous after a
            // 5-second timeout and rate-limited to 100 msg/min instead of
            // 300 msg/min — still functional, just worth sending promptly.
            let auth_frame = serde_json::json!({
                "type":  "auth",
                "token": WS_AUTH_TOKEN,
            })
            .to_string();
            if let Err(e) = ws_clone.send_with_str(&auth_frame) {
                web_sys::console::warn_1(
                    &format!("omega-ws: failed to send auth frame: {e:?}").into());
            }

            let mut cur = state.borrow_mut();
            let next    = cur.with_ws_status(WsConnectionStatus::Connected);
            *cur = next;
        });
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        on_open.forget();
    }

    // ── onerror ───────────────────────────────────────────────────────────
    {
        let on_err = Closure::<dyn FnMut(ErrorEvent)>::new(move |_e| {});
        ws.set_onerror(Some(on_err.as_ref().unchecked_ref()));
        on_err.forget();
    }

    // ── onclose ───────────────────────────────────────────────────────────
    {
        let state    = Rc::clone(&state);
        let on_close = Closure::<dyn FnMut(CloseEvent)>::new(move |e: CloseEvent| {
            if e.code() != 1000 {
                web_sys::console::warn_1(
                    &format!("omega-ws: disconnected (code={}) — reconnecting", e.code()).into());
            }
            {
                let mut cur = state.borrow_mut();
                let next    = cur.with_ws_status(WsConnectionStatus::Reconnecting { attempt });
                *cur = next;
            }
            schedule_reconnect(Rc::clone(&state), set_frame, attempt);
        });
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        on_close.forget();
    }
}

// ── Frame push ────────────────────────────────────────────────────────────────

fn push_frame(
    state:     &SharedState,
    set_frame: WriteSignal<RenderFrame>,
    conn:      &ConnState,
) {
    let snapshot = conn.to_health_snapshot();
    let mut cur  = state.borrow_mut();
    cur.force_health_snapshot(snapshot);
    let f = derive_frame(&*cur);
    drop(cur);
    set_frame.set(f);
}

// ── Reconnect with exponential backoff ─────────────────────────────────────────

fn schedule_reconnect(
    state:     SharedState,
    set_frame: WriteSignal<RenderFrame>,
    attempt:   u32,
) {
    let delay_ms = BACKOFF_MS
        .get(attempt as usize)
        .copied()
        .unwrap_or(*BACKOFF_MS.last().unwrap());

    if attempt > 0 {
        web_sys::console::log_1(
            &format!("omega-ws: reconnecting in {delay_ms}ms (attempt {attempt})").into());
    }

    let cb = Closure::once(move || {
        connect(state, set_frame, attempt.saturating_add(1));
    });

    web_sys::window()
        .unwrap()
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            delay_ms as i32,
        )
        .unwrap();

    cb.forget();
}