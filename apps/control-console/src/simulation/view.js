// apps/control-console/src/simulation/view.js
import { cyclesToSimEconomics } from "../shared/compare.js";
import { pct, weiToEthDisplay } from "../shared/format.js";

export function renderSimulationLab(state) {
  const report = state.simReport;
  const summary = report ? summarize(report) : null;

  return `
    <section class="sim-lab">
      <header class="topbar">
        <div>
          <p class="eyebrow tag sim">SIMULATION LAB</p>
          <h1>Independent simulation</h1>
        </div>
        <span class="pill warn">no wallet · no broadcast</span>
      </header>

      <section class="panel">
        <h2>Load SimulationReport JSON</h2>
        <p class="econ-note">Produced by <code>omega-simulation</code> (<code>--report path.json</code>). This UI does not run the harness in-browser and cannot submit to live relays.</p>
        <textarea id="simReportJson" rows="8" placeholder='Paste SimulationReport JSON from crates/omega-simulation'></textarea>
        <div class="button-grid">
          <button type="button" data-action="loadSimReport">Load report</button>
          <button type="button" data-action="clearSimReport">Clear</button>
        </div>
        ${state.simReportError ? `<p class="errors"><span>${escapeAttr(state.simReportError)}</span></p>` : ""}
      </section>

      ${summary ? renderSummary(summary, report) : `<p class="empty">No report loaded.</p>`}
    </section>
  `;
}

function summarize(report) {
  const cycles = report.cycles || [];
  const attempted = cycles.filter((c) => c.opportunity).length;
  const success = cycles.filter((c) => c.receipt?.success).length;
  const failed = attempted - success;
  let net = 0n;
  for (const c of cycles) {
    const p = c.receipt?.realized_profit_wei;
    if (p === undefined || p === null) continue;
    try {
      net += BigInt(String(p));
    } catch {
      /* ignore non-integer */
    }
  }
  return {
    cycles: cycles.length,
    attempted,
    success,
    failed,
    successRate: attempted ? success / attempted : 0,
    netProfitWei: net.toString(),
    fork: report.fork_endpoint,
    started: report.started_at,
    finished: report.finished_at,
  };
}

function renderSummary(s, report) {
  const rows = cyclesToSimEconomics(report);
  return `
    <section class="metrics">
      ${metric("Cycles", s.cycles)}
      ${metric("Attempted", s.attempted)}
      ${metric("Success", s.success)}
      ${metric("Failed", s.failed)}
      ${metric("Success rate", pct(s.successRate))}
      ${metric("Net profit", weiToEthDisplay(s.netProfitWei))}
    </section>
    <section class="panel">
      <h2>Run metadata</h2>
      <div class="detail">
        <span>Fork</span><strong title="${escapeAttr(s.fork)}">${escapeAttr(s.fork || "—")}</strong>
        <span>Started</span><strong>${escapeAttr(s.started || "—")}</strong>
        <span>Finished</span><strong>${escapeAttr(s.finished || "—")}</strong>
      </div>
    </section>
    <section class="panel">
      <h2>Cycles (${rows.length})</h2>
      <div class="table">
        ${rows
          .map(
            (r) => `
          <div class="row">
            <strong>${escapeAttr(r.tx_id)}</strong>
            <span>${escapeAttr(r.execution_status)}</span>
            <span>${weiToEthDisplay(r.realized_profit_wei)}</span>
            <span>${escapeAttr(r.error || "")}</span>
          </div>`,
          )
          .join("") || `<p class="empty">No cycles</p>`}
      </div>
    </section>
  `;
}

function metric(title, value) {
  return `<article class="metric neutral"><span>${title}</span><strong>${value}</strong></article>`;
}

function escapeAttr(v) {
  return String(v ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll('"', "&quot;")
    .replaceAll("<", "&lt;");
}