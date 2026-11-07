import type { Model } from "./api";

/**
 * Newest-first ordering for model lists. Version numbers are read from the id
 * (`claude-sonnet-4-5` → [4, 5]), 8-digit date suffixes are compared separately so a
 * date cannot outrank a generation bump, and retired rows always sort after current
 * ones. Mirrors the server's `catalog::compare_newest` so both orders agree.
 */
export function recency(id: string): { version: number[]; date: number[] } {
  const version: number[] = [];
  const date: number[] = [];
  for (const group of id.match(/\d+/g) ?? []) {
    const value = Number(group);
    if (!Number.isFinite(value)) continue;
    if (value >= 19_000_000) date.push(value);
    else version.push(value);
  }
  return { version, date };
}

function compareNumbersDesc(a: number[], b: number[]): number {
  const width = Math.max(a.length, b.length);
  for (let i = 0; i < width; i += 1) {
    const left = a[i] ?? 0;
    const right = b[i] ?? 0;
    if (left !== right) return right - left;
  }
  return 0;
}

/** Current models before retired ones, then newest version, then id. */
export function compareModels(a: Model, b: Model): number {
  if (a.deprecated !== b.deprecated) return a.deprecated ? 1 : -1;
  const ra = recency(a.id);
  const rb = recency(b.id);
  return (
    compareNumbersDesc(ra.version, rb.version) ||
    compareNumbersDesc(ra.date, rb.date) ||
    a.id.localeCompare(b.id)
  );
}

/** Sort a copy by {@link compareModels}. */
export function sortModels<T extends Model>(models: T[]): T[] {
  return [...models].sort(compareModels);
}
