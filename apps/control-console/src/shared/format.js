// apps/control-console/src/shared/format.js
export function money(value, digits = 4) {
  if (value === null || value === undefined || value === "") return "—";
  const n = Number(value);
  if (!Number.isFinite(n)) return "—";
  return n.toLocaleString(undefined, { maximumFractionDigits: digits });
}

export function weiToEthDisplay(weiLike) {
  if (weiLike === null || weiLike === undefined || weiLike === "") return "—";
  // U256 often serializes as decimal string
  try {
    const s = String(weiLike);
    if (!/^-?(\d+|0x[0-9a-fA-F]+)$/.test(s)) return String(weiLike);
    // avoid BigInt in old browsers if needed — Node/modern OK
    const bi = BigInt(s);
    const neg = bi < 0n;
    const abs = neg ? -bi : bi;
    const whole = abs / 10n ** 18n;
    const frac = abs % 10n ** 18n;
    const fracStr = frac.toString().padStart(18, "0").slice(0, 6);
    return `${neg ? "-" : ""}${whole}.${fracStr} ETH`;
  } catch {
    return String(weiLike);
  }
}

export function pct(value, digits = 2) {
  if (value === null || value === undefined || Number.isNaN(Number(value))) return "—";
  return `${(Number(value) * 100).toFixed(digits)}%`;
}

export function diffClass(delta) {
  if (delta === null || delta === undefined || Number.isNaN(Number(delta))) return "diff-na";
  const n = Number(delta);
  if (n > 0) return "diff-pos";
  if (n < 0) return "diff-neg";
  return "diff-zero";
}