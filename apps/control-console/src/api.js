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

    for (const result of [daoFee, blacklist, ceiling, checkpoints]) {
      if (result.status === "rejected") pushError(result.reason.message);
    }
  } catch (error) {
    pushError(error.message);
    useMockData(error.message);
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

  const response = await fetch(url, {
    method: route.method,
    headers,
    body: body === undefined || body === null ? undefined : JSON.stringify(body),
  });

  const text = await response.text();
  const payload = text ? JSON.parse(text) : null;

  if (!response.ok) {
    const message = payload?.message || `${route.method} ${route.path} returned ${response.status}`;
    throw new Error(message);
  }

  return payload;
}
