// Parse report JSON without losing integer precision.
// Rust writes i128 profits as bare JSON numbers; JSON.parse would round anything above 2^53.
const BIG = /"(?:\\.|[^"\\])*"|(?<![\d.eE+-])-?\d{16,}(?![\d.eE])/g;

export function parseReportText(text) {
  const safe = String(text).replace(BIG, (m) => (m[0] === '"' ? m : '"' + m + '"'));
  return JSON.parse(safe);
}