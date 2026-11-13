// apps/control-console/src/fidelity/view.js
import { buildFidelityReport } from "../shared/fidelity.js";
import { weiToEthDisplay, money, pct } from "../shared/format.js";

export function renderFidelityDashboard(state) {
  if (!state.simReport) {
    return `
      <section class="fidelity">
        <header class="topbar">
          <div>
            <p class="eyebrow tag sim">SIMULATION FIDELITY</p>
            <h1>Prediction accuracy</h1>
          </div>
        </header>
        <p class="empty">Load a SimulationReport on <a href="#/simulation">#/simulation</a> first. No fabricated accuracy metrics.</p>
      </section>`;
  }

  const report = buildFidelityReport(state.simReport, state.econIndex || []);
  const attr = report.attribution_counts || {};

  return `
    <section class="fidelity">
      <header class="topbar">
        <div>
          <p class="eyebrow tag sim">SIMULATION FIDELITY</p>
          <h1>Expected vs realized (fork)</h1>
        </div>
        <span class="pill warn">samples ${report.comparable_profit_samples}/${report.sample_size}</span>
      </header>

      <section class="metrics">
        ${metric("Comparable", report.comparable_profit_samples)}
        ${metric("Mean |error|", weiToEthDisplay(report.mean_abs_profit_error_wei))}
        ${metric("Mean error", weiToEthDisplay(report.mean_profit_error_wei))}
        ${metric("Pred. error rows", attr.prediction_error || 0)}
        ${metric("Exec failures", attr.execution_failure || 0)}
        ${metric("Missing data", attr.missing_or_unavailable || 0)}
      </section>

      <section class="panel">
        <h2>Error attribution (honest labels)</h2>
        <ul class="attr-list">
          <li><strong>prediction_error</strong> — both expected and realized profit present; delta is model vs fork execution.</li>
          <li><strong>execution_failure</strong> — cycle error string set; not scored as price prediction.</li>
          <li><strong>missing_or_unavailable</strong> — required fields absent.</li>
          <li><strong>no_opportunity</strong> — empty cycle; not an accuracy miss.</li>
          <li><strong>execution_or_model_mixed</strong> — live−sim net only; cannot split market move without marks.</li>
        </ul>
        <div class="detail">
          ${Object.entries(attr)
            .map(([k, v]) => `<span>${k}</span><strong>${v}</strong>`)
            .join("")}
        </div>
      </section>

      ${renderGroup("By strategy", report.by_strategy)}
      ${renderGroup("By token pair", report.by_token_pair)}
      ${renderGroup("By route", report.by_route)}
      ${renderGroup("By network", report.by_network)}

      <section class="panel">
        <h2>Live vs simulation (paired only)</h2>
        ${
          report.live_vs_sim.length
            ? `<div class="table">${report.live_vs_sim
                .map(
                  (r) => `
              <div class="row">
                <strong>${esc(r.id)}</strong>
                <span>${esc(r.status)}</span>
                <span class="col-live">${r.live_net ?? "—"}</span>
                <span class="col-sim">${r.sim_net ?? "—"}</span>
                <span class="col-diff">${r.difference ?? "—"}</span>
                <span>${esc(r.note || "")}</span>
              </div>`,
                )
                .join("")}</div>`
            : `<p class="empty">No paired live/sim economics rows. Live ledger API still required for mainnet reconciliation.</p>`
        }
      </section>

      <section class="panel">
        <h2>Per-cycle detail</h2>
        <div class="table">
          ${report.cycles
            .map(
              (r) => `
            <div class="row">
              <strong>#${r.cycle_index}</strong>
              <span>blk ${r.block_number ?? "—"}</span>
              <span class="col-sim">${weiToEthDisplay(r.expected_profit_wei)}</span>
              <span class="col-live">${weiToEthDisplay(r.realized_profit_wei)}</span>
              <span class="col-diff">${weiToEthDisplay(r.profit_error_wei)}</span>
              <span>${esc(r.attribution)}</span>
            </div>`,
            )
            .join("")}
        </div>
        <p class="econ-note">Columns: expected (sim) · realized (fork) · error (realized − expected).</p>
      </section>

      <section class="panel">
        <h2>Limitations</h2>
        <ul class="attr-list">${report.limitations.map((l) => `<li>${esc(l)}</li>`).join("")}</ul>
      </section>
    </section>
  `;
}

function renderGroup(title, rows) {
  if (!rows?.length) {
    return `<section class="panel"><h2>${title}</h2><p class="empty">No comparable samples</p></section>`;
  }
  return `
    <section class="panel">
      <h2>${title}</h2>
      <div class="table">
        ${rows
          .map(
            (g) => `
          <div class="row">
            <strong>${esc(g.key)}</strong>
            <span>n=${g.n}</span>
            <span>|err| ${weiToEthDisplay(g.mean_abs_error_wei)}</span>
            <span>err ${weiToEthDisplay(g.mean_error_wei)}</span>
          </div>`,
          )
          .join("")}
      </div>
    </section>`;
}

function metric(title, value) {
  return `<article class="metric neutral"><span>${title}</span><strong>${value}</strong></article>`;
}

function esc(v) {
  return String(v ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll('"', "&quot;");
}