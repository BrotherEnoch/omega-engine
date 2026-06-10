// omega-frontend-arch/crates/omega-ui/src/ws_client.rs

use std::{cell::RefCell, rc::Rc};

use chrono::Utc;
use leptos::{SignalSet, WriteSignal};
use serde::Deserialize;
use wasm_bindgen::{closure::Closure, JsCast};
use web_sys::{CloseEvent, ErrorEvent, MessageEvent, WebSocket};

use omega_control_contracts::{
    rest::{HealthSnapshot, LayerHealthEntry},
    ws::WsConnectionStatus,
};
use omega_frontend::render::{derive_frame, RenderFrame};

use crate::app::SharedState;

const WS_URL: &str = match option_env!("OMEGA_WS_URL") {
    Some(u) => u,
    None => "ws://127.0.0.1:9001/ws",
};

const BACKOFF_MS: &[u32] = &[500, 1_000, 2_000, 5_000, 10_000];

// ── Wire types ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WireEnvelope {
    #[serde(rename = "type")]
    kind: String,
    payload: serde_json::Value,
}

#[derive(Deserialize)]
struct WireLayerStatus {
    status: String,
    #[allow(dead_code)]
    latency_ns: u64,
    #[allow(dead_code)]
    message: String,
}

#[derive(Deserialize)]
struct WireSnapshot {
    version: u64,
    layers: std::collections::HashMap<String, WireLayerStatus>,
}

#[derive(Deserialize)]
struct WireLayerEvent {
    layer: String,
    status: String,
    #[allow(dead_code)]
    latency_ns: u64,
    #[allow(dead_code)]
    message: String,
    #[allow(dead_code)]
    version: u64,
}

// ── Status translation ────────────────────────────────────────────────────────

fn runtime_status_to_backend(s: &str) -> &'static str {
    match s {
        "HEALTHY" => "HEALTHY",
        "STARTING" => "RECOVERING",
        "DEGRADED" => "DEGRADED",
        "STALE" => "DEGRADED",
        "FAILED" => "HALTED",
        "STOPPED" => "HALTED",
        _ => "UNKNOWN",
    }
}

fn runtime_layer_id_to_backend(s: &str) -> Option<&'static str> {
    match s {
        "L00" => Some("SYSTEM_HEALTH"),
        "L01" => Some("EXTERNAL_DATA"),
        "L02" => Some("EIL"),
        "L03" => Some("RISK"),
        "L04" => Some("SECURITY"),
        "L05" => Some("CHAOS_GUARD"),
        "L06" => Some("DAG"),
        "L07" => Some("ZK"),
        "L08" => Some("HOT_PATH"),
        "L09" => Some("STRATEGY"),
        "L10" => Some("FLASHLOAN"),
        "L11" => Some("ORCHESTRATOR"),
        "L12" => Some("RELAY"),
        "L13" => Some("VAULT"),
        "L14" => Some("OBSERVABILITY"),
        "L15" => Some("LOSS_ATTRIBUTION"),
        _ => None,
    }
}

// ── Per-connection state ──────────────────────────────────────────────────────

struct ConnState {
    layers: std::collections::HashMap<String, (String, bool)>,
}

impl ConnState {
    fn new() -> Self {
        Self {
            layers: std::collections::HashMap::new(),
        }
    }

    fn apply_snapshot(&mut self, snap: WireSnapshot) {
        self.layers.clear();

        for (runtime_id, ws) in &snap.layers {
            if let Some(backend_id) = runtime_layer_id_to_backend(runtime_id) {
                let backend_state = runtime_status_to_backend(&ws.status).to_string();
                let is_op = ws.status == "HEALTHY";
                self.layers
                    .insert(backend_id.to_string(), (backend_state, is_op));
            }
        }

        web_sys::console::log_1(
            &format!(
                "omega-ws: snapshot v{} ({} layers mapped / {} received)",
                snap.version,
                self.layers.len(),
                snap.layers.len(),
            )
            .into(),
        );
    }

    fn apply_event(&mut self, ev: &WireLayerEvent) -> bool {
        if let Some(backend_id) = runtime_layer_id_to_backend(&ev.layer) {
            let backend_state = runtime_status_to_backend(&ev.status).to_string();
            let is_op = ev.status == "HEALTHY";

            let prev = self
                .layers
                .insert(backend_id.to_string(), (backend_state.clone(), is_op));

            return prev
                .map(|(s, _)| s != backend_state)
                .unwrap_or(true);
        }
        false
    }

    fn to_health_snapshot(&self) -> HealthSnapshot {
        let layers: Vec<LayerHealthEntry> = self
            .layers
            .iter()
            .map(|(id, (state, is_op))| LayerHealthEntry {
                layer: id.clone(),
                state: state.clone(),
                is_operational: *is_op,
            })
            .collect();

        let system_halted = layers.iter().any(|l| l.state == "HALTED");

        HealthSnapshot {
            generated_at: Utc::now(),
            layers,
            system_halted,
        }
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

pub fn start(state: SharedState, set_frame: WriteSignal<RenderFrame>) {
    connect(state, set_frame, 0);
}

// ── Connection lifecycle ──────────────────────────────────────────────────────

fn connect(state: SharedState, set_frame: WriteSignal<RenderFrame>, attempt: u32) {
    let ws = match WebSocket::new(WS_URL) {
        Ok(w) => w,
        Err(_) => {
            schedule_reconnect(state, set_frame, attempt);
            return;
        }
    };

    let conn = Rc::new(RefCell::new(ConnState::new()));

    // ── onmessage ─────────────────────────────────────────────────────────
    {
        let state = Rc::clone(&state);
        let conn = Rc::clone(&conn);

        let on_msg = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let text = match e.data().as_string() {
                Some(t) => t,
                None => return,
            };

            web_sys::console::log_1(
                &format!(
                    "omega-ws: raw message: {}",
                    &text[..text.len().min(500)]
                )
                .into(),
            );

            let envelope: WireEnvelope = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(je) => {
                    web_sys::console::warn_1(
                        &format!("omega-ws: parse error: {je}").into(),
                    );
                    return;
                }
            };

            match envelope.kind.as_str() {
                "snapshot" => {
                    let snap: WireSnapshot =
                        match serde_json::from_value(envelope.payload) {
                            Ok(s) => s,
                            Err(je) => {
                                web_sys::console::warn_1(
                                    &format!(
                                        "omega-ws: snapshot parse error: {je}"
                                    )
                                    .into(),
                                );
                                return;
                            }
                        };

                    conn.borrow_mut().apply_snapshot(snap);
                    push_to_leptos(&state, set_frame, &conn.borrow(), true);
                }

                "layer_event" => {
                    let ev: WireLayerEvent =
                        match serde_json::from_value(envelope.payload) {
                            Ok(v) => v,
                            Err(je) => {
                                web_sys::console::warn_1(
                                    &format!(
                                        "omega-ws: layer_event parse error: {je}"
                                    )
                                    .into(),
                                );
                                return;
                            }
                        };

                    let changed = conn.borrow_mut().apply_event(&ev);
                    if changed {
                        push_to_leptos(
                            &state,
                            set_frame,
                            &conn.borrow(),
                            false,
                        );
                    }
                }

                other => {
                    web_sys::console::warn_1(
                        &format!("omega-ws: unknown message type: {other}")
                            .into(),
                    );
                }
            }
        });

        ws.set_onmessage(Some(on_msg.as_ref().unchecked_ref()));
        on_msg.forget();
    }

    // ── onopen ────────────────────────────────────────────────────────────
    {
        let state = Rc::clone(&state);
        let on_open = Closure::<dyn FnMut()>::new(move || {
            web_sys::console::log_1(
                &"omega-ws: connected to omega-runtime".into(),
            );
            let mut cur = state.borrow_mut();
            let next = cur.with_ws_status(WsConnectionStatus::Connected);
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
        let state = Rc::clone(&state);

        let on_close = Closure::<dyn FnMut(CloseEvent)>::new(move |e: CloseEvent| {
            if e.code() != 1000 {
                web_sys::console::warn_1(
                    &format!(
                        "omega-ws: disconnected (code={}) — reconnecting",
                        e.code()
                    )
                    .into(),
                );
            }

            {
                let mut cur = state.borrow_mut();
                let next = cur.with_ws_status(
                    WsConnectionStatus::Reconnecting { attempt },
                );
                *cur = next;
            }

            schedule_reconnect(Rc::clone(&state), set_frame, attempt);
        });

        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        on_close.forget();
    }
}

// ── Leptos signal push — dirty-checked ───────────────────────────────────────

fn push_to_leptos(
    state: &SharedState,
    set_frame: WriteSignal<RenderFrame>,
    conn: &ConnState,
    force: bool,
) {
    let snapshot = conn.to_health_snapshot();
    let mut cur = state.borrow_mut();

    if let Some(next) = cur.accept_health_snapshot(&snapshot) {
        if force {
            let f = derive_frame(&next);
            *cur = next;
            set_frame.set(f);
        } else {
            use omega_control_contracts::health::LayerId;

            const ALL_LAYERS: &[LayerId] = &[
                LayerId::SystemHealth,
                LayerId::ExternalData,
                LayerId::Eil,
                LayerId::Risk,
                LayerId::Security,
                LayerId::ChaosGuard,
                LayerId::Dag,
                LayerId::Zk,
                LayerId::HotPath,
                LayerId::Strategy,
                LayerId::Flashloan,
                LayerId::Orchestrator,
                LayerId::Relay,
                LayerId::Vault,
                LayerId::Observability,
                LayerId::LossAttribution,
            ];

            let changed =
                cur.overall_health() != next.overall_health()
                    || ALL_LAYERS.iter().any(|&id| {
                        cur.layer_status(id) != next.layer_status(id)
                    });

            *cur = next;

            if changed {
                set_frame.set(derive_frame(&*cur));
            }
        }
    }
}

// ── Reconnect with exponential backoff ───────────────────────────────────────

fn schedule_reconnect(
    state: SharedState,
    set_frame: WriteSignal<RenderFrame>,
    attempt: u32,
) {
    let delay_ms = BACKOFF_MS
        .get(attempt as usize)
        .copied()
        .unwrap_or(*BACKOFF_MS.last().unwrap());

    if attempt > 0 {
        web_sys::console::log_1(
            &format!(
                "omega-ws: reconnecting in {delay_ms}ms (attempt {attempt})"
            )
            .into(),
        );
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