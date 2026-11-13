// apps/control-console/src/economics/view.js
import { compareRecords, ECONOMICS_METRIC_KEYS, cyclesToSimEconomics, pairKey } from "../shared/compare.js";
import { diffClass, money, weiToEthDisplay } from "../shared/format.js";

export function renderEconomicsShell({ liveRecords, simRecords, selectedId, filters }) {
  const index = buildIndex(liveRecords, simRecords);
  const filtered = applyFilters(index, filters);
  const selected = filtered.find((r) => r.id === selectedId) || filtered[0] || null;
  const detail = selected
    ? compareRecords(selected.live, selected.sim, ECONOMICS_METRIC_KEYS)
    : null;

  return `
    <section class="economics">
      <header class="econ-head">
        <div>
          <p class="eyebrow">Transaction Economics</p>
          <h1>LIVE <span class="tag live">🟢</span> · SIM <span class="tag sim">🔵</span> · DIFF <span class="tag diff">⚫</span></h1>
        </div>
        <p class="econ-note">DIFF = LIVE − SIM. Unmatched rows are labelled. Missing ledgers show unavailable — no fabricated P&amp;L.</p>
      </header>

      <section class="econ-filters">
        <input data-econ-filter="q" placeholder="Search id / hash / strategy / pair" value="${escapeAttr(filters.q || "")}" />
        <select data-econ-filter="status">
          <option value="">All status</option>
          <option value="paired" ${filters.status === "paired" ? "selected" : ""}>Paired</option>
          <option value="unmatched_live" ${filters.status === "unmatched_live" ? "selected" : ""}>Unmatched LIVE</option>
          <option value="unmatched_sim" ${filters.status === "unmatched_sim" ? "selected" : ""}>Unmatched SIM</option>
        </select>
      </section>

      <div class="econ-layout">
        <div class="econ-list panel">
          <h2>Explorer (${filtered.length})</h2>
          ${filtered.length ? filtered.map((r) => listRow(r, selected?.id)).join("") : emptyExplorer(liveRecords, simRecords)}
        </div>
        <div class="econ-detail panel">
          ${selected ? renderDetail(selected, detail) : `<p class="empty">Select a transaction</p>`}
        </div>
      </div>
    </section>
  `;
}

function emptyExplorer(liveRecords, simRecords) {
  const liveN = liveRecords?.length || 0;
  const simN = simRecords?.length || 0;
  return `
    <p class="empty">
      No comparable transactions.
      Live ledger records: <strong>${liveN}</strong> (API not exposed yet → always 0 until backend).
      Simulation cycles loaded: <strong>${simN}</strong>.
      Load a SimulationReport on /simulation, or wait for live economics API.
    </p>`;
}

function listRow(r, selectedId) {
  const badge =
    r.status === "paired"
      ? "paired"
      : r.status === "unmatched_live"
        ? "unmatched-live"
        : "unmatched-sim";
  return `
    <button type="button" class="econ-row ${r.id === selectedId ? "selected" : ""}" data-econ-id="${escapeAttr(r.id)}">
      <span class="badge ${badge}">${r.status.replaceAll("_", " ")}</span>
      <strong>${escapeAttr(r.label)}</strong>
      <span>${escapeAttr(r.strategy || "—")}</span>
    </button>`;
}

function renderDetail(selected, detail) {
  return `
    <h2>${escapeAttr(selected.label)}</h2>
    <p class="meta">Status: <strong>${detail.status}</strong>
      ${detail.status !== "paired" ? " — comparison incomplete; missing side not invented." : ""}</p>
    <table class="econ-table">
      <thead>
        <tr>
          <th>Metric</th>
          <th class="col-live">LIVE 🟢</th>
          <th class="col-sim">SIMULATION 🔵</th>
          <th class="col-diff">DIFFERENCE ⚫</th>
        </tr>
      </thead>
      <tbody>
        ${detail.rows.map(renderMetricRow).join("")}
      </tbody>
    </table>
    ${selected.live?.accounting_complete === false || selected.sim?.accounting_complete === false
      ? `<p class="econ-warn">Line-item purchase/sale/cost fields are unavailable until the live ledger API and full sim accounting export exist. Only fields present on the record are shown.</p>`
      : ""}
  `;
}

function renderMetricRow(row) {
  return `
    <tr>
      <td>${escapeAttr(row.key)}</td>
      <td class="col-live">${fmtCell(row.live, row.key)}</td>
      <td class="col-sim">${fmtCell(row.simulation, row.key)}</td>
      <td class="col-diff ${diffClass(row.difference)}">${row.difference === null ? "—" : money(row.difference, 6)}</td>
    </tr>`;
}

function fmtCell(v, key) {
  if (v === null || v === undefined || v === "") return "—";
  if (String(key).includes("wei")) return weiToEthDisplay(v);
  return escapeAttr(String(v));
}

function buildIndex(liveRecords = [], simRecords = []) {
  const map = new Map();

  for (const live of liveRecords) {
    const k = pairKey(live) || `live-only:${live.tx_id || Math.random()}`;
    map.set(k, {
      id: k,
      status: "unmatched_live",
      label: live.tx_hash || live.tx_id || k,
      strategy: live.strategy_id,
      live,
      sim: null,
    });
  }

  for (const sim of simRecords) {
    const k = pairKey(sim) || `sim-only:${sim.tx_id || Math.random()}`;
    if (map.has(k) && map.get(k).live) {
      const row = map.get(k);
      row.sim = sim;
      row.status = "paired";
    } else {
      map.set(k, {
        id: k,
        status: "unmatched_sim",
        label: sim.tx_id || k,
        strategy: sim.strategy_id,
        live: null,
        sim,
      });
    }
  }

  return [...map.values()];
}

function applyFilters(rows, filters = {}) {
  let out = rows;
  if (filters.status) out = out.filter((r) => r.status === filters.status);
  if (filters.q) {
    const q = filters.q.toLowerCase();
    out = out.filter((r) => JSON.stringify(r).toLowerCase().includes(q));
  }
  return out;
}

function escapeAttr(v) {
  return String(v ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll('"', "&quot;")
    .replaceAll("<", "&lt;");
}

export { cyclesToSimEconomics };