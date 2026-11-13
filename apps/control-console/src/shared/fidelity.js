// apps/control-console/src/shared/fidelity.js
/**
 * Simulation fidelity from omega-simulation CycleResult fields.
 * Primary signal available today: expected_profit_wei vs receipt.realized_profit_wei.
 * Live-vs-sim pairing uses the same helpers when both sides exist.
 */

function toBigInt(v) {
  if (v === null || v === undefined || v === "") return null;
  try {
    return BigInt(String(v));
  } catch {
    return null;
  }
}

function toNum(v) {
  if (v === null || v === undefined || v === "") return null;
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
}

/** Per-cycle fidelity row from a SimulationReport cycle */
export function cycleFidelity(cycle) {
  const expected = toBigInt(cycle.expected_profit_wei);
  const realized = toBigInt(cycle.receipt?.realized_profit_wei);
  const gas = toNum(cycle.receipt?.gas_used);

  let profitError = null;
  let profitErrorAbs = null;
  let profitErrorRel = null;
  let attribution = "missing_or_unavailable";

  if (cycle.error) {
    attribution = "execution_failure";
  } else if (expected !== null && realized !== null) {
    profitError = realized - expected; // actual − predicted (fork)
    profitErrorAbs = profitError < 0n ? -profitError : profitError;
    if (expected !== 0n) {
      // relative error as number for display only
      profitErrorRel = Number(profitError) / Number(expected);
    } else {
      profitErrorRel = null;
    }
    attribution = "prediction_error"; // model vs fork outcome; not market-move unless live paired
  } else if (expected !== null && realized === null) {
    attribution = "missing_or_unavailable";
  } else if (!cycle.opportunity) {
    attribution = "no_opportunity";
  }

  const opp = cycle.opportunity || {};
  return {
    cycle_index: cycle.cycle_index,
    block_number: cycle.block_number,
    strategy_id: opp.strategy_id || opp.kind || null,
    token_pair: opp.token_pair || opp.pair || null,
    route: opp.route || opp.execution_route || null,
    network: opp.chain_id || null,
    expected_profit_wei: expected !== null ? expected.toString() : null,
    realized_profit_wei: realized !== null ? realized.toString() : null,
    profit_error_wei: profitError !== null ? profitError.toString() : null,
    profit_error_abs_wei: profitErrorAbs !== null ? profitErrorAbs.toString() : null,
    profit_error_rel: profitErrorRel,
    gas_used: gas,
    success: cycle.receipt?.success ?? null,
    attribution,
    error: cycle.error || null,
  };
}

export function buildFidelityReport(simReport, livePairedRows = []) {
  const cycles = simReport?.cycles || [];
  const cycleRows = cycles.map(cycleFidelity);

  // Aggregate prediction error (expected vs realized on fork)
  const withBoth = cycleRows.filter((r) => r.profit_error_wei !== null);
  let sumAbs = 0n;
  let sumErr = 0n;
  for (const r of withBoth) {
    sumAbs += BigInt(r.profit_error_abs_wei);
    sumErr += BigInt(r.profit_error_wei);
  }

  const byStrategy = groupErrors(withBoth, (r) => r.strategy_id || "unknown");
  const byPair = groupErrors(withBoth, (r) => r.token_pair || "unknown");
  const byRoute = groupErrors(withBoth, (r) => r.route || "unknown");
  const byNetwork = groupErrors(withBoth, (r) => String(r.network ?? "unknown"));

  const attributionCounts = countBy(cycleRows, (r) => r.attribution);

  // Live-vs-sim: only where Phase C paired rows exist with numeric net fields
  const liveSim = [];
  for (const pair of livePairedRows) {
    if (pair.status !== "paired" || !pair.live || !pair.sim) continue;
    const liveNet = toNum(pair.live?.pnl?.net ?? pair.live?.realized_profit_wei);
    const simNet = toNum(pair.sim?.pnl?.net ?? pair.sim?.realized_profit_wei);
    if (liveNet === null || simNet === null) {
      liveSim.push({
        id: pair.id,
        status: "missing_or_unavailable",
        note: "Paired identity but net P&L missing on one or both sides",
      });
      continue;
    }
    const delta = liveNet - simNet;
    liveSim.push({
      id: pair.id,
      status: "live_vs_sim",
      live_net: liveNet,
      sim_net: simNet,
      difference: delta, // LIVE − SIM
      // Without timestamps/oracle marks we cannot split market_move vs model
      attribution: "execution_or_model_mixed",
      note: "Requires richer timestamps/marks to separate market movement from prediction error",
    });
  }

  return {
    sample_size: cycleRows.length,
    comparable_profit_samples: withBoth.length,
    mean_abs_profit_error_wei: withBoth.length ? (sumAbs / BigInt(withBoth.length)).toString() : null,
    mean_profit_error_wei: withBoth.length ? (sumErr / BigInt(withBoth.length)).toString() : null,
    attribution_counts: attributionCounts,
    by_strategy: byStrategy,
    by_token_pair: byPair,
    by_route: byRoute,
    by_network: byNetwork,
    cycles: cycleRows,
    live_vs_sim: liveSim,
    limitations: [
      "On-fork expected vs realized is model/execution fidelity on forked state, not mainnet inclusion.",
      "Live-vs-sim net comparison only appears when both ledgers expose paired records.",
      "Market movement vs pure prediction error is not classified without mark-out timestamps.",
      "Purchase/sale/slippage/fee line fidelity awaits full economics ledger fields.",
    ],
  };
}

function groupErrors(rows, keyFn) {
  const map = new Map();
  for (const r of rows) {
    const k = keyFn(r);
    if (!map.has(k)) map.set(k, { key: k, n: 0, sumAbs: 0n, sumErr: 0n });
    const g = map.get(k);
    g.n += 1;
    g.sumAbs += BigInt(r.profit_error_abs_wei);
    g.sumErr += BigInt(r.profit_error_wei);
  }
  return [...map.values()]
    .map((g) => ({
      key: g.key,
      n: g.n,
      mean_abs_error_wei: (g.sumAbs / BigInt(g.n)).toString(),
      mean_error_wei: (g.sumErr / BigInt(g.n)).toString(),
    }))
    .sort((a, b) => (BigInt(b.mean_abs_error_wei) > BigInt(a.mean_abs_error_wei) ? 1 : -1));
}

function countBy(rows, keyFn) {
  const out = {};
  for (const r of rows) {
    const k = keyFn(r);
    out[k] = (out[k] || 0) + 1;
  }
  return out;
}