// apps/control-console/src/nav.js
/** Unified console navigation — uppercase labels, hash routes */

export const NAV_ITEMS = [
  { id: "overview", href: "#/overview", label: "OVERVIEW" },
  { id: "controls", href: "#/controls", label: "CONTROLS" },
  { id: "connection", href: "#/connection", label: "CONNECTION" },
  { id: "simulation", href: "#/simulation", label: "SIMULATION" },
  { id: "economics", href: "#/economics", label: "ECONOMICS" },
  { id: "fidelity", href: "#/fidelity", label: "FIDELITY" },
  { id: "optimisation", href: "#/optimisation", label: "OPTIMIZATION" },
];

export function currentNavId() {
  const path = (location.hash.replace(/^#/, "") || "/overview").toLowerCase();
  if (path.startsWith("/controls") || path === "controls") return "controls";
  if (path.startsWith("/connection") || path === "connection") return "connection";
  if (path.startsWith("/simulation")) return "simulation";
  if (path.startsWith("/economics")) return "economics";
  if (path.startsWith("/fidelity")) return "fidelity";
  if (path.startsWith("/optimisation") || path.startsWith("/optimization")) return "optimisation";
  if (path.startsWith("/live")) return "overview";
  if (path.startsWith("/overview") || path === "overview" || path === "/" || path === "") return "overview";
  return "overview";
}

/** Live control-console screens owned by render.js */
export function isLiveRoute() {
  const id = currentNavId();
  return id === "overview" || id === "controls" || id === "connection";
}

export function renderNav(activeId = currentNavId()) {
  return `
    <nav class="console-nav tabs" role="tablist" aria-label="Console sections">
      ${NAV_ITEMS.map(
        (item) => `
        <a href="${item.href}"
           class="console-nav-item tab${item.id === activeId ? " active" : ""}"
           role="tab"
           aria-selected="${item.id === activeId ? "true" : "false"}"
           data-nav="${item.id}">${item.label}</a>`,
      ).join("")}
    </nav>`;
}