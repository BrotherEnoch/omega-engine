import { mockBlacklist, mockCeiling, mockCheckpoints, mockDaoFee, mockHealth } from "./mock.js";

export const store = {
  revision: 0,
  apiBase: localStorage.getItem("omega.console.apiBase") || window.location.origin,
  token: localStorage.getItem("omega.console.token") || "",
  mode: "live",
  connection: "disconnected",
  health: null,
  daoFee: null,
  blacklist: null,
  ceiling: null,
  checkpoints: [],
  config: null,
  events: [],
  errors: [],
  selectedLayer: null,
  pending: new Set(),
};

const subscribers = new Set();

export function subscribe(fn) {
  subscribers.add(fn);
  fn(store);
  return () => subscribers.delete(fn);
}

export function mutate(fn) {
  fn(store);
  store.revision += 1;
  for (const fn of subscribers) fn(store);
}

export function setPending(key, isPending) {
  mutate((state) => {
    if (isPending) state.pending.add(key);
    else state.pending.delete(key);
  });
}

export function pushError(message) {
  mutate((state) => {
    state.errors.unshift({ message, at: new Date().toISOString() });
    state.errors = state.errors.slice(0, 6);
  });
}

export function pushEvent(event) {
  mutate((state) => {
    state.events.unshift({ ...event, received_at: new Date().toISOString() });
    state.events = state.events.slice(0, 40);
    applyEventToSnapshot(state, event);
  });
}

export function useMockData(reason = "Backend unavailable") {
  mutate((state) => {
    state.mode = "mock";
    state.connection = "mock";
    state.health = structuredClone(mockHealth);
    state.daoFee = structuredClone(mockDaoFee);
    state.blacklist = structuredClone(mockBlacklist);
    state.ceiling = structuredClone(mockCeiling);
    state.checkpoints = structuredClone(mockCheckpoints);
    state.events = [
      {
        kind: "console_notice",
        message: reason,
        received_at: new Date().toISOString(),
      },
    ];
  });
}

function applyEventToSnapshot(state, event) {
  if (event.kind === "health_transition" && state.health) {
    const layer = state.health.layers.find((item) => item.layer === event.layer);
    if (layer) {
      layer.state = event.to;
      layer.is_operational = event.to !== "HALTED";
    }
    state.health.system_halted = state.health.layers.some((item) => item.state === "HALTED");
  }

  if (event.kind === "model_pause_changed") {
    state.ceiling = { ...(state.ceiling || {}), paused: event.paused };
  }

  if (event.kind === "blacklist_reloaded" && state.blacklist) {
    state.blacklist.entry_count = event.entry_count;
    state.blacklist.is_empty = event.entry_count === 0;
  }

  if (event.kind === "ceiling_escalation") {
    state.ceiling = {
      ...(state.ceiling || {}),
      consecutive_ceiling_hits: event.consecutive_hits,
      paused: event.paused,
    };
  }
}
