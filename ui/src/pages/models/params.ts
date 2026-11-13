/** The filter vocabulary the Registry view writes to the URL, and nothing else. */
export const ALL = "all";

/** Page sizes the registry offers; 50 is the default because it fills a laptop viewport. */
export const SIZES = [25, 50, 100, 200] as const;
export const DEFAULT_SIZE = 50;

/** The in-page views, addressable as `?view=`. `active` is the default. */
export const VIEWS = ["active", "registry", "names"] as const;
export type ModelView = (typeof VIEWS)[number];

/** A requested page size that is not offered falls back instead of shrinking the table. */
export function sizeOf(params: URLSearchParams): number {
  const requested = Number(params.get("size") ?? DEFAULT_SIZE);
  return (SIZES as readonly number[]).includes(requested) ? requested : DEFAULT_SIZE;
}

/** A page number clamped to what the current row count can show. */
export function pageOf(params: URLSearchParams, key: string, total: number, size: number): number {
  const last = Math.max(1, Math.ceil(total / size));
  return Math.min(Math.max(1, Number(params.get(key) ?? "1") || 1), last);
}
