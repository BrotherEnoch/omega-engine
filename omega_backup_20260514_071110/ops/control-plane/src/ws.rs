ï»¿// ops/control-plane/src/ws.rs
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

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use tokio::time::timeout;

use crate::state::{AppState, WsEvent};

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
/// Route: GET /ws/events
pub async fn events_handler(
    ws:           WebSocketUpgrade,
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
    let msg_limit        = if is_authenticated { AUTHED_LIMIT } else { ANON_LIMIT };

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
    let mut msg_count    = 0u32;
    let mut window_start = Instant::now();
    let mut ping_timer   = tokio::time::interval(PING_INTERVAL);
    ping_timer.tick().await; // consume first tick (fires immediately)

    loop {
        // Reset rate-limit window
        if window_start.elapsed() >= RATE_WINDOW {
            msg_count    = 0;
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
                        let _ = socket.send(Message::Text(
                            serde_json::to_string(&serde_json::json!({
                                "type": "auth_ok",
                                "rate_limit": AUTHED_LIMIT,
                                "window_secs": RATE_WINDOW.as_secs(),
                            })).unwrap_or_default(),
                        )).await;
                        return true;
                    }
                }
            }
            // Invalid auth frame â€” respond with anonymous rate limits
            let _ = socket.send(Message::Text(
                serde_json::to_string(&serde_json::json!({
                    "type": "auth_failed",
                    "rate_limit": ANON_LIMIT,
                    "window_secs": RATE_WINDOW.as_secs(),
                })).unwrap_or_default(),
            )).await;
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
    use crate::state::WS_CHANNEL_CAPACITY;
    use tokio::sync::broadcast;

    #[test]
    fn rate_limits_match_spec() {
        // Â§17.1 fix M4: authenticated 300/min, anonymous 100/min
        assert_eq!(AUTHED_LIMIT, 300);
        assert_eq!(ANON_LIMIT,   100);
        assert_eq!(RATE_WINDOW,  Duration::from_secs(60));
    }

    #[test]
    fn ws_event_serialises_correctly() {
        let event = WsEvent::ConfigReloaded {
            timestamp: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"kind\":\"config_reloaded\""));
    }

    #[test]
    fn model_pause_event_serialises() {
        let event = WsEvent::ModelPauseChanged {
            paused:    true,
            timestamp: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"kind\":\"model_pause_changed\""));
        assert!(json.contains("\"paused\":true"));
    }

    #[test]
    fn blacklist_event_serialises() {
        let event = WsEvent::BlacklistReloaded {
            entry_count: 42,
            timestamp:   chrono::Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"kind\":\"blacklist_reloaded\""));
        assert!(json.contains("\"entry_count\":42"));
    }

    #[test]
    fn health_transition_event_serialises() {
        let event = WsEvent::HealthTransition {
            layer:     "relay".into(),
            from:      "HEALTHY".into(),
            to:        "DEGRADED".into(),
            reason:    "test".into(),
            timestamp: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"kind\":\"health_transition\""));
        assert!(json.contains("\"layer\":\"relay\""));
    }

    #[tokio::test]
    async fn broadcast_channel_capacity() {
        let (tx, mut rx) = broadcast::channel::<WsEvent>(WS_CHANNEL_CAPACITY);
        for _ in 0..WS_CHANNEL_CAPACITY {
            tx.send(WsEvent::ConfigReloaded { timestamp: chrono::Utc::now() }).unwrap();
        }
        // One more should succeed (replaces oldest for receivers that lagged)
        let _ = tx.send(WsEvent::ConfigReloaded { timestamp: chrono::Utc::now() });
        // Receiver should get RecvError::Lagged if it was behind
        let result = rx.try_recv();
        // Either we get the first event or a Lagged error â€” both are valid
        assert!(result.is_ok() || matches!(
            result.unwrap_err(),
            tokio::sync::broadcast::error::TryRecvError::Lagged(_)
        ));
    }
}