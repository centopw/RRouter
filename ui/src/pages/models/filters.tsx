import { useState, type ReactNode } from "react";
import * as Popover from "@radix-ui/react-popover";
import { CaretDown } from "@phosphor-icons/react";
import { Link } from "react-router-dom";
import { Checkbox } from "../../components/atoms";
import { Button, FilterChip, Input, Select } from "../../components/ui";
import { ALL, STATUSES, STATUS_LABELS, type StatusKey } from "./params";

/** The row kinds the one table can hold; `all` is the absence of the `?kind=` filter. */
export const KINDS = ["all", "models", "aliases", "combos"] as const;
export type KindKey = (typeof KINDS)[number];
export const KIND_LABELS: Record<KindKey, string> = {
  all: "all kinds",
  models: "models",
  aliases: "aliases",
  combos: "combos",
};

/**
 * The one toolbar. Filters narrow the single table; they never swap the surface, so the
 * columns, the preset header and the row actions stay put while the row set changes.
 *
 * On a phone the row scrolls sideways instead of stacking into three control rows.
 */
export function ModelFilters({
  query,
  onQuery,
  providers,
  provider,
  onProvider,
  kind,
  onKind,
  statuses,
  counts,
  showStatus,
  onStatus,
  free,
  onFree,
  freeCount,
  showFreeModeLink,
  custom,
  onCustom,
  extra,
  outside,
  onShowOutside,
  canClear,
  onClear,
}: {
  query: string;
  onQuery: (next: string) => void;
  /** One option per real provider, plus RRouter for aliases/combos when any exist. */
  providers: Array<{ id: string; display: string }>;
  provider: string;
  onProvider: (next: string) => void;
  kind: KindKey;
  onKind: (next: KindKey) => void;
  statuses: ReadonlySet<StatusKey>;
  counts: Record<StatusKey, number>;
  /** The status filter is preset-scoped: members have no preset, so they get no menu. */
  showStatus: boolean;
  onStatus: (next: Set<StatusKey>) => void;
  free: boolean;
  onFree: () => void;
  /** Free rows matching the other active filters, so the count is visible before clicking. */
  freeCount: number;
  /** Admins get a direct link to Free mode to discover free models or configure fallback. */
  showFreeModeLink: boolean;
  custom: boolean;
  onCustom: () => void;
  /** Page-level controls that belong beside the filters (groups). */
  extra?: ReactNode;
  /** Rows a search found outside the current status filter. */
  outside: number;
  onShowOutside: () => void;
  canClear: boolean;
  onClear: () => void;
}) {
  return (
    <>
      <div className="flex items-center gap-2 overflow-x-auto border-b border-line px-4 py-2 [&>*]:shrink-0">
        <div className="w-[230px] max-w-[60vw]">
          <Input
            value={query}
            onChange={onQuery}
            placeholder="search models, aliases, combos…"
            ariaLabel="Search rows"
          />
        </div>
        <div className="w-[170px]">
          <Select
            ariaLabel="provider filter"
            value={provider}
            onChange={(next) => onProvider(next)}
          >
            <option value={ALL}>all providers</option>
            {providers.map((entry) => (
              <option key={entry.id} value={entry.id}>
                {entry.display}
              </option>
            ))}
          </Select>
        </div>
        <div className="w-[130px]">
          <Select
            ariaLabel="kind filter"
            value={kind}
            onChange={(next) => onKind(next as KindKey)}
          >
            {KINDS.map((value) => (
              <option key={value} value={value}>
                {KIND_LABELS[value]}
              </option>
            ))}
          </Select>
        </div>
        {showStatus ? (
          <StatusMenu statuses={statuses} counts={counts} onStatus={onStatus} />
        ) : null}
        <FilterChip on={free} onClick={onFree} ariaLabel="Only free ($0) models, aliases and combos">
          <span>Free</span>
          {freeCount > 0 ? (
            <span className="text-caption opacity-80 tnum">{freeCount.toLocaleString()}</span>
          ) : null}
        </FilterChip>
        <FilterChip on={custom} onClick={onCustom} ariaLabel="Only custom model rows">
          custom
        </FilterChip>
        {extra}
        <div className="ml-auto flex items-center gap-2 pl-2">
          <Button variant="quiet" onClick={onClear} disabled={!canClear}>
            Clear
          </Button>
        </div>
      </div>
      {free && showFreeModeLink ? (
        <div className="flex flex-wrap items-center justify-between gap-2 border-b border-line bg-surface-2 px-4 py-1.5 text-caption text-muted">
          <span>
            Showing models with a $0 catalog price. Find more free endpoints or enable automatic
            fallback when paid quota runs out in{" "}
            <Link to="/free-mode" className="font-medium text-accent-text underline underline-offset-2">
              Free mode →
            </Link>
          </span>
        </div>
      ) : null}
      {outside > 0 ? (
        <div className="flex flex-wrap items-center gap-2 border-b border-line bg-surface-2 px-4 py-1.5 text-caption text-muted">
          <span className="tnum">
            {outside.toLocaleString()} match{outside === 1 ? "" : "es"} outside{" "}
            {statusSelectionLabel(statuses)}
          </span>
          <Button variant="quiet" onClick={onShowOutside}>
            Show all
          </Button>
        </div>
      ) : null}
    </>
  );
}

function statusSelectionLabel(statuses: ReadonlySet<StatusKey>): string {
  const chosen = STATUSES.filter((status) => statuses.has(status));
  if (chosen.length === STATUSES.length) return "all statuses";
  const [first] = chosen;
  if (first === undefined) return "no statuses";
  if (chosen.length === 1) return STATUS_LABELS[first];
  return `${STATUS_LABELS[first]} +${chosen.length - 1}`;
}

/** A checkbox menu, not a select: the filter is a set and every change is explicit. */
function StatusMenu({
  statuses,
  counts,
  onStatus,
}: {
  statuses: ReadonlySet<StatusKey>;
  counts: Record<StatusKey, number>;
  onStatus: (next: Set<StatusKey>) => void;
}) {
  const [open, setOpen] = useState(false);
  const toggle = (status: StatusKey) => {
    const next = new Set(statuses);
    if (next.has(status)) next.delete(status);
    else next.add(status);
    // An empty set would look broken; the last box cannot be cleared by clicking it.
    if (next.size === 0) return;
    onStatus(next);
  };
  return (
    <Popover.Root open={open} onOpenChange={setOpen}>
      <Popover.Trigger asChild>
        <button
          type="button"
          aria-haspopup="dialog"
          aria-expanded={open}
          className="inline-flex h-8 items-center gap-1.5 rounded-full border border-line bg-surface-2 px-3 text-small text-muted transition-colors hover:border-line-strong hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
        >
          <span>Status</span>
          <span className="font-medium text-fg">{statusSelectionLabel(statuses)}</span>
          <CaretDown size={11} aria-hidden />
        </button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content
          role="group"
          aria-label="Filter rows by status"
          side="bottom"
          align="start"
          sideOffset={4}
          className="popover-panel z-[var(--z-popover-portal)] flex w-60 flex-col gap-1 rounded-[var(--radius-card)] border border-line bg-surface p-2 shadow-[var(--shadow-popover)]"
        >
          {STATUSES.map((status) => (
            <div
              key={status}
              className="flex items-center justify-between gap-2 rounded-[var(--radius-input)] px-2 py-1.5 hover:bg-surface-3"
            >
              <Checkbox
                checked={statuses.has(status)}
                onChange={() => toggle(status)}
                label={STATUS_LABELS[status]}
              />
              <span className="text-caption text-faint tnum">{counts[status].toLocaleString()}</span>
            </div>
          ))}
          <div className="mt-1 flex items-center justify-between border-t border-line px-1 pt-2">
            <button
              type="button"
              className="text-caption text-accent-text underline underline-offset-2"
              onClick={() => onStatus(new Set(STATUSES))}
            >
              Select all
            </button>
            <button
              type="button"
              className="text-caption text-muted underline underline-offset-2"
              onClick={() => onStatus(new Set<StatusKey>(["routed"]))}
            >
              Routed only
            </button>
          </div>
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}
