// Pure experiment metrics. Real report fields only: no estimates, no verdicts.
export function toBig(v) {
  if (v === null || v === undefined || v === "") return null;
  try {
    return BigInt(typeof v === "number" ? Math.trunc(v) : String(v).trim());
  } catch {
    return null;
  }
}

export function summarizeReport(report) {
  const cycles = Array.isArray(report && report.cycles) ? report.cycles : [];
  let attempted = 0, success = 0, errors = 0, realizedN = 0, bothN = 0, gasN = 0;
  let net = 0n, expBoth = 0n, relBoth = 0n, gas = 0n, cum = 0n, peak = 0n, maxDd = 0n;
  let worst = null, best = null;
  const reasons = {};
  const bump = (k) => { reasons[k] = (reasons[k] || 0) + 1; };
  for (const c of cycles) {
    const r = c.receipt || null;
    if (c.opportunity) attempted += 1;
    if (r && r.success) success += 1;
    if (c.error) {
      errors += 1;
      bump(String(c.error).slice(0, 120));
    } else if (r && r.success === false) {
      bump(String(r.revert_reason || "revert (no reason given)").slice(0, 120));
    }
    const real = toBig(r && r.realized_profit_wei);
    const exp = toBig(c.expected_profit_wei);
    if (real !== null) {
      net += real; realizedN += 1;
      if (worst === null || real < worst) worst = real;
      if (best === null || real > best) best = real;
      cum += real;
      if (cum > peak) peak = cum;
      if (peak - cum > maxDd) maxDd = peak - cum;
    }
    if (real !== null && exp !== null) { expBoth += exp; relBoth += real; bothN += 1; }
    const g = toBig(r && r.gas_used);
    if (g !== null) { gas += g; gasN += 1; }
  }
  const ms = report && report.started_at && report.finished_at
    ? Date.parse(report.finished_at) - Date.parse(report.started_at) : null;
  return {
    cycles: cycles.length,
    attempted,
    success,
    failed: Math.max(attempted - success, 0),
    errors,
    success_rate: attempted ? success / attempted : null,
    realized_samples: realizedN,
    net_profit_wei: realizedN ? net.toString() : null,
    mean_net_per_attempt_wei: attempted && realizedN ? (net / BigInt(attempted)).toString() : null,
    worst_cycle_wei: worst === null ? null : worst.toString(),
    best_cycle_wei: best === null ? null : best.toString(),
    max_drawdown_wei: realizedN ? maxDd.toString() : null,
    total_gas_used: gasN ? gas.toString() : null,
    mean_gas_used: gasN ? (gas / BigInt(gasN)).toString() : null,
    realized_over_expected: bothN && expBoth !== 0n ? Number((relBoth * 10000n) / expBoth) / 10000 : null,
    expected_realized_pairs: bothN,
    top_failure_reasons: Object.entries(reasons).sort((a, b) => b[1] - a[1]).slice(0, 5)
      .map(([reason, count]) => ({ reason, count })),
    duration_ms: Number.isFinite(ms) ? ms : null,
  };
}

const ROWS = [
  ["attempted", "Attempted cycles", "count"],
  ["success", "Successful", "count"],
  ["failed", "Failed", "count"],
  ["success_rate", "Success rate", "pct"],
  ["net_profit_wei", "Net realized profit", "wei"],
  ["mean_net_per_attempt_wei", "Mean net per attempted cycle", "wei"],
  ["worst_cycle_wei", "Worst single cycle", "wei"],
  ["best_cycle_wei", "Best single cycle", "wei"],
  ["max_drawdown_wei", "Max drawdown (cycle order)", "wei"],
  ["total_gas_used", "Total gas used", "count"],
  ["mean_gas_used", "Mean gas per receipt", "count"],
  ["realized_over_expected", "Realized / expected profit", "ratio"],
];

export function compareSummaries(base, cand) {
  return ROWS.map(([key, label, kind]) => {
    const b = base && base[key] !== undefined ? base[key] : null;
    const c = cand && cand[key] !== undefined ? cand[key] : null;
    let delta = null;
    if (b !== null && c !== null) {
      if (kind === "wei" || kind === "count") {
        const bb = toBig(b), cc = toBig(c);
        if (bb !== null && cc !== null) delta = (cc - bb).toString();
      } else {
        delta = Number(c) - Number(b);
      }
    }
    return { key, label, kind, base: b, cand: c, delta };
  });
}

export function comparisonWarnings(base, cand) {
  const w = [];
  for (const [name, s] of [["Baseline", base], ["Candidate", cand]]) {
    if (!s) continue;
    if (s.attempted === 0) w.push(name + ": no opportunities were attempted, so there is nothing to compare.");
    else if (s.attempted < 30) w.push(name + ": only " + s.attempted + " attempted cycles. Differences at this size are weak evidence.");
    if (s.attempted > 0 && s.realized_samples < s.attempted) {
      w.push(name + ": " + (s.attempted - s.realized_samples) + " attempted cycles have no realized profit figure and are not in the profit totals.");
    }
  }
  return w;
}