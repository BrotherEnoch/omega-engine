import { routes } from "./contracts.js";
import { mutate, pushError, pushEvent, store } from "./state.js";

let socket = null;
let reconnectTimer = null;

export function connectRealtime() {
  disconnectRealtime();

  const base = new URL(store.apiBase);
  base.protocol = base.protocol === "https:" ? "wss:" : "ws:";
  base.pathname = routes.wsEvents.path;
  base.search = "";

  mutate((state) => {
    state.connection = "connecting";
  });

  socket = new WebSocket(base.toString());

  socket.addEventListener("open", () => {
    mutate((state) => {
      state.connection = "connected";
    });
    if (store.token) {
      socket.send(JSON.stringify({ type: "auth", token: store.token }));
    }
  });

  socket.addEventListener("message", (message) => {
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

  socket.addEventListener("close", () => {
    mutate((state) => {
      if (state.mode === "live") state.connection = "disconnected";
    });
    reconnectTimer = window.setTimeout(connectRealtime, 5000);
  });

  socket.addEventListener("error", () => {
    pushError("Realtime stream unavailable");
  });
}

export function disconnectRealtime() {
  if (reconnectTimer) window.clearTimeout(reconnectTimer);
  reconnectTimer = null;
  if (socket) socket.close();
  socket = null;
}
