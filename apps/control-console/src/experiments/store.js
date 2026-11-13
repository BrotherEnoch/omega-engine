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