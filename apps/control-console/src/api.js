// apps/control-console/src/api.js â€” safer JSON handling, clearer 401 message, de-duplicated errors
import { API_BASE_KEY, TOKEN_KEY, authTier, clearHaltPath, revertCheckpointPath, routes } from "./contracts.js";
import { mutate, pushError, setPending, store, useMockData } from "./state.js";

export function saveSettings({ apiBase, token }) {
  mutate((state) => {
    state.apiBase = apiBase.replace(/\/$/, "");
    state.token = token;
  });
  localStorage.setItem(API_BASE_KEY, store.apiBase);
  localStorage.setItem(TOKEN_KEY, store.token);
}

export async function refreshAll() {
  setPending("refreshAll", true);
  mutate((state) => {
    state.errors = [];
  });
  try {
    const [health, daoFee, blacklist, ceiling, checkpoints] = await Promise.allSettled([
      request(routes.health),
      request(routes.daoFee),
      request(routes.builderBlacklist),
      request(routes.ceilingStatus),
      request(routes.checkpoints),
    ]);

    if (health.status !== "fulfilled") throw health.reason;

    mutate((state) => {
      state.mode = "live";
      state.health = health.value;
      if (daoFee.status === "fulfilled") state.daoFee = daoFee.value;
      if (blacklist.status === "fulfilled") state.blacklist = blacklist.value;
      if (ceiling.status === "fulfilled") state.ceiling = ceiling.value;
      if (checkpoints.status === "fulfilled") state.checkpoints = checkpoints.value;
    });

    const messages = new Set(
      [daoFee, blacklist, ceiling, checkpoints].filter((r) => r.status === "rejected").map((r) => r.reason.message),
    );
    for (const message of messages) pushError(message);
  } catch (error) {
    pushError(error.message);
    mutate((state) => { state.mode = "offline"; state.connection = "disconnected"; state.health = null; state.daoFee = null; state.blacklist = null; state.ceiling = null; state.checkpoints = []; });
  } finally {
    setPending("refreshAll", false);
  }
}

export async function reloadConfig() {
  await command("reloadConfig", () => request(routes.reloadConfig, { from_disk: true, body: null }));
}

export async function unpauseModel() {
  await command("unpauseModel", () => request(routes.unpauseModel, null));
  await refreshAll();
}

export async function reloadBuilderBlacklist() {
  await command("reloadBuilderBlacklist", () => request(routes.reloadBuilderBlacklist, null));
  await refreshAll();
}

export async function clearHalt(layer) {
  await command(`clearHalt:${layer}`, () =>
    request({ method: "POST", path: clearHaltPath(layer), auth: authTier.l2 }, null),
  );
  await refreshAll();
}

export async function revertCheckpoint(version) {
  await command(`revert:${version}`, () =>
    request({ method: "POST", path: revertCheckpointPath(version), auth: authTier.l2 }, null),
  );
  await refreshAll();
}

async function command(key, fn) {
  setPending(key, true);
  try {
    await fn();
  } catch (error) {
    pushError(error.message);
  } finally {
    setPending(key, false);
  }
}

async function request(route, body) {
  const url = `${store.apiBase}${route.path}`;
  const headers = { Accept: "application/json" };
  if (route.auth !== authTier.public && store.token) {
    headers.Authorization = `Bearer ${store.token}`;
  }
  if (body !== undefined && body !== null) {
    headers["Content-Type"] = "application/json";
  }

  let response;
  try {
    response = await fetch(url, {
      method: route.method,
      headers,
      body: body === undefined || body === null ? undefined : JSON.stringify(body),
    });
  } catch {
    throw new Error(`Cannot reach ${store.apiBase}. Is omega-runtime running?`);
  }

  const text = await response.text();
  let payload = null;
  if (text) {
    try {
      payload = JSON.parse(text);
    } catch {
      /* non-JSON body (HTML error page etc.) */
    }
  }

  if (response.status === 401) {
    throw new Error(
      store.token
        ? "Token rejected by the runtime. Check it on the Connection tab."
        : "This action needs an API token. Add it on the Connection tab.",
    );
  }
  if (response.status === 403) {
    throw new Error(`${route.path} needs a higher-privilege (${route.auth}) token than the one saved.`);
  }
  if (!response.ok) {
    throw new Error(payload?.message || `${route.method} ${route.path} returned ${response.status}`);
  }
  if (text && payload === null) {
    throw new Error(`${route.path} did not return JSON. Check the API address.`);
  }
  return payload;
}

