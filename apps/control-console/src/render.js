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

const app = document.querySelector("#app");

export function startRenderer() {
  subscribe(render);
}

function render(state) {
  const health = summarizeHealth(state.health);
  app.innerHTML = `
    <main class="shell">
      <header class="topbar">
        <div>
          <p class="eyebrow">Omega Engine</p>
          <h1>Control Console</h1>
        </div>
        <div class="top-actions">
          <span class="pill ${state.mode === "live" ? "ok" : "warn"}">${state.mode}</span>
          <span class="pill ${connectionTone(state.connection)}">${state.connection}</span>
          <button class="icon-button" data-action="refresh" title="Refresh">↻</button>
        </div>
      </header>

      <section class="settings">
        <label>
          API
          <input id="apiBase" value="${escapeAttr(state.apiBase)}" />
        </label>
        <label>
          Token
          <input id="token" type="password" value="${escapeAttr(state.token)}" />
        </label>
        <button data-action="saveSettings">Save</button>
        <button data-action="connectRealtime">Stream</button>
      </section>

      ${renderErrors(state.errors)}

      <section class="metrics">
        ${metric("System", health.systemStatus, health.systemTone)}
        ${metric("Operational", `${health.operational}/${health.total}`, "ok")}
        ${metric("Degraded", health.degraded, health.degraded ? "warn" : "muted")}
        ${metric("Halted", health.halted, health.halted ? "bad" : "muted")}
        ${metric("DAO Fee", state.daoFee ? `${state.daoFee.dao_fee_pct.toFixed(2)}%` : "—", "neutral")}
        ${metric("Ceiling Hits", state.ceiling?.consecutive_ceiling_hits ?? "—", state.ceiling?.paused ? "bad" : "neutral")}
      </section>

      <section class="workspace">
        <section class="panel health-panel">
          <div class="panel-head">
            <div>
              <h2>Layer Health</h2>
              <p>${state.health ? `Generated ${formatTime(state.health.generated_at)}` : "Waiting for snapshot"}</p>
            </div>
          </div>
          <div class="layer-grid">
            ${(state.health?.layers || []).map(renderLayer).join("")}
          </div>
        </section>

        <aside class="panel command-panel">
          <h2>Commands</h2>
          <div class="button-grid">
            <button data-action="reloadConfig">Reload Config</button>
            <button data-action="unpauseModel">Unpause Model</button>
            <button data-action="reloadBuilderBlacklist">Reload Blacklist</button>
          </div>
          <div class="selected-layer">
            <p>Selected layer</p>
            <strong>${state.selectedLayer || "None"}</strong>
            <button data-action="clearHalt" ${state.selectedLayer ? "" : "disabled"}>Clear Halt</button>
          </div>
          ${renderDaoFee(state.daoFee)}
          ${renderBlacklist(state.blacklist)}
        </aside>
      </section>

      <section class="lower-grid">
        <section class="panel">
          <h2>Checkpoints</h2>
          <div class="table">
            ${(state.checkpoints || []).map(renderCheckpoint).join("") || empty("No checkpoint metadata")}
          </div>
        </section>
        <section class="panel">
          <h2>Realtime Events</h2>
          <div class="event-list">
            ${state.events.map(renderEvent).join("") || empty("No events received")}
          </div>
        </section>
      </section>
    </main>
  `;

  bindEvents();
}

function bindEvents() {
  app.querySelector('[data-action="refresh"]')?.addEventListener("click", refreshAll);
  app.querySelector('[data-action="saveSettings"]')?.addEventListener("click", () => {
    saveSettings({
      apiBase: app.querySelector("#apiBase").value,
      token: app.querySelector("#token").value,
    });
  });
  app.querySelector('[data-action="connectRealtime"]')?.addEventListener("click", connectRealtime);
  app.querySelector('[data-action="reloadConfig"]')?.addEventListener("click", reloadConfig);
  app.querySelector('[data-action="unpauseModel"]')?.addEventListener("click", unpauseModel);
  app.querySelector('[data-action="reloadBuilderBlacklist"]')?.addEventListener("click", reloadBuilderBlacklist);
  app.querySelector('[data-action="clearHalt"]')?.addEventListener("click", () => clearHalt(store.selectedLayer));

  app.querySelectorAll("[data-layer]").forEach((node) => {
    node.addEventListener("click", () => {
      mutate((state) => {
        state.selectedLayer = node.dataset.layer;
      });
    });
  });

  app.querySelectorAll("[data-revert]").forEach((node) => {
    node.addEventListener("click", () => revertCheckpoint(node.dataset.revert));
  });
}

function summarizeHealth(health) {
  const layers = health?.layers || [];
  const halted = layers.filter((layer) => layer.state === "HALTED").length;
  const degraded = layers.filter((layer) => layer.state === "DEGRADED").length;
  const operational = layers.filter((layer) => layer.is_operational).length;
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
  return `
    <button class="layer ${layer.state.toLowerCase()} ${store.selectedLayer === layer.layer ? "selected" : ""}" data-layer="${escapeAttr(layer.layer)}">
      <span>${label(layer.layer)}</span>
      <strong>${layer.state}</strong>
    </button>
  `;
}

function renderDaoFee(fee) {
  if (!fee) return "";
  return `
    <div class="detail">
      <span>DAO fee</span><strong>${fee.dao_fee_bps} bps</strong>
      <span>Daily cap</span><strong>${number(fee.daily_cap_eth)} ETH</strong>
      <span>Confirmations</span><strong>${fee.confirmation_depth}</strong>
    </div>
  `;
}

function renderBlacklist(blacklist) {
  if (!blacklist) return "";
  return `
    <div class="detail">
      <span>Builder blacklist</span><strong>${blacklist.entry_count} entries</strong>
      <span>Path</span><strong title="${escapeAttr(blacklist.path)}">${blacklist.path}</strong>
    </div>
  `;
}

function renderCheckpoint(checkpoint) {
  return `
    <div class="row">
      <strong>v${checkpoint.version}</strong>
      <span>${percent(checkpoint.win_rate)}</span>
      <span>${number(checkpoint.sample_count)} samples</span>
      <button data-revert="${checkpoint.version}">Revert</button>
    </div>
  `;
}

function renderEvent(event) {
  const title = event.kind || event.type || event.error || "event";
  const detail = event.message || event.layer || event.reason || event.received_at || "";
  return `
    <article class="event">
      <strong>${title}</strong>
      <span>${detail}</span>
    </article>
  `;
}

function renderErrors(errors) {
  if (!errors.length) return "";
  return `<section class="errors">${errors.map((error) => `<p>${error.message}</p>`).join("")}</section>`;
}

function metric(title, value, tone) {
  return `<article class="metric ${tone}"><span>${title}</span><strong>${value}</strong></article>`;
}

function empty(text) {
  return `<p class="empty">${text}</p>`;
}

function connectionTone(connection) {
  if (connection === "authenticated" || connection === "connected") return "ok";
  if (connection === "connecting" || connection === "anonymous" || connection === "mock") return "warn";
  if (connection === "lagged") return "bad";
  return "muted";
}

function label(value) {
  return value.replaceAll("_", " ");
}

function number(value) {
  return Number(value).toLocaleString();
}

function percent(value) {
  return `${(Number(value) * 100).toFixed(1)}%`;
}

function formatTime(value) {
  return new Date(value).toLocaleTimeString();
}

function escapeAttr(value) {
  return String(value ?? "").replaceAll("&", "&amp;").replaceAll('"', "&quot;").replaceAll("<", "&lt;");
}
