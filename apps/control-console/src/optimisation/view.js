// apps/control-console/src/optimisation/view.js
/**
 * Phase E — Optimisation Centre
 * Reproducible experiment records + baseline vs candidate report compare.
 * Does not execute simulation in-browser and never promotes parameters to live.
 */

import { buildFidelityReport } from "../shared/fidelity.js";
import { weiToEthDisplay, pct } from "../shared/format.js";

export function renderOptimisationCentre(state) {
  const experiments = state.experiments || [];
  const selectedId = state.experimentSelectedId;
  const selected = experiments.find((e) => e.id === selectedId) || null;
  const baseline = experiments.find((e) => e.id === state.experimentBaselineId) || null;
  const candidate = experiments.find((e) => e.id === state.experimentCandidateId) || null;

  return `
    <section class="optimisation">
      <header class="topbar">
        <div>
          <p class="eyebrow tag sim">OPTIMISATION CENTRE</p>
          <h1>Reproducible simulation experiments</h1>
        </div>
        <span class="pill warn">no auto-promote · no live broadcast</span>
      </header>

      <section class="panel">
        <h2>Run config (documentation + archive)</h2>
        <p class="econ-note">
          Execution stays in <code>omega-simulation</code> (CLI / runner). Record the exact config here,
          then paste the resulting <code>SimulationReport</code> JSON. Promotion to Live requires explicit
          control-plane authorization outside this UI.
        </p>
        <div class="opt-form">
          <label>Label <input id="expLabel" placeholder="e.g. LA-v3 fork 245M" /></label>
          <label>Cycles <input id="expCycles" type="number" min="1" value="50" /></label>
          <label>Fork block <input id="expForkBlock" placeholder="245000000" /></label>
          <label>Fork RPC (read-only) <input id="expForkRpc" placeholder="https://…" /></label>
          <label>Strategy version <input id="expStrategyVersion" placeholder="optional" /></label>
          <label>Notes <input id="expNotes" placeholder="dataset / hypothesis" /></label>
        </div>
        <p class="econ-note"><strong>CLI template</strong></p>
        <pre class="cli-template" id="expCliTemplate">export ARBITRUM_RPC_URL="&lt;read-only-rpc&gt;"
cargo run --example run_sim -p omega-simulation -- \\
  --cycles 50 \\
  --fork-block &lt;block&gt; \\
  --report out/sim_report.json</pre>
        <h3>Paste SimulationReport for this experiment</h3>
        <textarea id="expReportJson" rows="6" placeholder="Paste SimulationReport JSON after the CLI run"></textarea>
        <div class="button-grid">
          <button type="button" data-action="saveExperiment">Save experiment</button>
          <button type="button" data-action="clearExperimentForm">Clear form</button>
        </div>
        ${state.experimentError ? `<p class="errors"><span>${esc(state.experimentError)}</span></p>` : ""}
      </section>

      <section class="panel">
        <h2>Experiment archive (${experiments.length})</h2>
        ${
          experiments.length
            ? `<div class="table">${experiments
                .map(
                  (e) => `
              <div class="row exp-row ${e.id === selectedId ? "selected" : ""}">
                <button type="button" data-exp-select="${esc(e.id)}"><strong>${esc(e.label)}</strong></button>
                <span>${esc(e.config.cycles)} cycles</span>
                <span>block ${esc(e.config.fork_block || "—")}</span>
                <span>${esc(e.created_at)}</span>
                <button type="button" data-exp-baseline="${esc(e.id)}">Baseline</button>
                <button type="button" data-exp-candidate="${esc(e.id)}">Candidate</button>
                <button type="button" data-exp-delete="${esc(e.id)}">Delete</button>
              </div>`,
                )
                .join("")}</div>`
            : `<p class="empty">No experiments saved. Save config + report after each CLI run.</p>`
        }
      </section>

      ${selected ? renderExperimentDetail(selected) : ""}
      ${renderCompare(baseline, candidate)}
      ${renderPromotionGuard()}
    </section>
  `;
}

function renderExperimentDetail(exp) {
  const fid = exp.report ? buildFidelityReport(exp.report) : null;
  return `
    <section class="panel">
      <h2>Selected: ${esc(exp.label)}</h2>
      <div class="detail">
        <span>Id</span><strong>${esc(exp.id)}</strong>
        <span>Cycles</span><strong>${esc(exp.config.cycles)}</strong>
        <span>Fork block</span><strong>${esc(exp.config.fork_block || "—")}</strong>
        <span>Strategy ver</span><strong>${esc(exp.config.strategy_version || "—")}</strong>
        <span>Notes</span><strong>${esc(exp.config.notes || "—")}</strong>
        <span>Created</span><strong>${esc(exp.created_at)}</strong>
      </div>
      ${
        fid
          ? `<section class="metrics">
              ${metric("Attempted samples", fid.comparable_profit_samples)}
              ${metric("Mean |err|", weiToEthDisplay(fid.mean_abs_profit_error_wei))}
              ${metric("Mean err", weiToEthDisplay(fid.mean_profit_error_wei))}
            </section>`
          : `<p class="empty">No report attached</p>`
      }
    </section>`;
}

function renderCompare(baseline, candidate) {
  if (!baseline || !candidate) {
    return `
      <section class="panel">
        <h2>Baseline vs candidate</h2>
        <p class="empty">Mark two experiments as Baseline and Candidate to compare fidelity summaries.</p>
      </section>`;
  }
  if (baseline.id === candidate.id) {
    return `
      <section class="panel">
        <h2>Baseline vs candidate</h2>
        <p class="empty">Choose two different experiments.</p>
      </section>`;
  }

  const b = baseline.report ? buildFidelityReport(baseline.report) : null;
  const c = candidate.report ? buildFidelityReport(candidate.report) : null;

  return `
    <section class="panel">
      <h2>Baseline vs candidate</h2>
      <p class="econ-note">
        <span class="col-live">Baseline:</span> ${esc(baseline.label)} ·
        <span class="col-sim">Candidate:</span> ${esc(candidate.label)}
      </p>
      <table class="econ-table">
        <thead>
          <tr>
            <th>Metric</th>
            <th class="col-live">BASELINE</th>
            <th class="col-sim">CANDIDATE</th>
            <th class="col-diff">DELTA (cand − base)</th>
          </tr>
        </thead>
        <tbody>
          ${cmpRow("Comparable samples", b?.comparable_profit_samples, c?.comparable_profit_samples, false)}
          ${cmpRow("Mean abs profit error (wei)", b?.mean_abs_profit_error_wei, c?.mean_abs_profit_error_wei, true)}
          ${cmpRow("Mean profit error (wei)", b?.mean_profit_error_wei, c?.mean_profit_error_wei, true)}
        </tbody>
      </table>
      <p class="econ-warn">Lower mean absolute error is better for prediction fidelity. This is not automatic promotion criteria.</p>
    </section>`;
}

function cmpRow(label, base, cand, asWei) {
  const b = base ?? null;
  const c = cand ?? null;
  let delta = "—";
  if (b !== null && c !== null && asWei) {
    try {
      delta = (BigInt(String(c)) - BigInt(String(b))).toString();
      delta = weiToEthDisplay(delta);
    } catch {
      delta = "—";
    }
  } else if (typeof b === "number" && typeof c === "number") {
    delta = String(c - b);
  }
  return `
    <tr>
      <td>${esc(label)}</td>
      <td class="col-live">${asWei ? weiToEthDisplay(b) : b ?? "—"}</td>
      <td class="col-sim">${asWei ? weiToEthDisplay(c) : c ?? "—"}</td>
      <td class="col-diff">${delta}</td>
    </tr>`;
}

function renderPromotionGuard() {
  return `
    <section class="panel">
      <h2>Promotion to Live</h2>
      <p class="econ-warn">
        This centre <strong>never</strong> writes strategy parameters to the Live Engine.
        Deploying or enabling parameters in production requires explicit Live control-plane
        authorization (L2-style commands) and is out of scope for simulation UI.
      </p>
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