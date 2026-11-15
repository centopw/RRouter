import { useState, type ReactNode } from "react";
import * as Popover from "@radix-ui/react-popover";
import { CaretDown } from "@phosphor-icons/react";
import { Checkbox } from "../../components/atoms";
import { Button, FilterChip, Input, Select } from "../../components/ui";
import type { ModelProvider } from "../../lib/api";
import { ALL, STATUSES, STATUS_LABELS, type StatusKey } from "./params";

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
  statuses,
  counts,
  showStatus,
  onStatus,
  showNames,
  names,
  onNames,
  custom,
  onCustom,
  shared,
  onShared,
  extra,
  outside,
  onShowOutside,
  canClear,
  onClear,
}: {
  query: string;
  onQuery: (next: string) => void;
  providers: ModelProvider[];
  provider: string;
  onProvider: (next: string) => void;
  statuses: ReadonlySet<StatusKey>;
  counts: Record<StatusKey, number>;
  /** The status filter is preset-scoped: members have no preset, so they get no menu. */
  showStatus: boolean;
  onStatus: (next: Set<StatusKey>) => void;
  showNames: boolean;
  names: boolean;
  onNames: () => void;
  custom: boolean;
  onCustom: () => void;
  shared: boolean;
  onShared: () => void;
  /** A page-level action that belongs beside the filters (group manager). */
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
            ariaLabel="Search models, aliases and combos"
          />
        </div>
        <div className="w-[180px]">
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
        {showStatus ? (
          <StatusMenu statuses={statuses} counts={counts} onStatus={onStatus} />
        ) : null}
        {showNames ? (
          <FilterChip
            on={names}
            onClick={onNames}
            ariaLabel="Include aliases and combos as rows"
          >
            names
          </FilterChip>
        ) : null}
        <FilterChip on={custom} onClick={onCustom} ariaLabel="Only custom model rows">
          custom
        </FilterChip>
        <FilterChip
          on={shared}
          onClick={onShared}
          ariaLabel="Only models served by more than one provider"
        >
          shared
        </FilterChip>
        {extra}
        <div className="ml-auto flex items-center gap-2 pl-2">
          <Button variant="quiet" onClick={onClear} disabled={!canClear}>
            Clear
          </Button>
        </div>
      </div>
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
          className="popover-panel z-50 flex w-60 flex-col gap-1 rounded-[var(--radius-card)] border border-line bg-surface p-2 shadow-lg"
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
