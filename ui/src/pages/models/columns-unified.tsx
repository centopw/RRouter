import { useMemo, type ReactNode } from "react";
import { Badge, Button, Select } from "../../components/ui";
import { Toggle } from "../../components/atoms";
import type { Column } from "../../components/table";
import { useModelColumns, type ModelColumnsOptions } from "./columns";
import { StaleIdentity } from "./table-unified";
import type { UnifiedRow } from "./unified-rows";

/**
 * One column definition for the model table. Cells reuse {@link useModelColumns}, so a
 * price, a capability badge or a row menu cannot drift between the board and a filter
 * view; the group and `/v1` cells are the two layout controls this page owns. Aliases
 * and combos live on their own screens, so there is no name row here.
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
  const base = useModelColumns(model);

  return useMemo<Array<Column<UnifiedRow>>>(() => {
    const columns: Array<Column<UnifiedRow>> = [];
    const byKey = new Map(base.map((column) => [column.key, column] as const));
    const identity = byKey.get("model");
    const actions = byKey.get("actions");
    const customGroups = (groups ?? []).filter((group) => !group.provider);

    const dim = (row: UnifiedRow, node: ReactNode) =>
      row.kind === "model" && row.model && !row.model.enabled ? (
        <div className="opacity-60">{node}</div>
      ) : (
        node
      );

    columns.push({
      key: "model",
      title: "provider / name",
      render: (row) => {
        if (row.kind !== "model") return null;
        if (row.model === null) return <StaleIdentity row={row} />;
        return identity ? dim(row, identity.render(row.model)) : null;
      },
    });

    if (isAdmin && presetScoped) {
      columns.push({
        key: "group",
        title: "group",
        width: "9.5rem",
        className: "hidden md:table-cell",
        render: (row) => {
          if (!editable) {
            const explicit = customGroups.find((group) => group.id === groupOf(row.ref));
            return (
              <span className="text-caption text-muted">
                {row.entry ? (explicit?.name ?? "ungrouped") : "—"}
              </span>
            );
          }
          const explicit = customGroups.find((group) => group.id === groupOf(row.ref));
          return (
            <Select
              ariaLabel={`Group for ${row.ref}`}
              value={explicit?.id ?? ""}
              onChange={(next) => onAssignGroup(row.ref, next === "" ? null : next)}
            >
              <option value="">Ungrouped</option>
              {customGroups.map((group) => (
                <option key={group.id} value={group.id}>
                  {group.name}
                </option>
              ))}
            </Select>
          );
        },
      });
      columns.push({
        key: "v1",
        title: "/v1",
        width: "3.5rem",
        render: (row) => {
          // A row the preset has never stored is still advertised while auto-include is
          // on; the toggle must reflect what /v1/models actually serves, not just the
          // presence of an explicit entry.
          // Disabled models never reach /v1/models, whatever the preset says.
          const listed =
            row.status !== "disabled" && (row.entry?.visible ?? row.status === "routed");
          if (!editable) {
            return (
              <Badge tone={listed ? "neutral" : "warn"}>{listed ? "listed" : "hidden"}</Badge>
            );
          }
          return (
            <Toggle
              size="sm"
              checked={listed}
              disabled={row.status === "disabled"}
              label={
                row.status === "disabled"
                  ? `${row.ref} is disabled and cannot be listed`
                  : `List ${row.ref} on /v1/models`
              }
              onChange={(next) => onVisible(row.ref, next)}
            />
          );
        },
      });
    }

    const enabled = byKey.get("enabled");
    if (enabled) {
      columns.push({
        ...enabled,
        render: (row) =>
          row.kind === "model" && row.model ? (
            dim(row, enabled.render(row.model))
          ) : (
            <span className="text-faint">—</span>
          ),
      });
    }

    if (isAdmin && actions) {
      columns.push({
        key: "actions",
        title: "Actions",
        srOnlyTitle: true,
        align: "right",
        // Test + delete (custom only) + overflow, plus the cell's horizontal padding.
        width: "8.5rem",
        render: (row) => {
          if (row.kind !== "model") return null;
          if (!row.model) {
            return editable && row.entry ? (
              <div className="flex justify-end">
                <Button
                  variant="quiet"
                  ariaLabel={`Hide ${row.ref} from the active list`}
                  onClick={() => onHide(row.ref)}
                >
                  hide
                </Button>
              </div>
            ) : null;
          }
          return dim(row, actions.render(row.model));
        },
      });
    }

    return columns;
    // The base columns are rebuilt only when their own inputs change; this list adds the
    // layout controls, which depend on the draft and the group vocabulary.
  }, [
    base,
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
