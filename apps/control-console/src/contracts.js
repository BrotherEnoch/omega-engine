export const API_BASE_KEY = "omega.console.apiBase";
export const TOKEN_KEY = "omega.console.token";

export const authTier = Object.freeze({
  public: "public",
  l1: "l1",
  l2: "l2",
});

export const method = Object.freeze({
  get: "GET",
  post: "POST",
});

export const routes = Object.freeze({
  liveness: { method: method.get, path: "/health", auth: authTier.public },
  health: { method: method.get, path: "/api/v1/health", auth: authTier.public },
  config: { method: method.get, path: "/api/v1/config", auth: authTier.l1 },
  reloadConfig: { method: method.post, path: "/api/v1/config", auth: authTier.l1 },
  checkpoints: {
    method: method.get,
    path: "/api/v1/la/gas-model/checkpoints",
    auth: authTier.l1,
  },
  ceilingStatus: {
    method: method.get,
    path: "/api/v1/la/gas-model/ceiling-status",
    auth: authTier.l1,
  },
  unpauseModel: {
    method: method.post,
    path: "/api/v1/la/gas-model/unpause",
    auth: authTier.l2,
  },
  daoFee: { method: method.get, path: "/api/v1/vault/dao-fee", auth: authTier.l1 },
  builderBlacklist: {
    method: method.get,
    path: "/api/v1/builders/blacklist",
    auth: authTier.l1,
  },
  reloadBuilderBlacklist: {
    method: method.post,
    path: "/api/v1/builders/blacklist/update",
    auth: authTier.l2,
  },
  wsEvents: { method: method.get, path: "/ws/events", auth: authTier.public },
});

export function revertCheckpointPath(version) {
  return `/api/v1/la/gas-model/revert/${encodeURIComponent(version)}`;
}

export function clearHaltPath(layer) {
  return `/api/v1/health/clear-halt/${encodeURIComponent(layer)}`;
}
