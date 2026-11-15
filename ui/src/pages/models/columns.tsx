import { useMemo } from "react";
import { Trash } from "@phosphor-icons/react";
import { Badge, CopyIconButton, cx } from "../../components/ui";
import { IconButton, Toggle } from "../../components/atoms";
import { CapabilityChips } from "../../components/capability-chips";
import type { Column } from "../../components/table";
import { compact } from "../../lib/format";
import type { Model } from "../../lib/api";
import type { DraftGroup } from "../../lib/layout-draft";
import { CoveragePeers, RowMenu, TestButton, groupModelRef, priceLabel } from "./rows";

/**
 * The registry row, shared by the sortable board and the plain table: one definition, so a
 * price, a capability or a badge can never drift between the two.
 */
export type ModelColumnsOptions = {
  isAdmin: boolean;
  /** Every model, by canonical id, for the "served by N providers" chip. */
  coverage: Map<string, Model[]>;
  providerDisplay: Map<string, string>;
  testTarget: string | null;
  toggleTarget: string | null;
  removeTarget: string | null;
  onTest: (model: Model) => void;
  onToggle: (model: Model) => void;
  onEdit: (model: Model) => void;
  onDelete: (model: Model) => void;
  onPrefer: (model: string, provider: string | null) => void;
  onPickProvider: (provider: string) => void;
  /** Only the board has a preset and a layout; the registry passes none. */
  hide?: (ref: string) => void;
  /** Whether this row can be hidden: an entry must exist in the preset. */
  hideFor?: (ref: string) => boolean;
  groups?: Array<DraftGroup & { implicit?: boolean }>;
  groupOf?: (ref: string) => string | undefined;
  onMoveGroup?: (ref: string, group: string | null) => void;
};

export function useModelColumns({
  isAdmin,
  coverage,
  providerDisplay,
  testTarget,
  toggleTarget,
  removeTarget,
  onTest,
  onToggle,
  onEdit,
  onDelete,
  onPrefer,
  onPickProvider,
  hide,
  hideFor,
  groups,
  groupOf,
  onMoveGroup,
}: ModelColumnsOptions): Array<Column<Model>> {
  return useMemo<Array<Column<Model>>>(() => {
    const movable = isAdmin && groups ? groups : undefined;
    const list: Array<Column<Model>> = [
      {
        key: "model",
        title: "model",
        render: (m) => (
          <div className="flex min-w-0 flex-col">
            <div className="flex min-w-0 items-center gap-1.5">
              <span className="sr-only">Model </span>
              {/* The badge never shrinks: a truncated provider reads as a different one. */}
              <span className="shrink-0 rounded-full border border-line bg-surface-2 px-1.5 py-px font-mono text-[10px] uppercase tracking-[0.06em] text-muted">
                {m.provider}
              </span>
              <span
                title={m.ref}
                className={cx(
                  // `min-w-0` matters: a truncating flex item keeps its full intrinsic
                  // width otherwise, and the table's auto layout would then scroll.
                  "min-w-0 truncate font-mono text-small",
                  m.enabled ? "text-fg" : "text-faint",
                )}
              >
                {m.id}
              </span>
              <CopyIconButton
                value={groupModelRef(m, groupOf?.(m.ref), groups)}
                label={`Copy model reference ${m.id}`}
              />
              <CoveragePeers
                model={m}
                peers={coverage.get(m.id) ?? []}
                displayOf={(id: string) => providerDisplay.get(id) ?? id}
                onPick={onPickProvider}
                onPrefer={(provider: string | null) => onPrefer(m.id, provider)}
              />
              {m.preferred_provider === m.provider ? (
                <span className="shrink-0">
                  <Badge tone="accent" title={`preferred provider for ${m.id}`}>
                    preferred
                  </Badge>
                </span>
              ) : null}
              {m.source === "custom" ? (
                <span className="shrink-0">
                  <Badge tone="accent" title="added by an operator, not synced from the provider">
                    custom
                  </Badge>
                </span>
              ) : null}
              {m.deprecated ? (
                <span className="shrink-0">
                  <Badge tone="warn" title="this family is retired by its provider">
                    deprecated
                  </Badge>
                </span>
              ) : null}
              {m.enabled ? null : (
                <span className="shrink-0">
                  <Badge tone="neutral" title="the gateway refuses this id on every endpoint">
                    disabled
                  </Badge>
                </span>
              )}
            </div>
            {m.display && m.display !== m.id ? (
              <div className="mt-0.5 min-w-0 truncate text-caption text-faint">{m.display}</div>
            ) : null}
            {/* The metadata rides here and nowhere else: the table has no separate
                context, max-output, price or capability columns. */}
            <div className="mt-1 flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-caption text-faint">
              <span className="whitespace-nowrap tnum">ctx {compact(m.context)}</span>
              <span className="whitespace-nowrap tnum">max {compact(m.max_output)}</span>
              {priceLabel(m) ? (
                <span className="whitespace-nowrap tnum">{priceLabel(m)}</span>
              ) : null}
              <CapabilityChips capabilities={m.capabilities} max={2} subject={m.ref} />
            </div>
          </div>
        ),
      },
      {
        key: "enabled",
        title: "enabled",
        width: "4.5rem",
        render: (m) => {
          // A reader sees the state; only an admin can flip it.
          if (!isAdmin) {
            return <Badge tone={m.enabled ? "pos" : "neutral"}>{m.enabled ? "on" : "off"}</Badge>;
          }
          return (
            <Toggle
              size="sm"
              checked={m.enabled}
              label={`${m.ref} ${m.enabled ? "enabled" : "disabled"}`}
              disabled={toggleTarget === m.id}
              onChange={() => onToggle(m)}
            />
          );
        },
      },
      {
        key: "actions",
        title: "Actions",
        srOnlyTitle: true,
        align: "right",
        // Test + delete (custom only) + overflow, plus the cell's horizontal padding.
        width: "8.5rem",
        render: (m) => (
          <div className="flex items-center justify-end gap-0.5">
            <TestButton
              model={m}
              pending={testTarget === m.ref}
              onTest={() => onTest(m)}
            />
            {m.source === "custom" ? (
              <IconButton
                label={`Delete custom model ${m.ref}`}
                disabled={removeTarget === m.ref}
                onClick={() => onDelete(m)}
                className="hover:bg-[var(--color-neg-tint)] hover:text-[var(--color-neg-ink)]"
              >
                <Trash size={15} aria-hidden />
              </IconButton>
            ) : null}
            <RowMenu
              model={m}
              pendingDelete={removeTarget === m.ref}
              onEdit={() => onEdit(m)}
              onDelete={() => onDelete(m)}
              onHide={hide && (!hideFor || hideFor(m.ref)) ? () => hide(m.ref) : undefined}
              groups={movable}
              currentGroup={groupOf?.(m.ref)}
              onMoveGroup={
                onMoveGroup && movable ? (next: string | null) => onMoveGroup(m.ref, next) : undefined
              }
            />
          </div>
        ),
      },
    ];
    // A reader sees the registry, not the controls that change it.
    return isAdmin ? list : list.filter((column) => column.key !== "actions");
  }, [
    isAdmin,
    coverage,
    providerDisplay,
    testTarget,
    toggleTarget,
    removeTarget,
    onTest,
    onToggle,
    onEdit,
    onDelete,
    onPrefer,
    onPickProvider,
    hide,
    hideFor,
    groups,
    groupOf,
    onMoveGroup,
  ]);
}
