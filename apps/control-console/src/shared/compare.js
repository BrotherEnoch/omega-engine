// apps/control-console/src/shared/compare.js
/** Pure comparison: DIFF = LIVE − SIM. Never invent a missing side. */

export function pairKey(rec) {
  if (!rec) return null;
  if (rec.correlation_id) return `corr:${rec.correlation_id}`;
  if (rec.opportunity_id) return `opp:${rec.opportunity_id}`;
  if (rec.tx_id) return `txid:${rec.tx_id}`;
  const parts = [
    rec.strategy_id,
    rec.strategy_version,
    rec.chain_id,
    rec.token_pair,
    rec.logical_trade_id,
  ].filter((x) => x !== undefined && x !== null && x !== "");
  if (parts.length >= 4) return `strat:${parts.join("|")}`;
  return null;
}

/**
 * @param {object|null} live
 * @param {object|null} sim
 * @param {string[]} metricKeys paths into accounting object
 */
export function compareRecords(live, sim, metricKeys) {
  if (!live && !sim) {
    return { status: "empty", live: null, sim: null, rows: [] };
  }
  if (!live) {
    return { status: "unmatched_sim", live: null, sim, rows: metricKeys.map((k) => row(k, null, get(sim, k))) };
  }
  if (!sim) {
    return { status: "unmatched_live", live, sim: null, rows: metricKeys.map((k) => row(k, get(live, k), null)) };
  }
  return {
    status: "paired",
    live,
    sim,
    rows: metricKeys.map((k) => {
      const lv = get(live, k);
      const sv = get(sim, k);
      return row(k, lv, sv);
    }),
  };
}

function row(key, liveVal, simVal) {
  const liveN = toNum(liveVal);
  const simN = toNum(simVal);
  let diff = null;
  if (liveN !== null && simN !== null) diff = liveN - simN;
  return {
    key,
    live: liveVal ?? null,
    simulation: simVal ?? null,
    difference: diff,
    liveMissing: liveVal === null || liveVal === undefined || liveVal === "",
    simMissing: simVal === null || simVal === undefined || simVal === "",
  };
}

function get(obj, path) {
  if (!obj) return null;
  return path.split(".").reduce((acc, part) => (acc == null ? null : acc[part]), obj);
}

function toNum(v) {
  if (v === null || v === undefined || v === "") return null;
  if (typeof v === "number" && Number.isFinite(v)) return v;
  if (typeof v === "string" && /^-?\d+(\.\d+)?$/.test(v)) return Number(v);
  return null;
}

/** Build sparse economics rows from SimulationReport cycles (real fields only). */
export function cyclesToSimEconomics(report) {
  if (!report?.cycles) return [];
  return report.cycles.map((c, i) => {
    const opp = c.opportunity || {};
    const receipt = c.receipt || {};
    return {
      source: "simulation",
      tx_id: `sim-cycle-${c.cycle_index ?? i}`,
      correlation_id: opp.id || opp.opportunity_id || null,
      strategy_id: opp.strategy_id || opp.kind || null,
      strategy_version: opp.strategy_version || null,
      token_pair: opp.token_pair || opp.pair || null,
      chain_id: opp.chain_id || null,
      block_number: c.block_number,
      execution_status: c.error ? "failed" : receipt.success ? "success" : "unknown",
      expected_profit_wei: c.expected_profit_wei ?? null,
      realized_profit_wei: receipt.realized_profit_wei ?? null,
      gas_used: receipt.gas_used ?? null,
      error: c.error || null,
      // Detailed purchase/sale/cost lines are unavailable until backend ledger exists
      accounting_complete: false,
    };
  });
}

export const ECONOMICS_METRIC_KEYS = [
  "purchase.avg_price",
  "sale.avg_price",
  "purchase.entry_slippage",
  "sale.exit_slippage",
  "costs.entry_gas",
  "costs.exit_gas",
  "costs.protocol_fees",
  "costs.flash_loan_premium",
  "costs.total",
  "pnl.gross",
  "pnl.net",
  "pnl.net_roi",
  "realized_profit_wei",
  "expected_profit_wei",
  "gas_used",
];