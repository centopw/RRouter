import {
  createContext,
  useCallback,
  useContext,
  useId,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { Link } from "react-router-dom";
import { Badge, Button, CopyIconButton, Select, cx } from "../../components/ui";
import { Toggle } from "../../components/atoms";
import type { Column } from "../../components/table";
import { DragHandle, DragPreview, SortableLayout, useRowDrag } from "../../components/sortable-list";
import type { LayoutRow } from "../../components/sortable-list";
import { compact } from "../../lib/format";
import type { Model } from "../../lib/api";
import type { DraftGroup } from "../../lib/layout-draft";
import { GroupHeading } from "./groups";
import { ModelCardBody, priceLabel } from "./rows";
import {
  STRATEGY_LABEL,
  nameDetail,
  type AliasRow,
  type ComboRow,
  type UnifiedRow,
} from "./unified-rows";

const CHIP =
  "shrink-0 rounded-full border border-line bg-surface-2 px-1.5 py-px font-mono text-[10px] uppercase tracking-[0.06em] text-muted";

/**
 * A member disclosure is local page state keyed by the combo ref: it survives a status or
 * provider filter, and it is shared by the desktop table and the phone cards so the same
 * combo is open in both. The registry index is carried alongside so a member can be
 * resolved without threading it through every row component.
 */
type UnifiedRowContextValue = {
  expanded: ReadonlySet<string>;
  toggle: (ref: string) => void;
  modelByRef: ReadonlyMap<string, Model>;
};

const NO_MODELS: ReadonlyMap<string, Model> = new Map();
const UnifiedRowContext = createContext<UnifiedRowContextValue>({
  expanded: new Set(),
  toggle: () => {},
  modelByRef: NO_MODELS,
});

export function UnifiedRowProvider({
  modelByRef,
  children,
}: {
  modelByRef: ReadonlyMap<string, Model>;
  children: ReactNode;
}) {
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  const toggle = useCallback((ref: string) => {
    setExpanded((previous) => {
      const next = new Set(previous);
      if (next.has(ref)) next.delete(ref);
      else next.add(ref);
      return next;
    });
  }, []);
  const value = useMemo(
    () => ({ expanded, toggle, modelByRef }),
    [expanded, toggle, modelByRef],
  );
  return <UnifiedRowContext.Provider value={value}>{children}</UnifiedRowContext.Provider>;
}

function membersId(ref: string): string {
  return `combo-members-${ref.replace(/[^a-zA-Z0-9_-]/g, "-")}`;
}

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
                  column.align === "right" ? "text-right" : "text-left",
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
                span={columns.length + 1}
                reducedMotion={reducedMotion}
              />
            ) : (
              <StaticRow key={row.ref} row={row} columns={columns} span={columns.length} />
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
  span,
  reducedMotion,
}: {
  row: UnifiedRow;
  columns: Array<Column<UnifiedRow>>;
  span: number;
  reducedMotion: boolean;
}) {
  const drag = useRowDrag(row.ref, reducedMotion);
  const expanded = useContext(UnifiedRowContext).expanded;
  return (
    <>
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
      {row.kind === "combo" ? (
        <tr hidden={!expanded.has(row.ref)}>
          <td
            colSpan={span}
            className="border-b border-[var(--color-rule)] bg-surface-2 px-4 py-3"
          >
            <ComboMemberList row={row} />
          </td>
        </tr>
      ) : null}
    </>
  );
}

function StaticRow({
  row,
  columns,
  span,
}: {
  row: UnifiedRow;
  columns: Array<Column<UnifiedRow>>;
  span: number;
}) {
  const expanded = useContext(UnifiedRowContext).expanded;
  return (
    <>
      <tr className="group">
        <RowCells row={row} columns={columns} />
      </tr>
      {row.kind === "combo" ? (
        <tr hidden={!expanded.has(row.ref)}>
          <td
            colSpan={span}
            className="border-b border-[var(--color-rule)] bg-surface-2 px-4 py-3"
          >
            <ComboMemberList row={row} />
          </td>
        </tr>
      ) : null}
    </>
  );
}

/**
 * The ordered stored candidates, joined to the registry for whatever it can prove. A ref
 * the registry no longer lists keeps its position and all metadata cells read `—`; it is
 * never dropped from the chain. Aggregate combo metadata is Phase 2, so nothing here is
 * summed, minimized or presented as one model's spec.
 */
function ComboMemberList({ row }: { row: ComboRow }) {
  const { modelByRef } = useContext(UnifiedRowContext);
  const headingId = useId();
  const strategy = row.combo.strategy ?? "fallback";
  const candidates = row.combo.models;
  const order =
    strategy === "fallback"
      ? "Fallback tries the candidates in this order."
      : strategy === "round_robin"
        ? "Round robin rotates the starting candidate; the rest keep this fallback order."
        : `Smart routing asks ${row.combo.router_model ?? "an unknown router model"} to choose the starting candidate; the rest keep this fallback order.`;
  return (
    <div id={membersId(row.ref)} role="group" aria-labelledby={headingId}>
      <p id={headingId} className="sr-only">
        Members for combo {row.name}
      </p>
      {candidates.length === 0 ? (
        <p className="text-caption text-faint">No candidates stored.</p>
      ) : (
        <ol className="flex flex-col gap-1">
          {candidates.map((ref, index) => {
            const model = modelByRef.get(ref);
            const healthy = model !== undefined && model.enabled && model.available;
            return (
              <li
                key={`${ref}-${index}`}
                className="flex flex-wrap items-baseline gap-x-2 gap-y-1 text-caption"
              >
                <span className="tnum text-faint">{index + 1}.</span>
                <span className="break-all font-mono text-fg">{ref}</span>
                {healthy ? null : <Badge tone="warn">unavailable</Badge>}
                <span className="tnum text-faint">ctx {compact(model?.context)}</span>
                <span className="tnum text-faint">max {compact(model?.max_output)}</span>
                <span className="tnum text-faint">
                  {model && priceLabel(model) ? `${priceLabel(model)} / 1M` : "price —"}
                </span>
              </li>
            );
          })}
        </ol>
      )}
      <p className="mt-1 text-caption text-faint">{order}</p>
    </div>
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
        />
        {controlsRow}
      </>
    );
  }

  return (
    <>
      {row.kind === "model" ? <StaleIdentity row={row} /> : <NameIdentity row={row} />}
      {row.kind === "combo" ? <CardComboMembers row={row} /> : null}
      <div className="mt-2 flex flex-wrap items-center gap-2 text-caption text-faint">
        {controls?.editable ? (
          <Button
            variant="quiet"
            onClick={() =>
              row.entry ? onHide(row.ref) : controls.onVisible(row.ref, false)
            }
            ariaLabel={`Hide ${row.ref} from the active list`}
          >
            hide
          </Button>
        ) : null}
        {row.kind === "model" ? null : (
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

/** The card's half of the combo disclosure: same state, same ordered candidates. */
function CardComboMembers({ row }: { row: ComboRow }) {
  const open = useContext(UnifiedRowContext).expanded.has(row.ref);
  return (
    <div
      hidden={!open}
      className="mt-2 rounded-[var(--radius-input)] border border-line bg-surface-2 px-3 py-2"
    >
      <ComboMemberList row={row} />
    </div>
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
          {explicit && row.entry?.visible ? (
            <span className="mt-1 block truncate text-caption text-faint">
              shown in {explicit.name}
            </span>
          ) : null}
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
  const { expanded, toggle, modelByRef } = useContext(UnifiedRowContext);
  const open = expanded.has(row.ref);
  const members = row.kind === "combo" ? row.combo.models.length : 0;
  const target = row.kind === "alias" ? modelByRef.get(row.alias.target) : undefined;
  const targetLabel =
    row.kind === "alias"
      ? target && target.enabled && target.available
        ? " · target available"
        : " · target unavailable"
      : "";
  return (
    <div className="flex min-w-0 flex-col">
      <div className="flex min-w-0 items-center gap-1.5">
        <span className={CHIP}>rrouter</span>
        <span className="min-w-0 truncate font-mono text-small text-fg">{row.name}</span>
        <CopyIconButton value={row.name} label={`Copy ${row.kind} ${row.name}`} />
        <Badge tone="neutral">{row.kind}</Badge>
        {row.kind === "combo" ? (
          <Badge tone="neutral">{STRATEGY_LABEL[row.combo.strategy ?? "fallback"]}</Badge>
        ) : null}
        {row.status === "hidden" ? <Badge tone="warn">hidden</Badge> : null}
      </div>
      <span className="mt-0.5 min-w-0 truncate text-caption text-faint">
        {nameDetail(row)}
        {targetLabel}
      </span>
      {row.kind === "combo" ? (
        <button
          type="button"
          aria-expanded={open}
          aria-controls={membersId(row.ref)}
          aria-label={`${open ? "Hide" : "Show"} ${members} members for ${row.name}`}
          onClick={() => toggle(row.ref)}
          className="mt-1 inline-flex w-fit items-center rounded-full border border-line bg-surface-2 px-2 py-0.5 text-caption text-muted transition-colors hover:border-line-strong hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
        >
          {open ? "Hide members" : `Show ${members} member${members === 1 ? "" : "s"}`}
        </button>
      ) : null}
    </div>
  );
}
