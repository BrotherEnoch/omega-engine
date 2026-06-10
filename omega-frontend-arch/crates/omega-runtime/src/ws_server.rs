// omega-frontend-arch/crates/omega-runtime/src/ws_server.rs

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info, warn};

use crate::registry::{Registry, RegistrySnapshot};

pub fn router(registry: Arc<Registry>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/ws",       get(ws_handler))
        .route("/snapshot", get(snapshot_handler))
        .with_state(registry)
        .layer(cors)
}

async fn snapshot_handler(State(registry): State<Arc<Registry>>) -> impl IntoResponse {
    axum::Json((*registry.snapshot()).clone())
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(registry): State<Arc<Registry>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, registry))
}

// ── Per-client state ──────────────────────────────────────────────────────────

struct ClientState {
    last_version: u64,
}

impl Default for ClientState {
    fn default() -> Self { Self { last_version: 0 } }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn snapshot_envelope(snap: &RegistrySnapshot) -> Option<String> {
    // Version lives INSIDE the payload so WireSnapshot is self-contained.
    serde_json::to_string(&json!({
        "type":    "snapshot",
        "payload": snap,
    })).ok()
}

// ── Main socket loop ──────────────────────────────────────────────────────────

async fn handle_socket(socket: WebSocket, registry: Arc<Registry>) {
    let (mut sender, mut receiver) = socket.split();

    // Subscribe BEFORE taking the snapshot so we cannot miss events that
    // fire between snapshot() and the loop start. The version filter below
    // discards anything already reflected in the snapshot.
    let mut rx = registry.subscribe();

    let mut client = ClientState::default();

    info!("ws: client connected");

    // ── 1. Initial snapshot ───────────────────────────────────────────────
    let snap = registry.snapshot();
    client.last_version = snap.version;

    match snapshot_envelope(&snap) {
        Some(text) => {
            if sender.send(Message::Text(text.into())).await.is_err() {
                warn!("ws: client disconnected before snapshot delivery");
                return;
            }
        }
        None => {
            error!("ws: failed to serialize initial snapshot");
            return;
        }
    }

    // ── 2. Drain stale broadcast events (version ≤ snapshot) before loop ─
    // This replaces the arbitrary sleep: we consume any events that raced
    // with snapshot() and are already reflected in the state we just sent.
    loop {
        match rx.try_recv() {
            Ok(ev) if ev.version <= client.last_version => { /* already in snapshot */ }
            Ok(ev) => {
                // First genuinely new event — process it in the main loop.
                // Re-inject by breaking and handling below, but simpler: just
                // update last_version and fall through; the loop will pick up
                // subsequent events normally.
                client.last_version = ev.version;
                let msg = json!({ "type": "layer_event", "payload": ev });
                if let Ok(text) = serde_json::to_string(&msg) {
                    if sender.send(Message::Text(text.into())).await.is_err() {
                        return;
                    }
                }
                break;
            }
            Err(_) => break, // channel empty or closed — proceed to main loop
        }
    }

    // ── 3. Main event loop ────────────────────────────────────────────────
    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        if event.version <= client.last_version {
                            continue;
                        }
                        client.last_version = event.version;

                        let msg = json!({ "type": "layer_event", "payload": event });
                        let text = match serde_json::to_string(&msg) {
                            Ok(t)  => t,
                            Err(e) => { error!("ws: event serialize error: {e}"); continue; }
                        };

                        if sender.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }

                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        warn!("ws: client lagged — resyncing snapshot");

                        let snap = registry.snapshot();
                        if snap.version <= client.last_version {
                            continue;
                        }
                        client.last_version = snap.version;

                        if let Some(text) = snapshot_envelope(&snap) {
                            let _ = sender.send(Message::Text(text.into())).await;
                        }
                    }

                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }

            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => {
                        info!("ws: client disconnected");
                        break;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let _ = sender.send(Message::Pong(data)).await;
                    }
                    _ => {}
                }
            }
        }
    }
}