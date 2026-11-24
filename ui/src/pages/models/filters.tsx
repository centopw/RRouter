import { useState } from "react";
import * as Popover from "@radix-ui/react-popover";
import { CaretDown, Check, MagnifyingGlass, SlidersHorizontal } from "@phosphor-icons/react";
import { Link } from "react-router-dom";
import { Checkbox } from "../../components/atoms";
import { Button, Field, FilterChip, Input, Modal, Select, cx } from "../../components/ui";
import {
  ALL,
  STATUSES,
  STATUS_LABELS,
  countActiveFilters,
  isDefaultStatusSet,
  type StatusKey,
} from "./params";

/** The row kinds the one table can hold; `all` is the absence of the `?kind=` filter. */
export const KINDS = ["all", "models", "aliases", "combos"] as const;
export type KindKey = (typeof KINDS)[number];
export const KIND_LABELS: Record<KindKey, string> = {
  all: "All kinds",
  models: "Models",
  aliases: "Aliases",
  combos: "Combos",
};

const PILL_TRIGGER_BASE =
  "inline-flex h-8 shrink-0 items-center gap-1.5 whitespace-nowrap rounded-full border px-3 text-small transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]";

const PILL_TRIGGER_REST =
  "border-line bg-surface-2 text-muted hover:border-[var(--color-line-strong)] hover:text-fg";

const PILL_TRIGGER_ACTIVE =
  "border-[var(--color-line-strong)] bg-surface text-fg hover:border-[var(--color-fg)]/40";

/**
 * The unified Models filter bar.
 *
 * Desktop uses one visual vocabulary: search input, pill dropdowns (Provider, Kind, Status),
 * and toggle chips (Free, Custom). On phone viewports (`< 640px`), the bar collapses to
 * search plus a `Filters (n)` button that opens a filter sheet — zero sideways scrolling.
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
  outside,
  onShowOutside,
  canClear,
  onClear,
  phone,
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
  /** Rows a search found outside the current status filter. */
  outside: number;
  onShowOutside: () => void;
  canClear: boolean;
  onClear: () => void;
  phone: boolean;
}) {
  const [sheetOpen, setSheetOpen] = useState(false);

  const activeCount = countActiveFilters({
    provider,
    kind,
    statuses,
    statusAware: showStatus,
    free,
    custom,
  });

  const selectedProviderLabel =
    provider === ALL
      ? null
      : (providers.find((entry) => entry.id === provider)?.display ?? provider);

  const toggleStatus = (status: StatusKey) => {
    const next = new Set(statuses);
    if (next.has(status)) next.delete(status);
    else next.add(status);
    // An empty set would look broken; the last box cannot be cleared by clicking it.
    if (next.size === 0) return;
    onStatus(next);
  };

  return (
    <>
      {phone ? (
        <div className="flex items-center gap-2 border-b border-line px-3.5 py-2.5">
          <div className="relative min-w-0 flex-1">
            <MagnifyingGlass
              size={14}
              aria-hidden
              className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-faint"
            />
            <Input
              value={query}
              onChange={onQuery}
              placeholder="Search models, aliases, combos…"
              ariaLabel="Search rows"
              className="pl-8"
            />
          </div>
          <button
            type="button"
            onClick={() => setSheetOpen(true)}
            aria-label={activeCount > 0 ? `Filters, ${activeCount} active` : "Filters"}
            aria-haspopup="dialog"
            aria-expanded={sheetOpen}
            className={cx(
              PILL_TRIGGER_BASE,
              activeCount > 0
                ? "border-accent bg-accent font-medium text-[var(--color-accent-ink)]"
                : PILL_TRIGGER_REST,
            )}
          >
            <SlidersHorizontal size={14} aria-hidden />
            <span>Filters</span>
            {activeCount > 0 ? <span className="tnum">({activeCount})</span> : null}
          </button>
        </div>
      ) : (
        <div className="flex flex-wrap items-center gap-2 border-b border-line px-4 py-2.5">
          <div className="relative w-[260px] max-w-full shrink-0">
            <MagnifyingGlass
              size={14}
              aria-hidden
              className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-faint"
            />
            <Input
              value={query}
              onChange={onQuery}
              placeholder="Search models, aliases, combos…"
              ariaLabel="Search rows"
              className="pl-8"
            />
          </div>

          <PillMenu
            label="Provider"
            activeValue={selectedProviderLabel}
            ariaLabel="Provider filter"
            options={[
              { value: ALL, label: "All providers" },
              ...providers.map((entry) => ({ value: entry.id, label: entry.display })),
            ]}
            value={provider}
            onChange={onProvider}
          />

          <PillMenu
            label="Kind"
            activeValue={kind === "all" ? null : KIND_LABELS[kind]}
            ariaLabel="Kind filter"
            options={KINDS.map((value) => ({ value, label: KIND_LABELS[value] }))}
            value={kind}
            onChange={(next) => onKind(next as KindKey)}
          />

          {showStatus ? (
            <StatusMenu
              statuses={statuses}
              counts={counts}
              onToggle={toggleStatus}
              onStatus={onStatus}
            />
          ) : null}

          <FilterChip on={free} onClick={onFree} ariaLabel="Only free ($0) models, aliases and combos">
            <span>Free</span>
            {freeCount > 0 ? (
              <span className="text-caption opacity-80 tnum">{freeCount.toLocaleString()}</span>
            ) : null}
          </FilterChip>

          <FilterChip on={custom} onClick={onCustom} ariaLabel="Only custom model rows">
            Custom
          </FilterChip>

          {canClear ? (
            <div className="ml-auto flex items-center pl-2">
              <Button variant="ghost" size="sm" onClick={onClear} className="text-muted hover:text-fg">
                Clear
              </Button>
            </div>
          ) : null}
        </div>
      )}

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
          <Button variant="quiet" size="sm" onClick={onShowOutside}>
            Show all
          </Button>
        </div>
      ) : null}

      {phone ? (
        <Modal
          open={sheetOpen}
          onClose={() => setSheetOpen(false)}
          variant="drawer"
          width="sm"
          title="Filters"
          description="Narrow models, aliases and combos in this view."
          footer={
            <>
              <Button
                variant="quiet"
                onClick={() => {
                  onClear();
                }}
                disabled={!canClear}
              >
                Clear all
              </Button>
              <Button variant="primary" onClick={() => setSheetOpen(false)}>
                Done
              </Button>
            </>
          }
        >
          <div className="flex flex-col gap-5">
            <Field label="Provider">
              <Select ariaLabel="Provider filter" value={provider} onChange={onProvider}>
                <option value={ALL}>All providers</option>
                {providers.map((entry) => (
                  <option key={entry.id} value={entry.id}>
                    {entry.display}
                  </option>
                ))}
              </Select>
            </Field>

            <Field label="Kind">
              <Select
                ariaLabel="Kind filter"
                value={kind}
                onChange={(next) => onKind(next as KindKey)}
              >
                {KINDS.map((value) => (
                  <option key={value} value={value}>
                    {KIND_LABELS[value]}
                  </option>
                ))}
              </Select>
            </Field>

            {showStatus ? (
              <div className="flex flex-col gap-2">
                <div className="flex items-center justify-between gap-2">
                  <span className="text-small font-medium text-fg">Status</span>
                  <div className="flex items-center gap-3 text-caption">
                    <button
                      type="button"
                      className="text-accent-text underline underline-offset-2"
                      onClick={() => onStatus(new Set(STATUSES))}
                    >
                      Select all
                    </button>
                    <button
                      type="button"
                      className="text-muted underline underline-offset-2"
                      onClick={() => onStatus(new Set<StatusKey>(["routed"]))}
                    >
                      Routed only
                    </button>
                  </div>
                </div>
                <div className="flex flex-col gap-1 rounded-[var(--radius-card)] border border-line bg-surface-2 p-2">
                  {STATUSES.map((status) => (
                    <div
                      key={status}
                      className="flex items-center justify-between gap-2 rounded-[var(--radius-input)] px-2 py-1.5"
                    >
                      <Checkbox
                        checked={statuses.has(status)}
                        onChange={() => toggleStatus(status)}
                        label={STATUS_LABELS[status]}
                      />
                      <span className="text-caption text-faint tnum">
                        {counts[status].toLocaleString()}
                      </span>
                    </div>
                  ))}
                </div>
              </div>
            ) : null}

            <div className="flex flex-col gap-2">
              <span className="text-small font-medium text-fg">Attributes</span>
              <div className="flex flex-wrap items-center gap-2">
                <FilterChip
                  on={free}
                  onClick={onFree}
                  ariaLabel="Only free ($0) models, aliases and combos"
                >
                  <span>Free</span>
                  {freeCount > 0 ? (
                    <span className="text-caption opacity-80 tnum">{freeCount.toLocaleString()}</span>
                  ) : null}
                </FilterChip>
                <FilterChip on={custom} onClick={onCustom} ariaLabel="Only custom model rows">
                  Custom
                </FilterChip>
              </div>
            </div>
          </div>
        </Modal>
      ) : null}
    </>
  );
}

/** Single-select pill dropdown matching `StatusMenu` so every desktop filter shares one style. */
function PillMenu({
  label,
  activeValue,
  ariaLabel,
  options,
  value,
  onChange,
}: {
  label: string;
  activeValue: string | null;
  ariaLabel: string;
  options: Array<{ value: string; label: string }>;
  value: string;
  onChange: (next: string) => void;
}) {
  const [open, setOpen] = useState(false);
  return (
    <Popover.Root open={open} onOpenChange={setOpen}>
      <Popover.Trigger asChild>
        <button
          type="button"
          aria-label={ariaLabel}
          aria-haspopup="dialog"
          aria-expanded={open}
          className={cx(
            PILL_TRIGGER_BASE,
            activeValue ? PILL_TRIGGER_ACTIVE : PILL_TRIGGER_REST,
          )}
        >
          <span className={activeValue ? "text-muted" : undefined}>{label}</span>
          {activeValue ? <span className="max-w-[120px] truncate font-medium text-fg">{activeValue}</span> : null}
          <CaretDown size={11} className="shrink-0 text-muted" aria-hidden />
        </button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content
          role="group"
          aria-label={ariaLabel}
          side="bottom"
          align="start"
          sideOffset={4}
          className="popover-panel z-[var(--z-popover-portal)] flex max-h-72 w-56 flex-col gap-0.5 overflow-y-auto rounded-[var(--radius-card)] border border-line bg-surface p-1 shadow-[var(--shadow-popover)]"
        >
          {options.map((option) => {
            const selected = option.value === value;
            return (
              <button
                key={option.value}
                type="button"
                aria-pressed={selected}
                onClick={() => {
                  onChange(option.value);
                  setOpen(false);
                }}
                className={cx(
                  "flex h-8 w-full shrink-0 items-center justify-between gap-2 rounded-[var(--radius-input)] px-2.5 text-left text-small transition-colors hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-[var(--color-focus-ring)]",
                  selected ? "bg-[var(--color-selected)] font-medium text-fg" : "text-fg",
                )}
              >
                <span className="truncate">{option.label}</span>
                {selected ? (
                  <Check size={14} weight="bold" className="shrink-0 text-accent-text" aria-hidden />
                ) : null}
              </button>
            );
          })}
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}

function statusSelectionLabel(statuses: ReadonlySet<StatusKey>): string {
  const chosen = STATUSES.filter((status) => statuses.has(status));
  if (chosen.length === STATUSES.length) return "All";
  const [first] = chosen;
  if (first === undefined) return "None";
  if (chosen.length === 1) return STATUS_LABELS[first];
  return `${STATUS_LABELS[first]} +${chosen.length - 1}`;
}

/** A checkbox menu, not a select: the filter is a set and every change is explicit. */
function StatusMenu({
  statuses,
  counts,
  onToggle,
  onStatus,
}: {
  statuses: ReadonlySet<StatusKey>;
  counts: Record<StatusKey, number>;
  onToggle: (status: StatusKey) => void;
  onStatus: (next: Set<StatusKey>) => void;
}) {
  const [open, setOpen] = useState(false);
  const nonDefault = !isDefaultStatusSet(statuses);
  return (
    <Popover.Root open={open} onOpenChange={setOpen}>
      <Popover.Trigger asChild>
        <button
          type="button"
          aria-haspopup="dialog"
          aria-expanded={open}
          className={cx(PILL_TRIGGER_BASE, nonDefault ? PILL_TRIGGER_ACTIVE : PILL_TRIGGER_REST)}
        >
          <span className="text-muted">Status</span>
          <span className="font-medium text-fg">{statusSelectionLabel(statuses)}</span>
          <CaretDown size={11} className="shrink-0 text-muted" aria-hidden />
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
                onChange={() => onToggle(status)}
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
