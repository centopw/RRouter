import { Link } from "react-router-dom";
import { Badge, Button, CopyIconButton, Select, cx } from "../../components/ui";
import { Toggle } from "../../components/atoms";
import type { Column } from "../../components/table";
import { DragHandle, DragPreview, SortableLayout, useRowDrag } from "../../components/sortable-list";
import type { LayoutRow } from "../../components/sortable-list";
import type { Model } from "../../lib/api";
import type { DraftGroup } from "../../lib/layout-draft";
import { GroupHeading } from "./groups";
import { ModelCardBody } from "./rows";
import { nameDetail, type AliasRow, type ComboRow, type UnifiedRow } from "./unified-rows";

const CHIP =
  "shrink-0 rounded-full border border-line bg-surface-2 px-1.5 py-px font-mono text-[10px] uppercase tracking-[0.06em] text-muted";

/** The registry mutations a model row reaches; the page owns the hooks behind them. */
export type ModelActions = {
  isAdmin: boolean;
  testTarget: string | null;
  toggleTarget: string | null;
  removeTarget: string | null;
  onTest: (model: Model) => void;
  onToggle: (model: Model) => void;
  onEdit: (model: Model) => void;
  onDelete: (model: Model) => void;
  coverageFor: (id: string) => Model[];
  displayOfProvider: (id: string) => string;
  onPickProvider: (id: string) => void;
  onPrefer: (id: string, provider: string | null) => void;
  /** Hide/restore is an entry operation; absent on rows the preset never stored. */
  onHide: (ref: string) => void;
};

/** Group and `/v1` are draft writes; absent when no preset is loaded for this caller. */
export type LayoutControls = {
  editable: boolean;
  groups: Array<DraftGroup & { implicit?: boolean }>;
  groupOf: (ref: string) => string;
  onAssignGroup: (ref: string, group: string | null) => void;
  onVisible: (ref: string, visible: boolean) => void;
};

/** One routed group: heading, drag-orderable table on desktop, cards on a phone. */
export function BoardSection({
  group,
  count,
  rows,
  columns,
  actions,
  controls,
  phone,
  drag,
  reducedMotion,
  onReorder,
  onHide,
  onMove,
  showHeader = true,
}: {
  group: DraftGroup & { implicit?: boolean };
  count: number;
  rows: UnifiedRow[];
  columns: Array<Column<UnifiedRow>>;
  actions: ModelActions;
  controls: LayoutControls | undefined;
  phone: boolean;
  drag: boolean;
  reducedMotion: boolean;
  onReorder: (from: string, to: string) => void;
  onHide: (ref: string) => void;
  onMove: (ref: string, delta: number) => void;
  /** Columns are painted once per page; later groups continue the same table. */
  showHeader?: boolean;
}) {
  return (
    <section aria-label={group.name}>
      <GroupHeading group={group} count={count} />
      {phone ? (
        <ul aria-label={`${group.name} rows`} className="flex flex-col">
          {rows.map((row, index) => (
            <li
              key={row.ref}
              className="border-b border-[var(--color-rule)] px-4 py-3 last:border-b-0"
            >
              <UnifiedCard
                row={row}
                actions={actions}
                controls={controls}
                onHide={onHide}
                move={
                  controls?.editable
                    ? {
                        up: index > 0,
                        down: index < rows.length - 1,
                        onMove: (delta) => onMove(row.ref, delta),
                      }
                    : undefined
                }
              />
            </li>
          ))}
        </ul>
      ) : (
        <BoardTable
          label={group.name}
          rows={rows}
          columns={columns}
          drag={drag}
          reducedMotion={reducedMotion}
          onReorder={onReorder}
          showHeader={showHeader}
        />
      )}
    </section>
  );
}

function BoardTable({
  label,
  rows,
  columns,
  drag,
  reducedMotion,
  onReorder,
  showHeader,
}: {
  label: string;
  rows: UnifiedRow[];
  columns: Array<Column<UnifiedRow>>;
  drag: boolean;
  reducedMotion: boolean;
  onReorder: (from: string, to: string) => void;
  showHeader: boolean;
}) {
  const layoutRows: LayoutRow[] = rows.map((row) => ({
    ref: row.ref,
    model: row.kind === "model" ? row.model : null,
    visible: row.entry?.visible ?? true,
    isNew: false,
  }));
  const table = (
    <div className="table-scroll overflow-x-auto" role="region" aria-label={`${label} models`} tabIndex={0}>
      <table className="w-full table-fixed border-separate border-spacing-0 text-body">
        <caption className="sr-only">{label} models</caption>
        {/* Widths live in a colgroup so a group whose header is visually skipped still
            lines up with the first group's columns. */}
        <colgroup>
          {drag ? <col style={{ width: "2.5rem" }} /> : null}
          {columns.map((column) => (
            <col key={column.key} style={column.width ? { width: column.width } : undefined} />
          ))}
        </colgroup>
        <thead className={showHeader ? "sticky top-0 z-20" : "sr-only"}>
          <tr>
            {drag ? (
              <th scope="col" className="w-10 border-b border-line bg-surface px-1 py-2.5">
                <span className="sr-only">reorder</span>
              </th>
            ) : null}
            {columns.map((column) => (
              <th
                key={column.key}
                scope="col"
                style={column.width ? { width: column.width } : undefined}
                className={cx(
                  "sticky top-0 z-20 border-b border-line bg-surface px-4 py-2.5 text-small font-semibold text-ink-soft",
                  column.align === "right" && "text-right",
                  column.key === "actions" &&
                    "sticky right-0 z-30 border-l border-line bg-surface shadow-[-8px_0_8px_-8px_rgba(28,43,66,0.16)]",
                  column.className,
                )}
              >
                {column.srOnlyTitle ? (
                  <span className="sr-only">{column.title}</span>
                ) : (
                  column.title
                )}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row) =>
            drag ? (
              <SortableRow
                key={row.ref}
                row={row}
                columns={columns}
                reducedMotion={reducedMotion}
              />
            ) : (
              <StaticRow key={row.ref} row={row} columns={columns} />
            ),
          )}
        </tbody>
      </table>
    </div>
  );

  if (!drag) return table;
  return (
    <SortableLayout
      rows={layoutRows}
      onReorder={onReorder}
      reducedMotion={reducedMotion}
      overlay={(row) => <DragPreview row={row} />}
    >
      {table}
    </SortableLayout>
  );
}

function RowCells({ row, columns }: { row: UnifiedRow; columns: Array<Column<UnifiedRow>> }) {
  return (
    <>
      {columns.map((column) => {
        const content = column.render(row);
        const shared = cx(
          "border-b border-[var(--color-rule)] px-4 py-2.5 align-middle text-fg",
          column.align === "right" && "text-right tnum",
          column.key === "actions" &&
            "sticky right-0 z-10 border-l border-line bg-surface shadow-[-8px_0_8px_-8px_rgba(28,43,66,0.16)] group-hover:bg-[var(--color-row-hover)]",
          column.className,
        );
        if (column.key === "model") {
          return (
            <th
              key={column.key}
              scope="row"
              className={cx(shared, "text-left font-normal group-hover:bg-[var(--color-row-hover)]")}
            >
              {content}
            </th>
          );
        }
        return (
          <td key={column.key} className={cx(shared, "group-hover:bg-[var(--color-row-hover)]")}>
            {content}
          </td>
        );
      })}
    </>
  );
}

function SortableRow({
  row,
  columns,
  reducedMotion,
}: {
  row: UnifiedRow;
  columns: Array<Column<UnifiedRow>>;
  reducedMotion: boolean;
}) {
  const drag = useRowDrag(row.ref, reducedMotion);
  return (
    <tr
      ref={drag.setNodeRef}
      style={drag.style}
      data-dragging={drag.isDragging || undefined}
      className={cx("group", drag.isDragging && "relative z-10 opacity-70")}
    >
      <td className="border-b border-[var(--color-rule)] px-1 py-2 align-middle">
        <DragHandle label={`Reorder ${row.ref}`} drag={drag} />
      </td>
      <RowCells row={row} columns={columns} />
    </tr>
  );
}

function StaticRow({ row, columns }: { row: UnifiedRow; columns: Array<Column<UnifiedRow>> }) {
  return (
    <tr className="group">
      <RowCells row={row} columns={columns} />
    </tr>
  );
}

/** Phone rows for a filtered table: one card per row, no drag, no group header. */
export function UnifiedCards({
  rows,
  actions,
  controls,
  onHide,
}: {
  rows: UnifiedRow[];
  actions: ModelActions;
  controls: LayoutControls | undefined;
  onHide: (ref: string) => void;
}) {
  return (
    <ul aria-label="Models" className="flex flex-col">
      {rows.map((row) => (
        <li key={row.ref} className="border-b border-[var(--color-rule)] px-4 py-3 last:border-b-0">
          <UnifiedCard row={row} actions={actions} controls={controls} onHide={onHide} />
        </li>
      ))}
    </ul>
  );
}

function UnifiedCard({
  row,
  actions,
  controls,
  onHide,
  move,
}: {
  row: UnifiedRow;
  actions: ModelActions;
  controls: LayoutControls | undefined;
  onHide: (ref: string) => void;
  move?: { up: boolean; down: boolean; onMove: (delta: number) => void };
}) {
  const controlsRow = controls ? (
    <CardControls row={row} controls={controls} move={move} />
  ) : null;

  if (row.kind === "model" && row.model) {
    return (
      <>
        <ModelCardBody
          model={row.model}
          isAdmin={actions.isAdmin}
          testTarget={actions.testTarget}
          toggleTarget={actions.toggleTarget}
          removeTarget={actions.removeTarget}
          onTest={actions.onTest}
          onToggle={actions.onToggle}
          onEdit={actions.onEdit}
          onDelete={actions.onDelete}
          onHide={
            row.entry && controls?.editable ? (ref) => onHide(ref) : undefined
          }
          groups={controls?.editable ? controls.groups : undefined}
          currentGroup={controls?.groupOf(row.ref)}
          onMoveGroup={
            controls?.editable
              ? (group) => controls.onAssignGroup(row.ref, group)
              : undefined
          }
          coverageFor={actions.coverageFor}
          displayOfProvider={actions.displayOfProvider}
          onPickProvider={actions.onPickProvider}
          onPreferPreference={(model, provider) => actions.onPrefer(model.id, provider)}
        />
        {controlsRow}
      </>
    );
  }

  return (
    <>
      {row.kind === "model" ? <StaleIdentity row={row} /> : <NameIdentity row={row} />}
      <div className="mt-2 flex flex-wrap items-center gap-2 text-caption text-faint">
        {row.kind === "model" ? (
          row.entry && controls?.editable ? (
            <Button variant="quiet" onClick={() => onHide(row.ref)} ariaLabel={`Hide ${row.ref}`}>
              hide
            </Button>
          ) : null
        ) : (
          <Link
            to={row.kind === "alias" ? "/routing/aliases" : "/routing/combos"}
            className="inline-flex h-8 items-center rounded-full px-2 text-small text-accent-text underline underline-offset-2"
          >
            edit {row.kind}
          </Link>
        )}
      </div>
      {controlsRow}
    </>
  );
}

function CardControls({
  row,
  controls,
  move,
}: {
  row: UnifiedRow;
  controls: LayoutControls;
  move?: { up: boolean; down: boolean; onMove: (delta: number) => void };
}) {
  const customGroups = controls.groups.filter((group) => !group.provider);
  const explicit = customGroups.find((group) => group.id === controls.groupOf(row.ref));
  return (
    <div className="mt-2 flex flex-wrap items-center gap-3 border-t border-line pt-2">
      {controls.editable ? (
        <div className="w-40">
          <Select
            ariaLabel={`Group for ${row.ref}`}
            value={explicit?.id ?? ""}
            onChange={(next) => controls.onAssignGroup(row.ref, next === "" ? null : next)}
          >
            <option value="">Ungrouped</option>
            {customGroups.map((group) => (
              <option key={group.id} value={group.id}>
                {group.name}
              </option>
            ))}
          </Select>
        </div>
      ) : (
        <span className="text-caption text-muted">group · {explicit?.name ?? "ungrouped"}</span>
      )}
      {controls.editable ? (
        <span className="flex items-center gap-1.5 text-caption text-muted">
          <Toggle
            size="sm"
            checked={
              row.status !== "disabled" &&
              (row.entry?.visible ?? row.status === "routed")
            }
            disabled={row.status === "disabled"}
            label={`List ${row.ref} on /v1/models`}
            onChange={(next) => controls.onVisible(row.ref, next)}
          />
          /v1
        </span>
      ) : (
        <Badge
          tone={
            row.status !== "disabled" && (row.entry?.visible ?? row.status === "routed")
              ? "neutral"
              : "warn"
          }
        >
          {row.status !== "disabled" && (row.entry?.visible ?? row.status === "routed")
            ? "listed"
            : "hidden"}
        </Badge>
      )}
      {move ? (
        <span className="ml-auto flex items-center gap-1">
          <Button
            variant="quiet"
            size="lg"
            ariaLabel={`Move ${row.ref} up`}
            onClick={() => move.onMove(-1)}
            disabled={!move.up}
          >
            ↑
          </Button>
          <Button
            variant="quiet"
            size="lg"
            ariaLabel={`Move ${row.ref} down`}
            onClick={() => move.onMove(1)}
            disabled={!move.down}
          >
            ↓
          </Button>
        </span>
      ) : null}
    </div>
  );
}

export function StaleIdentity({ row }: { row: UnifiedRow }) {
  return (
    <div className="flex min-w-0 flex-col">
      <span className="truncate font-mono text-small text-faint">{row.ref}</span>
      <span className="mt-0.5 flex flex-wrap gap-1">
        <Badge tone="warn" title="no connection lists this model right now">
          unavailable
        </Badge>
        {row.status === "hidden" ? <Badge tone="neutral">hidden</Badge> : null}
      </span>
    </div>
  );
}

export function NameIdentity({ row }: { row: AliasRow | ComboRow }) {
  return (
    <div className="flex min-w-0 flex-col">
      <div className="flex min-w-0 items-center gap-1.5">
        <span className={CHIP}>rrouter</span>
        <span className="min-w-0 truncate font-mono text-small text-fg">{row.name}</span>
        <CopyIconButton value={row.name} label={`Copy ${row.kind} ${row.name}`} />
        <Badge tone="neutral">{row.kind}</Badge>
        {row.status === "hidden" ? <Badge tone="warn">hidden</Badge> : null}
      </div>
      <span className="mt-0.5 min-w-0 truncate text-caption text-faint">{nameDetail(row)}</span>
    </div>
  );
}
