import { useMemo } from "react";
import { Link } from "react-router-dom";
import { Badge, CopyIconButton, cx } from "../../components/ui";
import { Toggle } from "../../components/atoms";
import { CapabilityChips } from "../../components/capability-chips";
import type { Column } from "../../components/table";
import type { Model } from "../../lib/api";
import type { DraftGroup } from "../../lib/layout-draft";
import { alsoMatches, canonicalRef } from "../../lib/model-ref";
import type { ModelTestOutcome } from "./actions";
import {
  AvailabilityBadge,
  FreeBadge,
  ModelMetaLine,
  ModelPreviewBody,
  PROVIDER_LINK,
  RowMenu,
  RowPreview,
  TestButton,
  TestOutcomeInline,
} from "./rows";
import { isFree } from "./price";

/**
 * The registry row, shared by the sortable board and the flat table: one definition, so a
 * price, a capability or a badge can never drift between the two.
 */
export type ModelColumnsOptions = {
  isAdmin: boolean;
  testTarget: string | null;
  testResults?: ReadonlyMap<string, ModelTestOutcome>;
  toggleTarget: string | null;
  removeTarget: string | null;
  onTest: (model: Model) => void;
  onToggle: (model: Model) => void;
  onEdit: (model: Model) => void;
  onDelete: (model: Model) => void;
  /** Only the board has a preset and a layout; the registry passes none. */
  hide?: (ref: string) => void;
  /** Whether this row can be hidden: an entry must exist in the preset. */
  hideFor?: (ref: string) => boolean;
  onMoveTop?: (ref: string) => void;
  onMoveBottom?: (ref: string) => void;
  canMoveTop?: (ref: string) => boolean;
  canMoveBottom?: (ref: string) => boolean;
  groups?: Array<DraftGroup & { implicit?: boolean }>;
  groupOf?: (ref: string) => string | undefined;
  onMoveGroup?: (ref: string, group: string | null) => void;
};

export function useModelColumns({
  isAdmin,
  testTarget,
  testResults,
  toggleTarget,
  removeTarget,
  onTest,
  onToggle,
  onEdit,
  onDelete,
  hide,
  hideFor,
  onMoveTop,
  onMoveBottom,
  canMoveTop,
  canMoveBottom,
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
        wrap: true,
        render: (m) => {
          const groupId = groupOf?.(m.ref);
          const reference = canonicalRef(m, groups, groupId);
          const otherSpellings = alsoMatches(m, groups, groupId);
          const outcome = testResults?.get(m.ref);
          return (
            <div className="flex min-w-0 flex-col">
              <div className="flex min-w-0 flex-wrap items-center gap-1.5">
                <span className="sr-only">Model </span>
                <Link
                  to="/providers"
                  aria-label={`View ${m.provider} on Providers`}
                  className={PROVIDER_LINK}
                >
                  {m.provider}
                </Link>
                <RowPreview
                  label={`Details for ${reference}`}
                  trigger={
                    <span
                      className={cx(
                        // Break instead of truncating: long ids are the norm and a clipped
                        // name reads as a different model.
                        "min-w-0 break-all font-mono text-small",
                        m.enabled ? "text-fg" : "text-faint",
                      )}
                    >
                      {m.id}
                    </span>
                  }
                  content={
                    <ModelPreviewBody
                      model={m}
                      reference={reference}
                      otherSpellings={otherSpellings}
                      testOutcome={outcome}
                    />
                  }
                />
                <CopyIconButton
                  value={reference}
                  label={`Copy model name ${reference}`}
                />
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
                {!m.enabled ? (
                  <span className="shrink-0">
                    <Badge tone="neutral" title="the gateway refuses this id on every endpoint">
                      disabled
                    </Badge>
                  </span>
                ) : !m.available ? (
                  <AvailabilityBadge linked={isAdmin} />
                ) : null}
                {isFree(m.cost) ? <FreeBadge linked={isAdmin} /> : null}
              </div>
              {m.display && m.display !== m.id ? (
                <div className="mt-0.5 min-w-0 truncate text-caption text-faint">{m.display}</div>
              ) : null}
              {reference !== m.ref ? (
                <div className="mt-0.5 min-w-0 break-all font-mono text-caption text-faint">
                  {reference}
                </div>
              ) : null}
              {/* The metadata rides here and nowhere else: the table has no separate
                  context, max-output, price or capability columns. */}
              <ModelMetaLine model={m}>
                <CapabilityChips capabilities={m.capabilities} max={2} subject={m.ref} />
                <TestOutcomeInline outcome={outcome} />
              </ModelMetaLine>
            </div>
          );
        },
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
        // Test + overflow menu (custom delete lives inside the menu, never duplicated).
        width: "6.5rem",
        render: (m) => (
          <div className="flex items-center justify-end gap-0.5">
            <TestButton
              model={m}
              pending={testTarget === m.ref}
              onTest={() => onTest(m)}
            />
            <RowMenu
              model={m}
              pendingDelete={removeTarget === m.ref}
              onEdit={() => onEdit(m)}
              onDelete={() => onDelete(m)}
              onHide={hide && (!hideFor || hideFor(m.ref)) ? () => hide(m.ref) : undefined}
              onMoveTop={onMoveTop ? () => onMoveTop(m.ref) : undefined}
              onMoveBottom={onMoveBottom ? () => onMoveBottom(m.ref) : undefined}
              canMoveTop={canMoveTop ? canMoveTop(m.ref) : undefined}
              canMoveBottom={canMoveBottom ? canMoveBottom(m.ref) : undefined}
              groups={movable}
              currentGroup={groupOf?.(m.ref)}
              onMoveGroup={
                onMoveGroup && movable ? (next: string | null) => onMoveGroup(m.ref, next) : undefined
              }
              // Desktop md+ shows an inline Group select column; keep Move to group only
              // on narrower viewports where that column is hidden.
              groupMenuClassName="md:hidden"
            />
          </div>
        ),
      },
    ];
    // A reader sees the registry, not the controls that change it.
    return isAdmin ? list : list.filter((column) => column.key !== "actions");
  }, [
    isAdmin,
    testTarget,
    testResults,
    toggleTarget,
    removeTarget,
    onTest,
    onToggle,
    onEdit,
    onDelete,
    hide,
    hideFor,
    onMoveTop,
    onMoveBottom,
    canMoveTop,
    canMoveBottom,
    groups,
    groupOf,
    onMoveGroup,
  ]);
}
