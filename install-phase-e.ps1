# install-phase-e.ps1
& {
  $d = "C:\Users\silve\Documents\omega-engine\apps\control-console\src"
  $u8 = New-Object System.Text.UTF8Encoding($false)
  function W($rel, $text) {
    $p = Join-Path $d $rel
    New-Item -ItemType Directory -Force -Path (Split-Path $p) | Out-Null
    [IO.File]::WriteAllText($p, $text, $u8)
    "wrote  $rel"
  }
  $app = Join-Path $d "app.js"
  if (-not (Test-Path $app)) { "STOP: app.js not found"; return }
  $t = [IO.File]::ReadAllText($app)
  if ($t -match 'experiments') { "STOP: app.js already mentions experiments (already installed?)"; return }
  $need = @(
    'import { renderEconomicsShell, cyclesToSimEconomics } from "./economics/view.js";',
    'ECONOMICS</a>',
    'if (path.startsWith("/economics")) return "economics";',
    'if (r === "economics") {'
  )
  foreach ($n in $need) { if ($t.IndexOf($n) -lt 0) { "STOP: app.js is missing expected text: $n"; return } }

W 'shared\bigjson.js' @'
// Parse report JSON without losing integer precision.
// Rust writes i128 profits as bare JSON numbers; JSON.parse would round anything above 2^53.
const BIG = /"(?:\\.|[^"\\])*"|(?<![\d.eE+-])-?\d{16,}(?![\d.eE])/g;

export function parseReportText(text) {
  const safe = String(text).replace(BIG, (m) => (m[0] === '"' ? m : '"' + m + '"'));
  return JSON.parse(safe);
}
'@

W 'experiments\metrics.js' @'
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
'@

W 'experiments\store.js' @'
// Experiment records live in this browser only. No URLs, keys or secrets are stored.
const KEY = "omega.sim.experiments.v1";

export function loadAll() {
  try {
    const raw = localStorage.getItem(KEY);
    const v = raw ? JSON.parse(raw) : [];
    return Array.isArray(v) ? v : [];
  } catch {
    return [];
  }
}

export function saveAll(list) {
  try {
    localStorage.setItem(KEY, JSON.stringify(list));
    return true;
  } catch {
    return false;
  }
}

export function newId() {
  return "exp-" + Date.now().toString(36) + "-" + Math.random().toString(36).slice(2, 6);
}

export async function sha256Hex(text) {
  if (!(globalThis.crypto && globalThis.crypto.subtle)) return null;
  const buf = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export function commandText(exp, role) {
  const c = exp.config;
  const block = c.fork_block !== null && c.fork_block !== undefined ? " --fork-block " + c.fork_block : "";
  return [
    '$env:ARBITRUM_RPC_URL = "<read-only Arbitrum RPC URL>"   # set in your shell, never stored here',
    "cargo run --example run_sim -p omega-simulation -- --cycles " + c.cycles + block +
      " --signer-index " + c.signer_index + " --report out/" + exp.id + "_" + role + ".json",
  ].join("\n");
}
'@

W 'experiments\view.js' @'
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
'@

  # ---- patch app.js (backup first) ----
  Copy-Item $app "$app.bak-e" -Force
  $t = $t.Replace('import { renderEconomicsShell, cyclesToSimEconomics } from "./economics/view.js";',
    ('import { renderEconomicsShell, cyclesToSimEconomics } from "./economics/view.js";' + "`n" +
     'import { mountExperiments } from "./experiments/view.js";' + "`n" +
     'import { parseReportText } from "./shared/bigjson.js";'))
  $t = $t.Replace('ECONOMICS</a>',
    ('ECONOMICS</a>' + "`n      " + '<a href="#/experiments" class="${active === "experiments" ? "active sim" : ""}">EXPERIMENTS</a>'))
  $t = $t.Replace('if (path.startsWith("/economics")) return "economics";',
    ('if (path.startsWith("/economics")) return "economics";' + "`n  " + 'if (path.startsWith("/experiments")) return "experiments";'))
  $branch = @'
if (r === "experiments") {
    if (!app.querySelector(".exp-root")) {
      app.innerHTML = nav("experiments") + '<div class="exp-mount"></div>';
    }
    mountExperiments(app.querySelector(".exp-mount"));
    return;
  }

  if (r === "economics") {
'@
  $t = $t.Replace('if (r === "economics") {', $branch.TrimEnd("`r","`n"))
  if ($t.Contains('const report = JSON.parse(raw);')) {
    $t = $t.Replace('const report = JSON.parse(raw);', 'const report = parseReportText(raw);')
    "patched simulation page to use the precision-safe parser"
  } else { "NOTE: JSON.parse(raw) not found in app.js - simulation page parser NOT switched" }
  [IO.File]::WriteAllText($app, $t, $u8)
  "patched app.js (backup: app.js.bak-e)"

  # ---- optional: hex wei in shared\format.js ----
  $ff = Join-Path $d "shared\format.js"
  if (Test-Path $ff) {
    $f = [IO.File]::ReadAllText($ff)
    $o = 'if (!/^-?\d+$/.test(s)) return String(weiLike);'
    if ($f.Contains($o)) {
      Copy-Item $ff "$ff.bak-e" -Force
      [IO.File]::WriteAllText($ff, $f.Replace($o, 'if (!/^-?(\d+|0x[0-9a-fA-F]+)$/.test(s)) return String(weiLike);'), $u8)
      "patched shared\format.js (hex U256 values now convert to ETH)"
    } else { "NOTE: expected line not found in shared\format.js - not patched" }
  }

  # ---- CSS ----
  $css = Join-Path $d "styles.css"
  if (-not (Select-String -Path $css -Pattern '\.exp-layout' -Quiet)) {
    Add-Content $css @'

/* ---- optimisation centre ---- */
.exp-layout { display: grid; grid-template-columns: 340px minmax(0, 1fr); gap: 12px; align-items: start; }
.exp-layout > div { display: grid; gap: 12px; }
.exp-form { display: grid; gap: 10px; }
.exp-form label { display: grid; gap: 4px; color: #aeb9be; font-size: 12px; text-transform: uppercase; }
.exp-form textarea, .exp-root textarea { width: 100%; border: 1px solid #334149; border-radius: 6px; padding: 8px 10px; background: #0e1418; color: #f6fbfd; font: inherit; }
pre.cmd { margin: 8px 0; padding: 10px; overflow-x: auto; border: 1px solid #25323a; border-radius: 6px; background: #0b1114; color: #cfe3ea; font-size: 12px; }
.col-cand { color: #bfdbfe; }
.exp-reasons { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
.exp-root .attr-list { margin: 6px 0; padding-left: 18px; }
@media (max-width: 1040px) { .exp-layout, .exp-reasons { grid-template-columns: 1fr; } }
'@
    "appended CSS"
  } else { "CSS already present" }
  "done"
}