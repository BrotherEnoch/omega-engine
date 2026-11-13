// apps/control-console/src/render.js — Overview / Controls / Connection + full nav
import {
  clearHalt,
  refreshAll,
  reloadBuilderBlacklist,
  reloadConfig,
  revertCheckpoint,
  saveSettings,
  unpauseModel,
} from "./api.js";
import { connectRealtime } from "./realtime.js";
import { mutate, store, subscribe } from "./state.js";
import { renderNav, currentNavId, isLiveRoute } from "./nav.js";

const app = document.querySelector("#app");

function liveTabFromHash() {
  const id = currentNavId();
  if (id === "controls" || id === "connection") return id;
  return "overview";
}

let tab = liveTabFromHash();

export function startRenderer() {
  tab = liveTabFromHash();
  subscribe(render);
  render(store);
}

/** Re-paint Overview / Controls / Connection after hash changes */
export function paintLive() {
  if (!isLiveRoute()) return;
  tab = liveTabFromHash();
  render(store);
}

function render(state) {
  // Do not paint live shell over Simulation / Economics / Fidelity / Optimisation
  if (!isLiveRoute()) return;

  tab = liveTabFromHash();

  const prevApi = app.querySelector("#apiBase");
  const prevToken = app.querySelector("#token");
  const draftApi = prevApi ? prevApi.value : state.apiBase;
  const draftToken = prevToken ? prevToken.value : state.token;
  const activeId = document.activeElement?.id;
  const selStart = document.activeElement?.selectionStart;
  const selEnd = document.activeElement?.selectionEnd;

  const body =
    tab === "controls"
      ? renderControls(state)
      : tab === "connection"
        ? renderConnection(state, draftApi, draftToken)
        : renderOverview(state);

  app.innerHTML = `
    <main class="shell">
      <header class="topbar">
        <div>
          <p class="eyebrow">Omega Engine</p>
          <h1>Control Console</h1>
        </div>
        <div class="top-actions">
          <span class="pill ${state.mode === "live" ? "ok" : "warn"}">${esc(state.mode)}</span>
          <span class="pill ${connectionTone(state.connection)}">${esc(state.connection)}</span>
          <button class="icon-button" data-action="refresh" title="Refresh">↻</button>
        </div>
      </header>

      ${renderNav(currentNavId())}

      ${renderErrors(state.errors)}
      ${
        state.mode === "offline"
          ? `<p class="notice">Engine unreachable at ${esc(state.apiBase)}. No data is shown. Check that the control plane is running, and the address on the Connection tab.</p>`
          : ""
      }

      ${body}
    </main>
  `;

  bindEvents();

  if (activeId === "apiBase" || activeId === "token") {
    const el = app.querySelector(`#${activeId}`);
    if (el) {
      el.focus();
      if (typeof selStart === "number" && typeof selEnd === "number") {
        try {
          el.setSelectionRange(selStart, selEnd);
        } catch {
          /* password inputs may reject selection */
        }
      }
    }
  }
}

function renderOverview(state) {
  const health = summarizeHealth(state.health);
  return `
    <section class="metrics">
      ${metric("System", health.systemStatus, health.systemTone)}
      ${metric("Operational", `${health.operational}/${health.total}`, "ok")}
      ${metric("Degraded", health.degraded, health.degraded ? "warn" : "muted")}
      ${metric("Halted", health.halted, health.halted ? "bad" : "muted")}
      ${metric("DAO fee", formatDaoFee(state.daoFee), "neutral")}
      ${metric("Ceiling hits", state.ceiling?.consecutive_ceiling_hits ?? "—", state.ceiling?.paused ? "bad" : "neutral")}
    </section>

    <section class="panel health-panel">
      <div class="panel-head">
        <div>
          <h2>Layer health</h2>
          <p>${state.health ? `Generated ${formatTime(state.health.generated_at)}` : "Waiting for snapshot"}</p>
        </div>
      </div>
      <div class="layer-grid">
        ${(state.health?.layers || []).map(renderLayer).join("")}
      </div>
    </section>

    <section class="panel" style="margin-top:12px">
      <h2>Live events</h2>
      <div class="event-list">
        ${state.events.map(renderEvent).join("") || empty("No events yet. Open the Connection tab and press Stream.")}
      </div>
    </section>
  `;
}

function renderControls(state) {
  const layers = (state.health?.layers || []).map(layerId);
  return `
    <section class="lower-grid">
      <aside class="panel command-panel">
        <h2>Commands</h2>
        <p>These act on the running engine. Each one asks you to confirm first.</p>
        <div class="button-grid">
          <button type="button" data-action="reloadConfig">Reload config</button>
          <button type="button" data-action="unpauseModel">Unpause model</button>
          <button type="button" data-action="reloadBuilderBlacklist">Reload blacklist</button>
        </div>
        <div class="selected-layer">
          <p>Clear halt on layer</p>
          <select id="layerSelect">
            <option value="">Choose a layer</option>
            ${layers
              .map(
                (id) =>
                  `<option value="${escAttr(id)}" ${state.selectedLayer === id ? "selected" : ""}>${esc(label(id))}</option>`,
              )
              .join("")}
          </select>
          <button type="button" data-action="clearHalt" ${state.selectedLayer ? "" : "disabled"}>Clear halt</button>
        </div>
        ${renderDaoFee(state.daoFee)}
        ${renderBlacklist(state.blacklist)}
      </aside>

      <section class="panel">
        <h2>Model checkpoints</h2>
        <div class="table">
          ${(state.checkpoints || []).map(renderCheckpoint).join("") || empty("No checkpoint data. A valid token is probably needed.")}
        </div>
      </section>
    </section>
  `;
}

function renderConnection(state, draftApi, draftToken) {
  return `
    <section class="panel">
      <h2>Connection</h2>
      <p>Paste the API token, press Save, then Stream. The token is stored only in this browser.</p>
      <div class="settings" style="margin-top:12px">
        <label>API address
          <input id="apiBase" autocomplete="off" spellcheck="false" value="${escAttr(draftApi)}" />
        </label>
        <label>Token
          <input id="token" type="password" autocomplete="off" value="${escAttr(draftToken)}" />
        </label>
        <button type="button" data-action="saveSettings">Save</button>
        <button type="button" data-action="connectRealtime">Stream</button>
      </div>
      <div class="detail" style="margin-top:16px">
        <span>Mode</span><strong>${esc(state.mode)}</strong>
        <span>Stream</span><strong>${esc(state.connection)}</strong>
        <span>Token saved</span><strong>${state.token ? "yes" : "no"}</strong>
      </div>
    </section>
  `;
}

function confirmThen(message, fn) {
  return () => {
    if (window.confirm(message)) fn();
  };
}

function on(selector, handler) {
  app.querySelector(selector)?.addEventListener("click", handler);
}

function bindEvents() {
  on('[data-action="refresh"]', refreshAll);
  on('[data-action="dismissErrors"]', () => mutate((s) => (s.errors = [])));

  on('[data-action="saveSettings"]', () => {
    saveSettings({
      apiBase: app.querySelector("#apiBase").value.trim(),
      token: app.querySelector("#token").value.trim(),
    });
    refreshAll();
    connectRealtime();
  });
  on('[data-action="connectRealtime"]', connectRealtime);

  on('[data-action="reloadConfig"]', confirmThen("Reload config from disk on the running engine?", reloadConfig));
  on('[data-action="unpauseModel"]', confirmThen("Unpause the model on the running engine?", unpauseModel));
  on(
    '[data-action="reloadBuilderBlacklist"]',
    confirmThen("Reload the builder blacklist?", reloadBuilderBlacklist),
  );
  on(
    '[data-action="clearHalt"]',
    confirmThen(`Clear the halt on "${store.selectedLayer}"?`, () => clearHalt(store.selectedLayer)),
  );

  app.querySelector("#layerSelect")?.addEventListener("change", (e) => {
    mutate((s) => {
      s.selectedLayer = e.target.value || null;
    });
  });

  app.querySelectorAll("[data-layer]").forEach((node) => {
    node.addEventListener("click", () => {
      mutate((s) => {
        s.selectedLayer = node.dataset.layer;
      });
      location.hash = "#/controls";
    });
  });

  app.querySelectorAll("[data-revert]").forEach((node) => {
    const v = node.dataset.revert;
    node.addEventListener(
      "click",
      confirmThen(`Revert the model to checkpoint v${v}?`, () => revertCheckpoint(v)),
    );
  });
}

function layerId(layer) {
  return layer.layer_id || layer.layer || "";
}

function summarizeHealth(health) {
  const layers = health?.layers || [];
  const halted = layers.filter((l) => tone(l) === "halted").length;
  const degraded = layers.filter((l) => tone(l) === "degraded").length;
  const operational = layers.filter((l) => l.is_operational).length;
  return {
    halted,
    degraded,
    operational,
    total: layers.length || 16,
    systemStatus: halted ? "HALTED" : degraded ? "DEGRADED" : health ? "HEALTHY" : "UNKNOWN",
    systemTone: halted ? "bad" : degraded ? "warn" : health ? "ok" : "muted",
  };
}

function renderLayer(layer) {
  const id = layerId(layer);
  return `
    <button type="button" class="layer ${tone(layer)} ${store.selectedLayer === id ? "selected" : ""}" data-layer="${escAttr(id)}" title="Open in Controls">
      <span>${esc(label(id))}</span>
      <strong>${esc(layer.state)}</strong>
    </button>
  `;
}

function formatDaoFee(fee) {
  if (!fee) return "—";
  if (typeof fee.dao_fee_pct === "number") return `${fee.dao_fee_pct.toFixed(2)}%`;
  if (typeof fee.dao_fee_bps === "number") return `${(fee.dao_fee_bps / 100).toFixed(2)}%`;
  return "—";
}

function renderDaoFee(fee) {
  if (!fee) return "";
  return `
    <div class="detail">
      <span>DAO fee</span><strong>${esc(fee.dao_fee_bps ?? "—")} bps</strong>
      <span>Daily cap</span><strong>${number(fee.daily_cap_eth)} ETH</strong>
      <span>Confirmations</span><strong>${esc(fee.confirmation_depth)}</strong>
    </div>
  `;
}

function renderBlacklist(b) {
  if (!b) return "";
  return `
    <div class="detail">
      <span>Builder blacklist</span><strong>${esc(b.entry_count)} entries</strong>
      <span>Path</span><strong title="${escAttr(b.path)}">${esc(b.path)}</strong>
    </div>
  `;
}

function renderCheckpoint(c) {
  return `
    <div class="row">
      <strong>v${esc(c.version)}</strong>
      <span>${percent(c.win_rate)}</span>
      <span>${number(c.sample_count)} samples</span>
      <button type="button" data-revert="${escAttr(c.version)}">Revert</button>
    </div>
  `;
}

function renderEvent(event) {
  const title = event.kind || event.type || event.error || "event";
  const detail = event.message || event.layer || event.reason || event.received_at || "";
  return `<article class="event"><strong>${esc(title)}</strong><span>${esc(detail)}</span></article>`;
}

function renderErrors(errors) {
  if (!errors.length) return "";
  return `<section class="errors">
    ${errors.map((e) => `<p>${esc(e.message)}</p>`).join("")}
    <button type="button" class="dismiss" data-action="dismissErrors">Dismiss</button>
  </section>`;
}

function metric(title, value, toneName) {
  return `<article class="metric ${toneName}"><span>${esc(title)}</span><strong>${esc(value)}</strong></article>`;
}

function empty(text) {
  return `<p class="empty">${esc(text)}</p>`;
}

function connectionTone(c) {
  if (c === "authenticated" || c === "connected") return "ok";
  if (c === "connecting" || c === "anonymous" || c === "mock") return "warn";
  if (c === "lagged") return "bad";
  return "muted";
}

function label(v) {
  return String(v).replaceAll("_", " ");
}
function number(v) {
  return Number(v).toLocaleString();
}
function percent(v) {
  return `${(Number(v) * 100).toFixed(1)}%`;
}
function formatTime(v) {
  return new Date(v).toLocaleTimeString();
}
function esc(v) {
  return String(v ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}
function escAttr(v) {
  return esc(v).replaceAll('"', "&quot;");
}
function tone(layer) {
  const s = String(layer.state).toUpperCase();
  if (!layer.is_operational || /HALT|FAIL|DOWN|ERR|CRIT/.test(s)) return "halted";
  if (/^(OK|HEALTHY|UP|READY)$/.test(s)) return "healthy";
  return "degraded";
}