// apps/control-console/src/realtime.js
import { routes } from "./contracts.js";
import { mutate, pushError, pushEvent, store } from "./state.js";

let socket = null;
let reconnectTimer = null;

export function connectRealtime() {
  disconnectRealtime();

  let base;
  try {
    base = new URL(store.apiBase);
  } catch {
    pushError("The API address is not a valid URL. Fix it on the Connection tab.");
    return;
  }
  base.protocol = base.protocol === "https:" ? "wss:" : "ws:";
  base.pathname = routes.wsEvents.path;
  base.search = "";

  mutate((state) => {
    state.connection = "connecting";
  });

  const ws = new WebSocket(base.toString());
  socket = ws;

  ws.addEventListener("open", () => {
    if (ws !== socket) return;
    mutate((state) => {
      state.connection = "connected";
    });
    if (store.token) {
      ws.send(JSON.stringify({ type: "auth", token: store.token }));
    }
  });

  ws.addEventListener("message", (message) => {
    if (ws !== socket) return;
    try {
      const payload = JSON.parse(message.data);
      if (payload.type === "auth_ok") {
        mutate((state) => {
          state.connection = "authenticated";
        });
        return;
      }
      if (payload.type === "auth_failed") {
        mutate((state) => {
          state.connection = "anonymous";
        });
        pushError("The stream rejected the token. Check it on the Connection tab.");
        return;
      }
      if (payload.error === "LAG_DETECTED") {
        mutate((state) => {
          state.connection = "lagged";
        });
        return;
      }
      pushEvent(payload);
    } catch (error) {
      pushError(`Realtime payload error: ${error.message}`);
    }
  });

  // Ignore events from sockets we replaced; otherwise each replaced socket
  // schedules a reconnect that kills the healthy one.
  ws.addEventListener("close", () => {
    if (ws !== socket) return;
    socket = null;
    mutate((state) => {
      if (state.mode === "live") state.connection = "disconnected";
    });
    reconnectTimer = window.setTimeout(connectRealtime, 5000);
  });

  ws.addEventListener("error", () => {
    if (ws !== socket) return;
    pushError("Realtime stream unavailable. Retrying every 5 seconds.");
  });
}

export function disconnectRealtime() {
  if (reconnectTimer) window.clearTimeout(reconnectTimer);
  reconnectTimer = null;
  const old = socket;
  socket = null;
  if (old) old.close();
}