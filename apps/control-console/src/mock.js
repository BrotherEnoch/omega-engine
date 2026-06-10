const now = () => new Date().toISOString();

export const layerOrder = [
  "system_health",
  "external_data",
  "eil",
  "risk",
  "security",
  "chaos_guard",
  "dag",
  "zk",
  "hot_path",
  "strategy",
  "flashloan",
  "orchestrator",
  "relay",
  "vault",
  "observability",
  "loss_attribution",
];

export const mockHealth = {
  generated_at: now(),
  system_halted: false,
  layers: layerOrder.map((layer, index) => ({
    layer,
    state: index === 12 ? "DEGRADED" : "HEALTHY",
    is_operational: true,
  })),
};

export const mockDaoFee = {
  dao_fee_bps: 500,
  dao_fee_pct: 5,
  per_transfer_cap_eth: 50,
  daily_cap_eth: 500,
  confirmation_depth: 12,
};

export const mockBlacklist = {
  entry_count: 3,
  path: "config/builder_blacklist.toml",
  is_empty: false,
};

export const mockCeiling = {
  paused: false,
  consecutive_ceiling_hits: 2,
  escalation_threshold: 100,
  trigger_key: "gas_ceiling:relay",
  last_hit_at: now(),
  paused_at: null,
};

export const mockCheckpoints = [
  { version: 42, win_rate: 0.73, sample_count: 1800, saved_at: now() },
  { version: 41, win_rate: 0.71, sample_count: 1732, saved_at: now() },
  { version: 40, win_rate: 0.68, sample_count: 1620, saved_at: now() },
];
