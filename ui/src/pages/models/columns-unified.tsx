import { useMemo, type ReactNode } from "react";
import { Link } from "react-router-dom";
import { Badge, Button, Select } from "../../components/ui";
import { Toggle } from "../../components/atoms";
import type { Column } from "../../components/table";
import { useModelColumns, type ModelColumnsOptions } from "./columns";
import { NameIdentity, StaleIdentity } from "./table-unified";
import type { AliasRow, ComboRow, UnifiedRow } from "./unified-rows";

/**
 * One column definition for every row kind. Model cells reuse {@link useModelColumns}, so a
 * price, a capability badge or a row menu cannot drift between the board and a filter view;
 * the group and `/v1` cells are the two layout controls the old separate views owned.
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
        if (row.kind !== "model") return <NameIdentity row={row} />;
        if (row.model === null) return <StaleIdentity row={row} />;
        return identity ? dim(row, identity.render(row.model)) : null;
      },
    });

    for (const key of ["context", "max_output", "cost", "capabilities"]) {
      const column = byKey.get(key);
      if (!column) continue;
      columns.push({
        ...column,
        render: (row) =>
          row.kind === "model" && row.model ? (
            dim(row, column.render(row.model))
          ) : (
            <span className="text-faint">—</span>
          ),
      });
    }

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
        width: "5rem",
        render: (row) => {
          if (row.kind !== "model") return <NameActions row={row} />;
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

function NameActions({ row }: { row: AliasRow | ComboRow }) {
  const to = row.kind === "alias" ? "/routing/aliases" : "/routing/combos";
  return (
    <div className="flex justify-end">
      <Link
        to={to}
        className="inline-flex h-8 items-center rounded-full px-2 text-small text-accent-text underline underline-offset-2"
      >
        edit
      </Link>
    </div>
  );
}
