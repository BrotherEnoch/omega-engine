// ops/control-plane/src/ws.rs
//
//
// WebSocket event stream â€” ws://<host>/ws/events (spec Â§17, Â§17.1).
//
// ## Rate limits (Â§17.1, fix M4)
//
//   Authenticated connections (valid Bearer token in the first message):
//     300 messages/minute outbound
//   Anonymous connections:
//     100 messages/minute outbound
//
//   When the outbound rate is exceeded, the connection is closed with a
//   1008 (Policy Violation) close frame after sending one final error
//   message.
//
// ## Authentication
//
//   HTTP-level Bearer auth is not available for WebSocket upgrades in
//   most clients.  Instead, the client sends a single text frame
//   immediately after connection:
//
//     { "type": "auth", "token": "<bearer-token>" }
//
//   The server reads this frame with a 5-second timeout.  A valid token
//   upgrades the connection to authenticated rate limits.  An invalid
//   or missing auth frame is treated as anonymous.
//
// ## Event stream
//
//   After the auth handshake, the server fans out `WsEvent` messages
//   from the broadcast channel as JSON text frames.  The connection is
//   kept alive with periodic pings.  If the broadcast receiver lags and
//   events are dropped, the client receives a `lag_detected` error frame
//   and should reconnect.
//
//   The route itself (`GET /ws/events`) is mounted in `main.rs`'s
//   `build_router()` â€” this module only provides the handler.
//
// ## Wire format (fixed this revision â€” see "Fix" note below)
//
//   `WsEvent` (the broadcast event stream) is INTERNALLY tagged, with
//   NO separate payload wrapper â€” `omega_control_contracts::ws::WsEvent`
//   actually declares `#[serde(tag = "kind", rename_all = "snake_case")]`
//   (no `content = "..."`), so every field is flattened alongside the
//   tag at the top level:
//
//     { "kind": "<snake_case_variant>", "field_a": â€¦, "field_b": â€¦ }
//
//   e.g. `WsEvent::ProfitSplit { .. }` serialises as
//     { "kind": "profit_split", "blueprint_hash": "0x1", "pil_share_wei": "â€¦", â€¦ }
//   NOT as `{ "type": "profit_split", "payload": { â€¦ } }`.
//
//   The two one-shot auth-handshake frame types are tagged differently
//   but follow the SAME flattened (no-wrapper) shape â€” just with
//   `tag = "type"` instead of `tag = "kind"`:
//     - `WsClientFrame` (client â†’ server): `{ "type": "auth", "token": "â€¦" }`
//     - `WsAuthFrame` (server â†’ client ack): `{ "type": "auth_ok", "rate_limit": â€¦, "window_secs": â€¦ }`
//       or `{ "type": "auth_failed", "rate_limit": â€¦, "window_secs": â€¦ }`
//   `negotiate_auth` below hand-builds these via `serde_json::json!` and
//   was already correct against this shape â€” only the broadcast-event
//   wire format (this section) was previously documented wrong.
//
// ## Fix (this revision)
//
// The test module previously imported `WsEvent` and
// `WS_CHANNEL_CAPACITY` from `crate::state` â€” a module that does not
// exist in this crate (no `mod state;` in `main.rs`; confirmed the same
// way `grpc.rs`'s own module comment already documented for its
// equivalent `AppState`/`WsEvent`/`ALL_LAYER_IDS` imports). Fixed by
// importing `WsEvent` from `omega_control_contracts::ws`, the same
// shared type `grpc.rs`, `obs_bridge.rs`, and the frontend dashboard
// already agree on.
//
// `WS_CHANNEL_CAPACITY` resolves via `crate::WS_CHANNEL_CAPACITY` â€” this
// compiles and its two tests (`broadcast_channel_capacity`,
// `broadcast_publish_reaches_subscriber`) pass, confirming `main.rs`
// really does expose it there (whether by re-export from
// `omega_control_contracts::ws::WS_CHANNEL_CAPACITY` or its own const of
// the same value) â€” left unchanged.
//
// ## Fix (this revision, 2): six WsEvent serialisation tests asserted
// the wrong wire format
//
// This file's own module doc comment (see "## Wire format" above)
// previously claimed `WsEvent` used
// `#[serde(tag = "type", content = "payload", rename_all = "snake_case")]`
// â€” an assumption made without having seen
// `crates/omega-control-contracts/src/ws.rs`, and explicitly flagged as
// such at the time (`main.rs's contents were not available when this
// fix was made`). The real attribute is
// `#[serde(tag = "kind", rename_all = "snake_case")]` â€” no `content`,
// fields flattened at the top level, not wrapped under `"payload"`.
// Confirmed independently by `omega-control-contracts`' OWN test suite
// (`ws::tests::blueprint_confirmed_uses_wei_string_like_profit_split`,
// `ws::tests::profit_split_and_blueprint_confirmed_agree_on_precision_strategy`),
// which already passed against this real shape â€” only this file's
// tests were out of sync with it. Fixed all six affected tests
// (`config_reloaded_â€¦`, `model_pause_changed_â€¦`, `blacklist_reloaded_â€¦`,
// `health_transition_â€¦`, `profit_split_â€¦`, `gas_model_reverted_â€¦`) to
// assert `"kind":"<variant>"` instead of `"type":"<variant>"`, and
// removed the now-incorrect `"payload":{` wrapper assertions â€” each
// event's own fields (already separately asserted, e.g.
// `"entry_count":42`) are the correct thing to check for flatness
// instead.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use tokio::time::timeout;

use crate::AppState;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Constants
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Rate limit window.
const RATE_WINDOW: Duration = Duration::from_secs(60);
/// Messages per window for authenticated connections (Â§17.1, fix M4).
const AUTHED_LIMIT: u32 = 300;
/// Messages per window for anonymous connections.
const ANON_LIMIT: u32 = 100;
/// How long to wait for the initial auth frame.
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
/// WebSocket ping interval.
const PING_INTERVAL: Duration = Duration::from_secs(30);

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Upgrade handler
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Axum route handler: upgrades an HTTP connection to WebSocket.
///
/// Mounted at `GET /ws/events` in `main.rs`'s `build_router()`.
pub async fn events_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Connection handler
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

async fn handle_ws(mut socket: WebSocket, state: Arc<AppState>) {
    // â”€â”€ Auth handshake â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    let is_authenticated = negotiate_auth(&mut socket, &state.api_token).await;
    let msg_limit = if is_authenticated {
        AUTHED_LIMIT
    } else {
        ANON_LIMIT
    };

    tracing::debug!(
        authenticated = is_authenticated,
        msg_limit,
        "WebSocket connection established",
    );

    // Subscribe to the event broadcast channel.
    // Receivers that lag behind the channel capacity will have events
    // dropped; we detect this via `RecvError::Lagged` and notify the client.
    let mut rx = state.ws_tx.subscribe();

    // â”€â”€ Rate-limit state â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    let mut msg_count = 0u32;
    let mut window_start = Instant::now();
    let mut ping_timer = tokio::time::interval(PING_INTERVAL);
    ping_timer.tick().await; // consume first tick (fires immediately)

    loop {
        // Reset rate-limit window
        if window_start.elapsed() >= RATE_WINDOW {
            msg_count = 0;
            window_start = Instant::now();
        }

        tokio::select! {
            // â”€â”€ Inbound frame from client â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
            frame = socket.recv() => {
                match frame {
                    Some(Ok(Message::Close(_))) | None => {
                        tracing::debug!("WebSocket client disconnected");
                        break;
                    }
                    Some(Ok(Message::Ping(p))) => {
                        if socket.send(Message::Pong(p)).await.is_err() { break; }
                    }
                    Some(Ok(_)) => {
                        // Clients may send any other frame; ignore silently.
                    }
                    Some(Err(e)) => {
                        tracing::debug!(error = %e, "WebSocket receive error");
                        break;
                    }
                }
            }

            // â”€â”€ Outbound event from broadcast channel â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        if msg_count >= msg_limit {
                            // Rate limit exceeded â€” close with Policy Violation
                            let _ = socket.send(Message::Text(
                                serde_json::to_string(&serde_json::json!({
                                    "error": "RATE_LIMIT_EXCEEDED",
                                    "limit": msg_limit,
                                    "window_secs": RATE_WINDOW.as_secs(),
                                })).unwrap_or_default(),
                            )).await;
                            let _ = socket.send(Message::Close(Some(CloseFrame {
                                code:   1008, // Policy Violation
                                reason: "rate limit exceeded".into(),
                            }))).await;
                            break;
                        }

                        let text = match serde_json::to_string(&event) {
                            Ok(t)  => t,
                            Err(e) => {
                                tracing::error!(error = %e, "Failed to serialise WsEvent");
                                continue;
                            }
                        };

                        if socket.send(Message::Text(text)).await.is_err() {
                            tracing::debug!("WebSocket send failed â€” client disconnected");
                            break;
                        }
                        msg_count += 1;
                    }

                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(dropped = n, "WebSocket broadcast receiver lagged");
                        let _ = socket.send(Message::Text(
                            serde_json::to_string(&serde_json::json!({
                                "error": "LAG_DETECTED",
                                "dropped_events": n,
                                "message": "reconnect recommended",
                            })).unwrap_or_default(),
                        )).await;
                        // Continue â€” the receiver is now caught up
                    }

                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::info!("WebSocket broadcast channel closed â€” shutting down");
                        break;
                    }
                }
            }

            // â”€â”€ Periodic ping â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
            _ = ping_timer.tick() => {
                if socket.send(Message::Ping(vec![])).await.is_err() {
                    break;
                }
            }
        }
    }

    tracing::debug!("WebSocket connection closed");
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Auth negotiation
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Read the initial auth frame from the client and validate the token.
///
/// Returns `true` when the client sent a valid Bearer token.
/// Returns `false` (anonymous) on timeout, missing frame, or wrong token.
///
/// Expected frame format:
///   `{ "type": "auth", "token": "<bearer-token>" }`
async fn negotiate_auth(socket: &mut WebSocket, api_token: &str) -> bool {
    match timeout(AUTH_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if v.get("type").and_then(|t| t.as_str()) == Some("auth") {
                    let token = v.get("token").and_then(|t| t.as_str()).unwrap_or("");
                    if token == api_token {
                        // Acknowledge authentication
                        let _ = socket
                            .send(Message::Text(
                                serde_json::to_string(&serde_json::json!({
                                    "type": "auth_ok",
                                    "rate_limit": AUTHED_LIMIT,
                                    "window_secs": RATE_WINDOW.as_secs(),
                                }))
                                .unwrap_or_default(),
                            ))
                            .await;
                        return true;
                    }
                }
            }
            // Invalid auth frame â€” respond with anonymous rate limits
            let _ = socket
                .send(Message::Text(
                    serde_json::to_string(&serde_json::json!({
                        "type": "auth_failed",
                        "rate_limit": ANON_LIMIT,
                        "window_secs": RATE_WINDOW.as_secs(),
                    }))
                    .unwrap_or_default(),
                ))
                .await;
            false
        }
        // Timeout or non-text frame â€” treat as anonymous
        _ => false,
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;
    use omega_control_contracts::ws::WsEvent;
    use tokio::sync::broadcast;

    // See this file's module-level "Fix (this revision)" note: this is
    // an assumed location, not a confirmed one â€” update if wrong.
    use crate::WS_CHANNEL_CAPACITY;

    #[test]
    fn rate_limits_match_spec() {
        // Â§17.1 fix M4: authenticated 300/min, anonymous 100/min
        assert_eq!(AUTHED_LIMIT, 300);
        assert_eq!(ANON_LIMIT, 100);
        assert_eq!(RATE_WINDOW, Duration::from_secs(60));
    }

    // â”€â”€ Wire format correctness â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    // These tests mirror the frontend's deserialisation expectations.
    // omega_control_contracts::ws::WsEvent actually declares
    // `#[serde(tag = "kind", rename_all = "snake_case")]` â€” INTERNALLY
    // tagged, no `content` wrapper â€” so every serialised frame carries
    // its tag as `"kind"` with all other fields flattened alongside it
    // at the top level, NOT wrapped under a `"payload"` key. See this
    // file's module-level "Fix (this revision, 2)" note for why these
    // tests previously asserted a different (wrong) shape.

    #[test]
    fn config_reloaded_serialises_with_type_and_payload() {
        let event = WsEvent::ConfigReloaded {
            timestamp: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.contains("\"kind\":\"config_reloaded\""),
            "wrong type tag: {json}"
        );
    }

    #[test]
    fn model_pause_changed_serialises_with_type_and_payload() {
        let event = WsEvent::ModelPauseChanged {
            paused: true,
            timestamp: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.contains("\"kind\":\"model_pause_changed\""),
            "wrong type tag: {json}"
        );
        assert!(
            json.contains("\"paused\":true"),
            "paused field missing (should be flattened at top level): {json}"
        );
    }

    #[test]
    fn blacklist_reloaded_serialises_with_type_and_payload() {
        let event = WsEvent::BlacklistReloaded {
            entry_count: 42,
            timestamp: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.contains("\"kind\":\"blacklist_reloaded\""),
            "wrong type tag: {json}"
        );
        assert!(
            json.contains("\"entry_count\":42"),
            "entry_count missing (should be flattened at top level): {json}"
        );
    }

    #[test]
    fn health_transition_serialises_with_type_and_payload() {
        let event = WsEvent::HealthTransition {
            layer: "relay".into(),
            from: "HEALTHY".into(),
            to: "DEGRADED".into(),
            reason: "test".into(),
            timestamp: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.contains("\"kind\":\"health_transition\""),
            "wrong type tag: {json}"
        );
        assert!(
            json.contains("\"layer\":\"relay\""),
            "layer field missing (should be flattened at top level): {json}"
        );
    }

    #[test]
    fn profit_split_serialises_with_type_and_payload() {
        let event = WsEvent::ProfitSplit {
            blueprint_hash: "0xabc".into(),
            pil_share_wei: "1000000000000000000".into(),
            dao_fee_wei: "50000000000000000".into(),
            timestamp: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.contains("\"kind\":\"profit_split\""),
            "wrong type tag: {json}"
        );
        assert!(
            json.contains("\"blueprint_hash\":\"0xabc\""),
            "blueprint_hash missing (should be flattened at top level): {json}"
        );
    }

    #[test]
    fn gas_model_reverted_serialises_with_type_and_payload() {
        let event = WsEvent::GasModelReverted {
            checkpoint_version: 7,
            win_rate: 0.72,
            sample_count: 7000,
            timestamp: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.contains("\"kind\":\"gas_model_reverted\""),
            "wrong type tag: {json}"
        );
        assert!(
            json.contains("\"checkpoint_version\":7"),
            "checkpoint_version missing (should be flattened at top level): {json}"
        );
    }

    #[tokio::test]
    async fn broadcast_channel_capacity() {
        let (tx, mut rx) = broadcast::channel::<WsEvent>(WS_CHANNEL_CAPACITY);
        for _ in 0..WS_CHANNEL_CAPACITY {
            tx.send(WsEvent::ConfigReloaded {
                timestamp: chrono::Utc::now(),
            })
            .unwrap();
        }
        // One more should succeed (replaces oldest for receivers that lagged)
        let _ = tx.send(WsEvent::ConfigReloaded {
            timestamp: chrono::Utc::now(),
        });
        // Receiver should get RecvError::Lagged if it was behind
        let result = rx.try_recv();
        assert!(
            result.is_ok()
                || matches!(
                    result.unwrap_err(),
                    tokio::sync::broadcast::error::TryRecvError::Lagged(_)
                )
        );
    }

    #[tokio::test]
    async fn broadcast_publish_reaches_subscriber() {
        let (tx, mut rx) = broadcast::channel::<WsEvent>(16);
        tx.send(WsEvent::ProfitSplit {
            blueprint_hash: "0x1".into(),
            pil_share_wei: "1000".into(),
            dao_fee_wei: "50".into(),
            timestamp: chrono::Utc::now(),
        })
        .unwrap();
        let received = rx.try_recv();
        assert!(received.is_ok(), "subscriber must receive published event");
        match received.unwrap() {
            WsEvent::ProfitSplit { blueprint_hash, .. } => {
                assert_eq!(blueprint_hash, "0x1");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}
