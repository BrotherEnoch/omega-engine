import { parseReportText } from "../shared/bigjson.js";
import { compareSummaries, comparisonWarnings, summarizeReport, toBig } from "./metrics.js";
import { commandText, loadAll, newId, saveAll, sha256Hex } from "./store.js";

const ui = { selectedId: null, error: "", draft: null };
let root = null;

export function mountExperiments(container) {
  if (!container || container.querySelector(".exp-root")) return;
  root = container;
  container.onclick = onClick;
  container.onchange = onChange;
  draw();
}

/* ---------- actions ---------- */

const FIELDS = ["expName", "expHyp", "expBlock", "expCycles", "expCommit", "expSimVer", "expDataset", "expOverrides"];

function fail(message) {
  ui.draft = {};
  for (const f of FIELDS) {
    const el = root.querySelector("#" + f);
    if (el) ui.draft[f] = el.value;
  }
  ui.error = message;
  draw();
}

function create() {
  const v = (id) => ((root.querySelector("#" + id) || {}).value || "").trim();
  const name = v("expName");
  const cycles = Number(v("expCycles") || 50);
  const forkRaw = v("expBlock");
  const fork = forkRaw === "" ? null : Number(forkRaw);
  const texts = [name, v("expHyp"), v("expDataset"), v("expOverrides"), v("expSimVer")].join("\n");
  if (!name) return fail("Give the experiment a name.");
  if (!Number.isInteger(cycles) || cycles < 1 || cycles > 100000) return fail("Cycles must be a whole number from 1 to 100000.");
  if (fork !== null && (!Number.isInteger(fork) || fork < 0)) return fail("Fork block must be a whole number, or empty.");
  if (/:\/\//.test(texts) || /0x[0-9a-fA-F]{64}/.test(texts)) return fail("Do not put URLs or keys in an experiment record.");
  const list = loadAll();
  const exp = {
    schema: 1,
    id: newId(),
    created_at: new Date().toISOString(),
    name,
    hypothesis: v("expHyp"),
    config: { chain: "Arbitrum One (42161)", fork_block: fork, cycles, signer_index: 0, overrides_note: v("expOverrides") },
    versions: { strategy_commit: v("expCommit"), simulation_crate: v("expSimVer") },
    dataset_note: v("expDataset"),
    runs: { baseline: null, candidate: null },
    promotion: "not requested; not available from this console",
  };
  list.unshift(exp);
  if (!saveAll(list)) return fail("Could not save to browser storage.");
  ui.selectedId = exp.id;
  ui.error = "";
  ui.draft = null;
  draw();
}

async function attach(role, file) {
  if (!file) return;
  const list = loadAll();
  const exp = list.find((x) => x.id === ui.selectedId);
  if (!exp) return;
  try {
    if (file.size > 50 * 1024 * 1024) throw new Error("File is larger than 50 MB.");
    const text = await file.text();
    const report = parseReportText(text);
    if (!Array.isArray(report.cycles)) throw new Error("Not a SimulationReport: cycles[] is missing.");
    exp.runs[role] = {
      file_name: file.name,
      sha256: await sha256Hex(text),
      attached_at: new Date().toISOString(),
      started_at: report.started_at || null,
      finished_at: report.finished_at || null,
      summary: summarizeReport(report),
    };
    if (!saveAll(list)) throw new Error("Could not save to browser storage.");
    ui.error = "";
  } catch (err) {
    ui.error = err.message;
  }
  draw();
}

function exportRecord(exp) {
  const blob = new Blob([JSON.stringify(exp, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = exp.id + ".json";
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function copyText(text) {
  const fallback = () => { ui.error = "Clipboard unavailable. Select the command and copy it by hand."; draw(); };
  if (!(navigator.clipboard && navigator.clipboard.writeText)) return fallback();
  navigator.clipboard.writeText(text).catch(fallback);
}

function onClick(e) {
  const el = e.target.closest("[data-act]");
  if (!el || !root.contains(el)) return;
  const act = el.dataset.act;
  if (act === "clearError") { ui.error = ""; return draw(); }
  if (act === "select") { ui.selectedId = el.dataset.id; return draw(); }
  if (act === "create") return create();
  const list = loadAll();
  const exp = list.find((x) => x.id === ui.selectedId);
  if (!exp) return;
  if (act === "export") return exportRecord(exp);
  if (act === "copy") return copyText(commandText(exp, el.dataset.role));
  if (act === "delete") {
    if (!window.confirm("Delete this experiment record from this browser? Report files on disk are not touched.")) return;
    saveAll(list.filter((x) => x.id !== exp.id));
    ui.selectedId = null;
    draw();
  }
}

function onChange(e) {
  const t = e.target;
  if (t && t.matches && t.matches("[data-attach]")) attach(t.dataset.attach, t.files && t.files[0]);
}

/* ---------- rendering ---------- */

function draw() {
  const list = loadAll();
  if (!ui.selectedId && list.length) ui.selectedId = list[0].id;
  const exp = list.find((x) => x.id === ui.selectedId) || null;
  root.innerHTML = `
    <section class="exp-root">
      <header class="topbar">
        <div>
          <p class="eyebrow tag sim">OPTIMISATION CENTRE</p>
          <h1>Experiments</h1>
        </div>
        <span class="pill warn">simulation only &middot; never promotes to live</span>
      </header>
      ${ui.error ? `<section class="errors"><p>${esc(ui.error)}</p><button type="button" class="dismiss" data-act="clearError">Dismiss</button></section>` : ""}
      <section class="panel">
        <h2>What this page can and cannot do today</h2>
        <ul class="attr-list">
          <li>The run_sim example always uses default strategy settings (OmegaConfig::default()). Parameter overrides recorded here are documentation until the runner accepts a config file.</li>
          <li>Reports do not record fork block, config hash, strategy version or commit. They are kept in the experiment record and cannot be verified from the report.</li>
          <li>One run each is one data point. No winner is declared, and nothing here can change the Live Engine.</li>
        </ul>
      </section>
      <div class="exp-layout">
        <div class="panel">
          <h2>Experiments (${list.length})</h2>
          ${list.map((x) => `<button type="button" class="econ-row ${x.id === ui.selectedId ? "selected" : ""}" data-act="select" data-id="${escAttr(x.id)}"><strong>${esc(x.name)}</strong><span>${esc(statusOf(x))}</span></button>`).join("") || `<p class="empty">No experiments yet.</p>`}
          ${formHtml()}
        </div>
        <div>${exp ? detail(exp) : `<section class="panel"><p class="empty">Create or select an experiment.</p></section>`}</div>
      </div>
    </section>`;
}

function statusOf(x) {
  const b = !!x.runs.baseline, c = !!x.runs.candidate;
  return b && c ? "baseline + candidate loaded" : b ? "baseline loaded" : c ? "candidate loaded" : "no reports yet";
}

function formHtml() {
  const d = ui.draft || {};
  const val = (k, dflt) => escAttr(d[k] !== undefined ? d[k] : (dflt || ""));
  return `
    <h2 style="margin-top:16px">New experiment</h2>
    <div class="exp-form">
      <label>Name<input id="expName" value="${val("expName")}" /></label>
      <label>Hypothesis<textarea id="expHyp" rows="2">${esc(d.expHyp || "")}</textarea></label>
      <label>Fork block (optional)<input id="expBlock" inputmode="numeric" value="${val("expBlock")}" /></label>
      <label>Cycles<input id="expCycles" inputmode="numeric" value="${val("expCycles", "50")}" /></label>
      <label>Strategy version / git commit<input id="expCommit" value="${val("expCommit")}" /></label>
      <label>Simulation crate version<input id="expSimVer" value="${val("expSimVer", "omega-simulation 0.1.0")}" /></label>
      <label>Dataset note<textarea id="expDataset" rows="2" placeholder="Which market period / fork block and why">${esc(d.expDataset || "")}</textarea></label>
      <label>Parameter overrides (note only)<textarea id="expOverrides" rows="2" placeholder="Documentation only. Not applied by run_sim today.">${esc(d.expOverrides || "")}</textarea></label>
      <button type="button" data-act="create">Create experiment</button>
    </div>`;
}

function detail(exp) {
  const b = exp.runs.baseline, c = exp.runs.candidate;
  const cmp = b && c ? compareSummaries(b.summary, c.summary) : null;
  const warns = b && c ? comparisonWarnings(b.summary, c.summary) : [];
  if (exp.config.overrides_note) {
    warns.unshift("Parameter overrides are recorded but run_sim does not apply them yet. Unless the runner was changed, baseline and candidate used the same default settings.");
  }
  return `
    <section class="panel">
      <h2>${esc(exp.name)}</h2>
      <div class="detail">
        <span>Experiment ID</span><strong>${esc(exp.id)}</strong>
        <span>Created</span><strong>${esc(exp.created_at)}</strong>
        <span>Hypothesis</span><strong style="white-space:normal">${esc(exp.hypothesis || "\u2014")}</strong>
        <span>Chain</span><strong>${esc(exp.config.chain)}</strong>
        <span>Fork block</span><strong>${exp.config.fork_block === null ? "not set (latest)" : esc(exp.config.fork_block)}</strong>
        <span>Cycles</span><strong>${esc(exp.config.cycles)}</strong>
        <span>Strategy version</span><strong>${esc(exp.versions.strategy_commit || "not recorded")}</strong>
        <span>Simulation version</span><strong>${esc(exp.versions.simulation_crate || "not recorded")}</strong>
        <span>Dataset</span><strong style="white-space:normal">${esc(exp.dataset_note || "not recorded")}</strong>
        <span>Overrides (note)</span><strong style="white-space:normal">${esc(exp.config.overrides_note || "none")}</strong>
      </div>
    </section>
    ${runCard("baseline", b, exp)}
    ${runCard("candidate", c, exp)}
    ${warns.length ? `<section class="panel"><h2>Read before trusting this comparison</h2><ul class="attr-list">${warns.map((w) => `<li>${esc(w)}</li>`).join("")}</ul></section>` : ""}
    ${cmp ? compareTable(cmp, b, c) : `<section class="panel"><p class="empty">Attach both reports to compare them.</p></section>`}
    <section class="panel">
      <h2>Promotion to live</h2>
      <p class="econ-note">Not available from this console. Nothing here is ever sent to the Live Engine. Promotion needs a separate, explicitly authorised change on the live side, after repeated runs across different fork blocks. Export the record as evidence for that review.</p>
      <div class="button-grid">
        <button type="button" data-act="export">Export experiment record (JSON)</button>
        <button type="button" data-act="delete">Delete record</button>
      </div>
    </section>`;
}

function runCard(role, run, exp) {
  const title = role === "baseline" ? "Baseline run" : "Candidate run";
  return `
    <section class="panel">
      <h2>${title}</h2>
      <p class="econ-note">Command that produces the report:</p>
      <pre class="cmd">${esc(commandText(exp, role))}</pre>
      <div class="button-grid"><button type="button" data-act="copy" data-role="${role}">Copy command</button></div>
      <p class="econ-note">Then load the JSON file it writes:</p>
      <input type="file" accept=".json,application/json" data-attach="${role}" />
      ${run ? runSummary(run) : `<p class="empty">No report attached.</p>`}
    </section>`;
}

function runSummary(run) {
  const s = run.summary;
  return `
    <div class="detail" style="margin-top:10px">
      <span>File</span><strong title="${escAttr(run.file_name)}">${esc(run.file_name)}</strong>
      <span>SHA-256</span><strong title="${escAttr(run.sha256 || "")}">${esc((run.sha256 || "unavailable").slice(0, 16))}</strong>
      <span>Attached</span><strong>${esc(run.attached_at)}</strong>
      <span>Cycles / attempted</span><strong>${s.cycles} / ${s.attempted}</strong>
      <span>Net realized</span><strong>${fmtEth(s.net_profit_wei)}</strong>
    </div>`;
}

function compareTable(cmp, b, c) {
  const reasons = (s) => (s.summary.top_failure_reasons.length
    ? s.summary.top_failure_reasons.map((r) => `<li>${esc(r.reason)} (${r.count})</li>`).join("")
    : "<li>none recorded</li>");
  return `
    <section class="panel">
      <h2>Baseline vs candidate</h2>
      <table class="econ-table">
        <thead><tr><th>Metric</th><th class="col-sim">BASELINE</th><th class="col-cand">CANDIDATE</th><th class="col-diff">CANDIDATE &minus; BASELINE</th></tr></thead>
        <tbody>${cmp.map((r) => `<tr><td>${esc(r.label)}</td><td class="col-sim">${cell(r.kind, r.base)}</td><td class="col-cand">${cell(r.kind, r.cand)}</td><td class="col-diff">${deltaCell(r.kind, r.delta)}</td></tr>`).join("")}</tbody>
      </table>
      <p class="econ-note">No winner is declared. Read net profit, worst cycle, drawdown, failures and gas together, not success rate alone.</p>
      <h2 style="margin-top:14px">Most common failure reasons</h2>
      <div class="exp-reasons">
        <div><strong class="col-sim">Baseline</strong><ul class="attr-list">${reasons(b)}</ul></div>
        <div><strong class="col-cand">Candidate</strong><ul class="attr-list">${reasons(c)}</ul></div>
      </div>
    </section>`;
}

function fmtEth(v) {
  const b = toBig(v);
  if (b === null) return "&mdash;";
  const neg = b < 0n;
  const a = neg ? -b : b;
  const whole = a / 10n ** 18n;
  const frac = (a % 10n ** 18n).toString().padStart(18, "0").slice(0, 6);
  return (neg ? "-" : "") + whole + "." + frac + " ETH";
}

function cell(kind, v) {
  if (v === null || v === undefined) return "&mdash;";
  if (kind === "wei") return fmtEth(v);
  if (kind === "pct") return (Number(v) * 100).toFixed(1) + "%";
  if (kind === "ratio") return Number(v).toFixed(4);
  return esc(String(v));
}

function deltaCell(kind, d) {
  if (d === null || d === undefined) return "&mdash;";
  const big = kind === "wei" || kind === "count";
  const n = big ? toBig(d) : Number(d);
  const zero = big ? n === 0n : n === 0;
  const pos = big ? n > 0n : n > 0;
  const mark = zero ? "=" : pos ? "&#9650;" : "&#9660;";
  const body = kind === "wei" ? fmtEth(d) : kind === "pct" ? (n * 100).toFixed(1) + " pts" : kind === "ratio" ? n.toFixed(4) : String(d);
  return mark + " " + (pos ? "+" : "") + body;
}

function esc(v) {
  return String(v === null || v === undefined ? "" : v).replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
}
function escAttr(v) {
  return esc(v).replaceAll('"', "&quot;");
}