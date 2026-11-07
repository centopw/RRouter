import { compact, ms, pct, splitRef, usd } from "../../lib/format";
import { cx } from "../../components/ui";
import type { UsageSummaryRow } from "../../lib/api";

/** Models in the window, ranked by tokens; selecting a row filters the log. */

export type ModelRow = UsageSummaryRow & { ref: string; tokens: number };

export function ModelTable({
  rows,
  totalTokens,
  colorOf,
  selected,
  onSelect,
}: {
  rows: ModelRow[];
  totalTokens: number;
  colorOf: (ref: string) => string;
  selected: string | null;
  onSelect: (ref: string) => void;
}) {
  if (rows.length === 0) {
    return <div className="px-6 py-8 text-center text-[13px] text-faint">No models served in this window.</div>;
  }
  return (
    <div className="overflow-x-auto">
      <div role="table" aria-label="models in use" className="min-w-[760px]">
        <div
          role="row"
          className="grid grid-cols-[minmax(0,1fr)_90px_190px_88px_96px_72px] items-center gap-3 border-b border-line px-6 py-2 text-[11px] uppercase tracking-[0.06em] text-faint"
        >
          <span role="columnheader">Model</span>
          <span role="columnheader" className="text-right">
            Requests
          </span>
          <span role="columnheader" className="text-right">
            Tokens
          </span>
          <span role="columnheader" className="text-right">
            Cost
          </span>
          <span role="columnheader" className="text-right">
            Avg latency
          </span>
          <span role="columnheader" className="text-right">
            Errors
          </span>
        </div>
        <div role="rowgroup">
          {rows.map((row) => {
            const on = selected === row.ref;
            const share = totalTokens > 0 ? (row.tokens / totalTokens) * 100 : 0;
            return (
              <div
                role="row"
                key={row.ref}
                tabIndex={0}
                aria-selected={on}
                onClick={() => onSelect(row.ref)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    onSelect(row.ref);
                  }
                }}
                className={cx(
                  "grid cursor-pointer grid-cols-[minmax(0,1fr)_90px_190px_88px_96px_72px] items-center gap-3 border-b border-[var(--color-rule)] py-2.5 pl-5 pr-6 text-[13.5px] last:border-0 focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent-text",
                  on ? "bg-[var(--color-surface-2)]" : "hover:bg-[var(--color-row-hover)]",
                )}
              >
                <span role="cell" className="flex min-w-0 items-center gap-2">
                  {on ? <span aria-hidden className="-ml-5 h-6 w-[2px] shrink-0 rounded-full bg-fg" /> : null}
                  <span
                    aria-hidden
                    className="h-2.5 w-2.5 shrink-0 rounded-[3px]"
                    style={{ background: colorOf(row.ref) }}
                  />
                  <ModelName ref_={row.ref} on={on} />
                </span>
                <span role="cell" className="text-right font-mono text-[12.5px] text-muted tnum">
                  {compact(row.requests)}
                </span>
                <span role="cell" className="text-right">
                  <span className="font-mono text-[12.5px] text-fg tnum">{compact(row.tokens)}</span>
                  <span className="mt-1 flex items-center justify-end gap-2">
                    <span className="h-1.5 w-full max-w-[110px] overflow-hidden rounded-full bg-[var(--color-rule)]">
                      <span
                        className="block h-full rounded-full"
                        style={{ width: `${Math.max(2, share)}%`, background: colorOf(row.ref) }}
                      />
                    </span>
                    <span className="w-10 text-right font-mono text-[11px] text-faint tnum">
                      {pct(share)}
                    </span>
                  </span>
                </span>
                <span
                  role="cell"
                  className={cx(
                    "text-right font-mono text-[12.5px] tnum",
                    row.cost_micros > 0 ? "text-muted" : "text-faint",
                  )}
                >
                  {usd(row.cost_micros)}
                </span>
                <span role="cell" className="text-right font-mono text-[12.5px] text-muted tnum">
                  {ms(row.avg_latency_ms)}
                </span>
                <span
                  role="cell"
                  className={cx(
                    "text-right font-mono text-[12.5px] tnum",
                    row.errors > 0 ? "text-neg" : "text-faint",
                  )}
                >
                  {row.errors}
                </span>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

/** `provider/id` as the registry stores it: provider faint, id legible. The row owns the
 *  click target, so every cell selects the model rather than only this label. */
function ModelName({ ref_, on }: { ref_: string; on: boolean }) {
  const { provider, id } = splitRef(ref_);
  return (
    <span
      title={on ? "clear the model filter" : `filter the log to ${ref_}`}
      className="min-w-0 truncate text-left"
    >
      {provider ? <span className="font-mono text-[12px] text-faint">{provider}/</span> : null}
      <span className="text-fg">{id}</span>
    </span>
  );
}
