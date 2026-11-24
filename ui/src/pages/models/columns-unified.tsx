import { useMemo, type ReactNode } from "react";
import type { Column } from "../../components/table";
import { ModelIdentityCell, type ModelColumnsOptions } from "./columns";
import { NameRowMenu, PlayLinkButton, RowMenu, TestButton } from "./rows";
import { NameIdentity, StaleIdentity, UnifiedStatusCell } from "./table-unified";
import type { UnifiedRow } from "./unified-rows";

/**
 * One column definition for the union table:
 * - `model`: identity, pricing, context/max, and capabilities
 * - `status`: single HealthBadge-style status pill (Routed / Unavailable / Hidden / Not in preset / Disabled)
 * - `actions`: one uniform `▷` + `⋯` action cell for every row kind (admin only)
 *
 * Per-row group assignment, `/v1` hide/show, and global enable/disable live inside `⋯`
 * so the default board never repeats 20 identical "Ungrouped" selects or twin toggles.
 */
export function useUnifiedColumns({
  model,
  isAdmin,
  editable,
  presetScoped,
  groups,
  groupOf,
  onAssignGroup,
  onVisible,
  onHide,
}: {
  model: ModelColumnsOptions;
  isAdmin: boolean;
  /** A loaded, non-conflicted draft; layout controls are live. */
  editable: boolean;
  /** A preset exists at all, so group and `/v1` mean something. */
  presetScoped: boolean;
  groups?: Array<{ id: string; name: string; provider?: string }>;
  groupOf: (ref: string) => string;
  onAssignGroup: (ref: string, group: string | null) => void;
  onVisible: (ref: string, visible: boolean) => void;
  onHide: (ref: string) => void;
}): Array<Column<UnifiedRow>> {
  return useMemo<Array<Column<UnifiedRow>>>(() => {
    const columns: Array<Column<UnifiedRow>> = [];
    const customGroupById = new Map(
      (groups ?? [])
        .filter((group) => !group.provider)
        .map((group) => [group.id, group.name] as const),
    );
    const movableGroups =
      isAdmin && editable && presetScoped ? model.groups : undefined;

    const dim = (row: UnifiedRow, node: ReactNode) =>
      row.kind === "model" && row.model && !row.model.enabled ? (
        <div className="opacity-60">{node}</div>
      ) : (
        node
      );

    columns.push({
      key: "model",
      title: "Model",
      wrap: true,
      render: (row) => {
        const customGroupLabel = customGroupById.get(groupOf(row.ref));
        if (row.kind !== "model") {
          return <NameIdentity row={row} customGroupLabel={customGroupLabel} />;
        }
        if (row.model === null) {
          return <StaleIdentity row={row} customGroupLabel={customGroupLabel} />;
        }
        return dim(
          row,
          <ModelIdentityCell
            model={row.model}
            isAdmin={model.isAdmin}
            groups={model.groups}
            groupId={model.groupOf?.(row.model.ref)}
            outcome={model.testResults?.get(row.model.ref)}
            rowStatus={row.status}
            customGroupLabel={customGroupLabel}
          />,
        );
      },
    });

    columns.push({
      key: "status",
      title: "Status",
      align: "right",
      width: "8.5rem",
      render: (row) => <UnifiedStatusCell row={row} />,
    });

    if (isAdmin) {
      columns.push({
        key: "actions",
        title: "Actions",
        srOnlyTitle: true,
        align: "right",
        width: "5.25rem",
        render: (row) => {
          const canHide = editable && presetScoped;
          const hideAction = canHide
            ? () => (row.entry ? onHide(row.ref) : onVisible(row.ref, false))
            : undefined;
          const showAction = canHide ? () => onVisible(row.ref, true) : undefined;
          const onMoveGroup =
            movableGroups
              ? (next: string | null) => onAssignGroup(row.ref, next)
              : undefined;

          if (row.kind !== "model") {
            return (
              <div className="flex items-center justify-end gap-0.5">
                <PlayLinkButton
                  to={`/playground/chat?model=${encodeURIComponent(row.name)}`}
                  label={`Test ${row.name} in Playground`}
                  title={`Test ${row.name} in Playground`}
                />
                <NameRowMenu
                  subject={row.ref}
                  provider="rrouter"
                  kind={row.kind}
                  name={row.name}
                  rowStatus={row.status}
                  onHide={hideAction}
                  onShow={showAction}
                  onMoveTop={model.onMoveTop ? () => model.onMoveTop!(row.ref) : undefined}
                  onMoveBottom={
                    model.onMoveBottom ? () => model.onMoveBottom!(row.ref) : undefined
                  }
                  canMoveTop={model.canMoveTop ? model.canMoveTop(row.ref) : undefined}
                  canMoveBottom={
                    model.canMoveBottom ? model.canMoveBottom(row.ref) : undefined
                  }
                  groups={movableGroups}
                  currentGroup={groupOf(row.ref)}
                  onMoveGroup={onMoveGroup}
                />
              </div>
            );
          }

          if (!row.model) {
            if (!canHide) return null;
            const cut = row.ref.indexOf("/");
            const staleProvider = cut < 0 ? row.ref : row.ref.slice(0, cut);
            return (
              <div className="flex items-center justify-end gap-0.5">
                <NameRowMenu
                  subject={row.ref}
                  provider={staleProvider}
                  kind="stale"
                  rowStatus={row.status}
                  onHide={row.entry ? () => onHide(row.ref) : undefined}
                  onShow={showAction}
                  groups={movableGroups}
                  currentGroup={groupOf(row.ref)}
                  onMoveGroup={onMoveGroup}
                />
              </div>
            );
          }

          const m = row.model;
          return (
            <div className="flex items-center justify-end gap-0.5">
              <TestButton
                model={m}
                pending={model.testTarget === m.ref}
                onTest={() => model.onTest(m)}
              />
              <RowMenu
                model={m}
                pendingDelete={model.removeTarget === m.ref}
                pendingToggle={model.toggleTarget === m.id}
                rowStatus={row.status}
                onEdit={() => model.onEdit(m)}
                onDelete={() => model.onDelete(m)}
                onToggle={() => model.onToggle(m)}
                onHide={hideAction}
                onShow={showAction}
                onMoveTop={model.onMoveTop ? () => model.onMoveTop!(m.ref) : undefined}
                onMoveBottom={model.onMoveBottom ? () => model.onMoveBottom!(m.ref) : undefined}
                canMoveTop={model.canMoveTop ? model.canMoveTop(m.ref) : undefined}
                canMoveBottom={model.canMoveBottom ? model.canMoveBottom(m.ref) : undefined}
                groups={movableGroups}
                currentGroup={groupOf(m.ref)}
                onMoveGroup={onMoveGroup}
              />
            </div>
          );
        },
      });
    }

    return columns;
  }, [
    model,
    isAdmin,
    editable,
    presetScoped,
    groups,
    groupOf,
    onAssignGroup,
    onVisible,
    onHide,
  ]);
}
