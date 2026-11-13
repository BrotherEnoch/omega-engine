// apps/control-console/src/app.js
import { refreshAll } from "./api.js";
import { connectRealtime } from "./realtime.js";
import { startRenderer, paintLive } from "./render.js";
import { mutate, store, subscribe } from "./state.js";
import { renderNav, currentNavId, isLiveRoute } from "./nav.js";
import { renderSimulationLab } from "./simulation/view.js";
import { renderEconomicsShell, cyclesToSimEconomics } from "./economics/view.js";
import { renderFidelityDashboard } from "./fidelity/view.js";
import { renderOptimisationCentre } from "./optimisation/view.js";

const app = document.querySelector("#app");
const EXPERIMENTS_KEY = "omega.sim.experiments";

let liveStarted = false;

function route() {
  if (isLiveRoute()) return "live";
  const path = location.hash.replace(/^#/, "").toLowerCase();
  if (path.startsWith("/simulation")) return "simulation";
  if (path.startsWith("/economics")) return "economics";
  if (path.startsWith("/fidelity")) return "fidelity";
  if (path.startsWith("/optimisation") || path.startsWith("/optimization")) return "optimisation";
  return "live";
}

function loadExperimentsFromStorage() {
  try {
    const raw = localStorage.getItem(EXPERIMENTS_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function persistExperiments(list) {
  localStorage.setItem(EXPERIMENTS_KEY, JSON.stringify(list));
}

export function boot() {
  mutate((s) => {
    if (s.simReport === undefined) s.simReport = null;
    if (s.simReportError === undefined) s.simReportError = "";
    if (!s.liveEconomics) s.liveEconomics = [];
    if (!s.econFilters) s.econFilters = { q: "", status: "" };
    if (s.econSelectedId === undefined) s.econSelectedId = null;
    if (!s.experiments) s.experiments = loadExperimentsFromStorage();
    if (s.experimentSelectedId === undefined) s.experimentSelectedId = null;
    if (s.experimentBaselineId === undefined) s.experimentBaselineId = null;
    if (s.experimentCandidateId === undefined) s.experimentCandidateId = null;
    if (s.experimentError === undefined) s.experimentError = "";
  });

  if (!location.hash || location.hash === "#") {
    location.hash = "#/overview";
  }

  window.addEventListener("hashchange", () => paint());
  subscribe(() => {
    if (route() !== "live") paint();
  });

  paint();
}

function ensureLive() {
  if (!liveStarted) {
    startRenderer();
    liveStarted = true;
    refreshAll().finally(() => {
      const base = store.apiBase || "";
      if (base && !base.includes(":5173") && !base.includes(":4173")) {
        connectRealtime();
      }
    });
  }
}

function paint() {
  const r = route();

  if (r === "live") {
    ensureLive();
    paintLive();
    return;
  }

  if (r === "simulation") {
    app.innerHTML = `${renderNav(currentNavId())}${renderSimulationLab(store)}`;
    bindSim();
    return;
  }

  if (r === "economics") {
    const simRecords = store.simReport ? cyclesToSimEconomics(store.simReport) : [];
    app.innerHTML =
      renderNav(currentNavId()) +
      renderEconomicsShell({
        liveRecords: store.liveEconomics || [],
        simRecords,
        selectedId: store.econSelectedId,
        filters: store.econFilters || {},
      });
    bindEcon();
    return;
  }

  if (r === "fidelity") {
    app.innerHTML = `${renderNav(currentNavId())}${renderFidelityDashboard(store)}`;
    return;
  }

  if (r === "optimisation") {
    app.innerHTML = `${renderNav(currentNavId())}${renderOptimisationCentre(store)}`;
    bindOptimisation();
  }
}

function bindSim() {
  app.querySelector('[data-action="loadSimReport"]')?.addEventListener("click", () => {
    const raw = app.querySelector("#simReportJson")?.value || "";
    try {
      const report = JSON.parse(raw);
      if (!Array.isArray(report.cycles)) throw new Error("Not a SimulationReport: missing cycles[]");
      mutate((s) => {
        s.simReport = report;
        s.simReportError = "";
      });
      paint();
    } catch (e) {
      mutate((s) => {
        s.simReportError = e.message;
      });
      paint();
    }
  });
  app.querySelector('[data-action="clearSimReport"]')?.addEventListener("click", () => {
    mutate((s) => {
      s.simReport = null;
      s.simReportError = "";
    });
    paint();
  });
}

function bindEcon() {
  app.querySelectorAll("[data-econ-id]").forEach((node) => {
    node.addEventListener("click", () => {
      mutate((s) => {
        s.econSelectedId = node.getAttribute("data-econ-id");
      });
      paint();
    });
  });
  app.querySelectorAll("[data-econ-filter]").forEach((node) => {
    const apply = () => {
      mutate((s) => {
        s.econFilters = {
          ...s.econFilters,
          [node.getAttribute("data-econ-filter")]: node.value,
        };
      });
      paint();
    };
    node.addEventListener("change", apply);
    if (node.tagName === "INPUT") {
      node.addEventListener("keydown", (ev) => {
        if (ev.key === "Enter") apply();
      });
    }
  });
}

function bindOptimisation() {
  app.querySelector('[data-action="saveExperiment"]')?.addEventListener("click", () => {
    const label = app.querySelector("#expLabel")?.value?.trim() || "";
    const cycles = Number(app.querySelector("#expCycles")?.value || 0);
    const fork_block = app.querySelector("#expForkBlock")?.value?.trim() || "";
    const fork_rpc = app.querySelector("#expForkRpc")?.value?.trim() || "";
    const strategy_version = app.querySelector("#expStrategyVersion")?.value?.trim() || "";
    const notes = app.querySelector("#expNotes")?.value?.trim() || "";
    const raw = app.querySelector("#expReportJson")?.value || "";

    try {
      if (!label) throw new Error("Label is required");
      if (!cycles || cycles < 1) throw new Error("Cycles must be >= 1");
      if (!raw.trim()) throw new Error("Paste a SimulationReport JSON to make the experiment reproducible");
      const report = JSON.parse(raw);
      if (!Array.isArray(report.cycles)) throw new Error("Not a SimulationReport: missing cycles[]");

      const experiment = {
        id: `exp_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`,
        label,
        created_at: new Date().toISOString(),
        config: {
          cycles,
          fork_block,
          fork_rpc,
          strategy_version,
          notes,
          sim_crate: "omega-simulation",
          example: "run_sim",
        },
        report,
      };

      mutate((s) => {
        s.experiments = [experiment, ...(s.experiments || [])];
        s.experimentSelectedId = experiment.id;
        s.experimentError = "";
        persistExperiments(s.experiments);
      });
      paint();
    } catch (e) {
      mutate((s) => {
        s.experimentError = e.message;
      });
      paint();
    }
  });

  app.querySelector('[data-action="clearExperimentForm"]')?.addEventListener("click", () => {
    ["expLabel", "expForkBlock", "expForkRpc", "expStrategyVersion", "expNotes", "expReportJson"].forEach(
      (id) => {
        const el = app.querySelector(`#${id}`);
        if (el) el.value = "";
      },
    );
    const cycles = app.querySelector("#expCycles");
    if (cycles) cycles.value = "50";
    mutate((s) => {
      s.experimentError = "";
    });
  });

  app.querySelectorAll("[data-exp-select]").forEach((node) => {
    node.addEventListener("click", () => {
      mutate((s) => {
        s.experimentSelectedId = node.getAttribute("data-exp-select");
      });
      paint();
    });
  });
  app.querySelectorAll("[data-exp-baseline]").forEach((node) => {
    node.addEventListener("click", () => {
      mutate((s) => {
        s.experimentBaselineId = node.getAttribute("data-exp-baseline");
      });
      paint();
    });
  });
  app.querySelectorAll("[data-exp-candidate]").forEach((node) => {
    node.addEventListener("click", () => {
      mutate((s) => {
        s.experimentCandidateId = node.getAttribute("data-exp-candidate");
      });
      paint();
    });
  });
  app.querySelectorAll("[data-exp-delete]").forEach((node) => {
    node.addEventListener("click", () => {
      const id = node.getAttribute("data-exp-delete");
      mutate((s) => {
        s.experiments = (s.experiments || []).filter((e) => e.id !== id);
        if (s.experimentSelectedId === id) s.experimentSelectedId = null;
        if (s.experimentBaselineId === id) s.experimentBaselineId = null;
        if (s.experimentCandidateId === id) s.experimentCandidateId = null;
        persistExperiments(s.experiments);
      });
      paint();
    });
  });
}

boot();