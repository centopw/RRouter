import type { ModelCost } from "../../lib/api";

/**
 * The one price vocabulary of the Models page. A table row, a phone card, a combo member
 * and a combo's candidate range all answer "what does it cost" with the same words, so the
 * unit is always visible (`/1M`) and never lives only in a tooltip.
 *
 * Three states, never confused: `Free` (a catalog price of exactly zero), a dollar price,
 * and `price —` (no catalog covers the model). An unknown price is never shown as `$0`.
 */

/** Zero on every published dimension. A missing cost is unknown, not free. */
export function isFree(cost: ModelCost | null | undefined): boolean {
  if (!cost) return false;
  return (
    cost.input === 0 &&
    cost.output === 0 &&
    (cost.cache_read ?? 0) === 0 &&
    (cost.cache_write ?? 0) === 0
  );
}

/**
 * USD per 1M tokens: two decimals minimum, up to four while they carry information, so
 * `$0.075` stays `$0.075` instead of rounding to a different price.
 */
export function dollars(value: number): string {
  return `$${value.toFixed(4).replace(/(\.\d{2}\d*?)0+$/, "$1")}`;
}

/** `$2.50 in · $10.00 out /1M`, `Free`, or `price —`. */
export function priceText(cost: ModelCost | null | undefined): string {
  if (!cost) return "price —";
  if (isFree(cost)) return "Free";
  return `${dollars(cost.input)} in · ${dollars(cost.output)} out /1M`;
}

/** Cache prices, when the catalog has them; shown as a second, quieter line. */
export function cacheText(cost: ModelCost | null | undefined): string | null {
  if (!cost || isFree(cost)) return null;
  const parts = [
    cost.cache_read === undefined ? null : `cache read ${dollars(cost.cache_read)}`,
    cost.cache_write === undefined ? null : `cache write ${dollars(cost.cache_write)}`,
  ].filter((part): part is string => part !== null);
  return parts.length > 0 ? `${parts.join(" · ")} /1M` : null;
}

export type NumberRange = { min: number | null; max: number | null };
export type PriceRange = NumberRange;

/** `$1.00`, `$1.00–$5.00`, or null when either bound is unknown. */
export function rangeText(range: NumberRange | null | undefined): string | null {
  if (!range || range.min === null || range.max === null) return null;
  return range.min === range.max
    ? dollars(range.min)
    : `${dollars(range.min)}–${dollars(range.max)}`;
}
