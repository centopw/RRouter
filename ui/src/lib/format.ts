/** Display helpers. Numbers are compact, times are local, nothing is faked. */

export function compact(n: number | null | undefined): string {
  if (n === null || n === undefined) return "—";
  const abs = Math.abs(n);
  if (abs >= 1_000_000_000) return `${scaled(n / 1_000_000_000)}B`;
  if (abs >= 1_000_000) return `${scaled(n / 1_000_000)}M`;
  if (abs >= 1_000) return `${scaled(n / 1_000)}K`;
  return String(n);
}

/** Two significant decimals, trailing zeros dropped: 1.20 -> "1.2", 12.0 -> "12". */
function scaled(n: number): string {
  const fixed = n >= 100 ? n.toFixed(0) : n >= 10 ? n.toFixed(1) : n.toFixed(2);
  return fixed.replace(/\.0+$/, "").replace(/(\.\d)0$/, "$1");
}

export function ms(n: number | null | undefined): string {
  if (n === null || n === undefined) return "—";
  return n >= 1000 ? `${(n / 1000).toFixed(2)}s` : `${Math.round(n)}ms`;
}

export function pct(n: number): string {
  return `${n.toFixed(1)}%`;
}

/** One USD is 1,000,000 micros: the smallest unit any provider charges. */
const MICROS = 1_000_000;

/**
 * Money from micros. Four decimals from a cent up, up to six below it so a
 * fraction of a cent stays visible instead of rounding to nothing; `dash` shows
 * zero or absent spend as an em dash.
 */
export function usd(micros: number | null | undefined, opts?: { dash?: boolean }): string {
  if (micros === null || micros === undefined || !Number.isFinite(micros) || micros === 0) {
    return opts?.dash ? "—" : "$0.0000";
  }
  const dollars = micros / MICROS;
  if (Math.abs(dollars) < 0.01) {
    return `$${dollars.toFixed(6).replace(/0+$/, "").replace(/\.$/, "")}`;
  }
  return `$${dollars.toFixed(4)}`;
}

/** Micros as an editable dollar string: `2500000` -> "2.5", `1` -> "0.000001". */
export function usdInput(micros: number): string {
  if (!(micros > 0)) return "";
  return (micros / MICROS)
    .toFixed(6)
    .replace(/0+$/, "")
    .replace(/\.$/, "");
}

/**
 * A dollar field's value in micros, or why it is not one: non-negative, at most
 * six decimals (one micro is the smallest unit), empty or zero meaning unlimited.
 */
export function parseUsd(value: string): { micros: number } | { problem: string } {
  const text = value.trim();
  if (text === "") return { micros: 0 };
  if (!/^\d*\.?\d*$/.test(text)) {
    return { problem: "enter a non-negative dollar amount, digits only" };
  }
  if (!/^(\d+(\.\d{1,6})?|\.\d{1,6})$/.test(text)) {
    return { problem: "enter a dollar amount with at most 6 decimals" };
  }
  const dollars = Number(text);
  if (!Number.isFinite(dollars)) return { problem: "not a dollar amount" };
  return { micros: Math.round(dollars * MICROS) };
}

export function when(ts: number): string {
  return new Date(ts * 1000).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function ago(ts: number): string {
  const secs = Math.max(0, Math.floor(Date.now() / 1000 - ts));
  if (secs < 60) return `${secs}s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h ago`;
  return `${Math.floor(secs / 86400)}d ago`;
}

export function uptime(secs: number): string {
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

/** Split a model reference into provider and id; both halves may be absent. */
export function splitRef(ref: string): { provider: string | null; id: string } {
  const slash = ref.indexOf("/");
  return slash === -1
    ? { provider: null, id: ref }
    : { provider: ref.slice(0, slash), id: ref.slice(slash + 1) };
}
