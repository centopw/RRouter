import {
  createContext,
  memo,
  useCallback,
  useContext,
  useId,
  useMemo,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import { CaretDown, CaretUp } from "@phosphor-icons/react";
import { Link } from "react-router-dom";
import { Badge, Button, CopyIconButton, Select, cx } from "../../components/ui";
import { Toggle } from "../../components/atoms";
import { CAPABILITY_LABELS, CapabilityChips } from "../../components/capability-chips";
import type { Column } from "../../components/table";
import { DragHandle, DragPreview, SortableLayout, useRowDrag } from "../../components/sortable-list";
import type { LayoutRow } from "../../components/sortable-list";
import { compact } from "../../lib/format";
import type { Model } from "../../lib/api";
import type { DraftGroup } from "../../lib/layout-draft";
import type { ModelTestOutcome } from "./actions";
import { GroupHeading } from "./groups";
import { cacheText, isFree, priceText } from "./price";
import {
  FreeBadge,
  ModelCardBody,
  PROVIDER_PILL,
  RowPreview,
} from "./rows";
import {
  STRATEGY_LABEL,
  aggregateOf,
  comboBlocked,
  comboCapabilitySplit,
  comboPriceText,
  membersOf,
  nameDetail,
  qualifiedTokenRange,
  rowIsFree,
  type AliasRow,
  type ComboMember,
  type ComboRow,
  type UnifiedRow,
} from "./unified-rows";

/**
 * Split contexts so toggling one combo disclosure never re-renders unrelated model rows:
 * `UnifiedRowDataContext` changes only when `/api/models` or `isAdmin` changes;
 * `UnifiedRowExpandedContext` is subscribed only by combo rows and `NameIdentity`.
 */
type UnifiedRowDataValue = {
  modelByRef: ReadonlyMap<string, Model>;
  isAdmin: boolean;
  toggle: (ref: string) => void;
};

const NO_MODELS: ReadonlyMap<string, Model> = new Map();
const UnifiedRowDataContext = createContext<UnifiedRowDataValue>({
  modelByRef: NO_MODELS,
  isAdmin: false,
  toggle: () => {},
});
const UnifiedRowExpandedContext = createContext<ReadonlySet<string>>(new Set());

export function UnifiedRowProvider({
  modelByRef,
  isAdmin = false,
  children,
}: {
  modelByRef: ReadonlyMap<string, Model>;
  isAdmin?: boolean;
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
  const dataValue = useMemo(
    () => ({ modelByRef, isAdmin, toggle }),
    [modelByRef, isAdmin, toggle],
  );
  return (
    <UnifiedRowDataContext.Provider value={dataValue}>
      <UnifiedRowExpandedContext.Provider value={expanded}>
        {children}
      </UnifiedRowExpandedContext.Provider>
    </UnifiedRowDataContext.Provider>
  );
}

function membersId(ref: string): string {
  return `combo-members-${ref.replace(/[^a-zA-Z0-9_-]/g, "-")}`;
}

/** The registry mutations a model row reaches; the page owns the hooks behind them. */
export type ModelActions = {
  isAdmin: boolean;
  testTarget: string | null;
  testResults?: ReadonlyMap<string, ModelTestOutcome>;
  toggleTarget: string | null;
  removeTarget: string | null;
  onTest: (model: Model) => void;
  onToggle: (model: Model) => void;
  onEdit: (model: Model) => void;
  onDelete: (model: Model) => void;
  /** Hide/restore is an entry operation; absent on rows the preset never stored. */
  onHide: (ref: string) => void;
  onMoveTop?: (ref: string) => void;
  onMoveBottom?: (ref: string) => void;
  canMoveTop?: (ref: string) => boolean;
  canMoveBottom?: (ref: string) => boolean;
};

/** Group and `/v1` are draft writes; absent when no preset is loaded for this caller. */
export type LayoutControls = {
  editable: boolean;
  groups: Array<DraftGroup & { implicit?: boolean }>;
  groupOf: (ref: string) => string;
  onAssignGroup: (ref: string, group: string | null) => void;
  onVisible: (ref: string, visible: boolean) => void;
};

/**
 * Off-screen layout deferral for below-the-fold board sections (`index > 0`, never applied
 * to the initial viewport section or while dragging). Paired with `containIntrinsicSize`
 * so the scrollbar never jumps.
 */
const DEFER_SECTION_STYLE: CSSProperties = {
  contentVisibility: "auto",
  containIntrinsicSize: "auto none auto 280px",
};

/** One routed group: heading, drag-orderable table on desktop, cards on a phone. */
export const BoardSection = memo(function BoardSection({
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
  deferLayout = false,
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
  /** Defer off-screen paint for below-the-fold sections when drag is inactive. */
  deferLayout?: boolean;
}) {
  return (
    <section
      aria-label={group.name}
      style={deferLayout && !drag ? DEFER_SECTION_STYLE : undefined}
    >
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
});

/**
 * Flat desktop table for non-board views (readers, `Status: all/hidden/disabled/unrouted`).
 * Uses the same `StaticRow` and `ComboMemberList` as the board so multi-line identity cells
 * never clip and combo member disclosures work in every filter state.
 */
export const FlatTable = memo(function FlatTable({
  caption,
  rows,
  columns,
}: {
  caption: string;
  rows: UnifiedRow[];
  columns: Array<Column<UnifiedRow>>;
}) {
  return (
    <div className="table-scroll overflow-x-auto" role="region" aria-label={caption} tabIndex={0}>
      <table className="w-full table-fixed border-separate border-spacing-0 text-body">
        <caption className="sr-only">{caption}</caption>
        <colgroup>
          {columns.map((column) => (
            <col key={column.key} style={column.width ? { width: column.width } : undefined} />
          ))}
        </colgroup>
        <thead className="sticky top-0 z-20">
          <tr>
            {columns.map((column) => (
              <th
                key={column.key}
                scope="col"
                style={column.width ? { width: column.width } : undefined}
                className={cx(
                  "sticky top-0 z-20 border-b border-line bg-surface px-4 py-2.5 text-small font-semibold text-ink-soft",
                  column.align === "right" ? "text-right" : "text-left",
                  column.key === "actions" &&
                    "sticky right-0 z-30 border-l border-line bg-surface shadow-[var(--shadow-sticky-col)]",
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
          {rows.map((row) => (
            <StaticRow key={row.ref} row={row} columns={columns} span={columns.length} />
          ))}
        </tbody>
      </table>
    </div>
  );
});

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
  const layoutRows = useMemo<LayoutRow[]>(
    () =>
      rows.map((row) => ({
        ref: row.ref,
        model: row.kind === "model" ? row.model : null,
        visible: row.entry?.visible ?? true,
        isNew: false,
      })),
    [rows],
  );
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
                    "sticky right-0 z-30 border-l border-line bg-surface shadow-[var(--shadow-sticky-col)]",
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
            "sticky right-0 z-10 border-l border-line bg-surface shadow-[var(--shadow-sticky-col)] group-hover:bg-[var(--color-row-hover)]",
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

/** Mounted only for combo rows so model rows never subscribe to `expanded`. */
function ComboDetailTableRow({ row, span }: { row: ComboRow; span: number }) {
  const open = useContext(UnifiedRowExpandedContext).has(row.ref);
  if (!open) return null;
  return (
    <tr>
      <td
        colSpan={span}
        className="border-b border-[var(--color-rule)] bg-surface-2 px-4 py-3"
      >
        <ComboMemberList row={row} />
      </td>
    </tr>
  );
}

const SortableRow = memo(function SortableRow({
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
      {row.kind === "combo" ? <ComboDetailTableRow row={row} span={span} /> : null}
    </>
  );
});

const StaticRow = memo(function StaticRow({
  row,
  columns,
  span,
}: {
  row: UnifiedRow;
  columns: Array<Column<UnifiedRow>>;
  span: number;
}) {
  return (
    <>
      <tr className="group">
        <RowCells row={row} columns={columns} />
      </tr>
      {row.kind === "combo" ? <ComboDetailTableRow row={row} span={span} /> : null}
    </>
  );
});

function memberStateBadge(state: ComboMember["state"]): ReactNode {
  if (state === "exact") return null;
  if (state === "fanout") return <Badge tone="info">fan-out</Badge>;
  if (state === "core") return <Badge tone="info">version match</Badge>;
  if (state === "disabled") return <Badge tone="neutral">disabled</Badge>;
  return <Badge tone="warn">unknown</Badge>;
}

/**
 * The ordered stored candidates. When `/api/combos` sends `members[]`, every resolved
 * target (including fan-out and version-core matches) renders with its real context,
 * max output, unified `/1M` price (or `Free` badge), and capabilities. Older gateways
 * fall back to joining `models[]` against `modelByRef`.
 */
function ComboMemberList({ row }: { row: ComboRow }) {
  const { modelByRef, isAdmin } = useContext(UnifiedRowDataContext);
  const headingId = useId();
  const strategy = row.combo.strategy ?? "fallback";
  const aggregate = aggregateOf(row.combo);
  const wireMembers = membersOf(row.combo);
  const candidates = row.combo.models;
  const order =
    strategy === "fallback"
      ? "Fallback tries the candidates in this order. Combo ranges describe the candidate set, not one model's spec."
      : strategy === "round_robin"
        ? "Round robin rotates the starting candidate; the rest keep this fallback order."
        : `Smart routing asks ${row.combo.router_model ?? "an unknown router model"} to pick the first candidate; candidate pricing excludes the extra router call.`;

  return (
    <div id={membersId(row.ref)} role="group" aria-labelledby={headingId}>
      <p id={headingId} className="sr-only">
        Members for combo {row.name}
      </p>
      {wireMembers ? (
        wireMembers.length === 0 ? (
          <p className="text-caption text-faint">No candidates stored.</p>
        ) : (
          <ol className="flex flex-col gap-1.5">
            {wireMembers.map((member, index) => {
              const targets = member.targets.length > 0 ? member.targets : member.model ? [member.model] : [];
              return (
                <li key={`${member.ref}-${index}`} className="flex flex-col gap-1 text-caption">
                  <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                    <span className="tnum text-faint">{index + 1}.</span>
                    <span className="break-all font-mono text-fg">{member.ref}</span>
                    {member.via_alias ? (
                      <span className="text-faint">via alias {member.via_alias}</span>
                    ) : null}
                    {memberStateBadge(member.state)}
                    {targets.length === 1 && targets[0] ? (
                      <MemberTargetSummary target={targets[0]} isAdmin={isAdmin} />
                    ) : targets.length === 0 ? (
                      <>
                        <span className="tnum text-faint">ctx —</span>
                        <span className="tnum text-faint">max —</span>
                        <span className="tnum text-faint">price —</span>
                      </>
                    ) : (
                      <span className="text-faint tnum">{targets.length} targets</span>
                    )}
                  </div>
                  {targets.length > 1 ? (
                    <ul className="ml-5 flex flex-col gap-1 border-l border-line pl-2.5">
                      {targets.map((target) => (
                        <li
                          key={target.ref}
                          className="flex flex-wrap items-center gap-x-2 gap-y-1 text-caption"
                        >
                          <span className="break-all font-mono text-muted">{target.ref}</span>
                          <MemberTargetSummary target={target} isAdmin={isAdmin} />
                        </li>
                      ))}
                    </ul>
                  ) : null}
                </li>
              );
            })}
          </ol>
        )
      ) : candidates.length === 0 ? (
        <p className="text-caption text-faint">No candidates stored.</p>
      ) : (
        <ol className="flex flex-col gap-1.5">
          {candidates.map((ref, index) => {
            const model = modelByRef.get(ref);
            return (
              <li
                key={`${ref}-${index}`}
                className="flex flex-wrap items-center gap-x-2 gap-y-1 text-caption"
              >
                <span className="tnum text-faint">{index + 1}.</span>
                <span className="break-all font-mono text-fg">{ref}</span>
                {model ? (
                  <MemberTargetSummary target={model} isAdmin={isAdmin} />
                ) : (
                  <>
                    <Badge tone="warn">unavailable</Badge>
                    <span className="tnum text-faint">ctx —</span>
                    <span className="tnum text-faint">max —</span>
                    <span className="tnum text-faint">price —</span>
                  </>
                )}
              </li>
            );
          })}
        </ol>
      )}
      {strategy === "smart" && aggregate?.router ? (
        <p className="mt-1.5 text-caption text-muted">
          Router: <span className="font-mono text-fg">{aggregate.router.ref}</span>
          {aggregate.router.model && aggregate.router.model.enabled && aggregate.router.model.available
            ? ` · ${priceText(aggregate.router.model.cost)}`
            : " · unavailable"}
        </p>
      ) : null}
      <p className="mt-1.5 text-caption text-faint">{order}</p>
    </div>
  );
}

function MemberTargetSummary({ target, isAdmin }: { target: Model; isAdmin: boolean }) {
  const free = isFree(target.cost);
  return (
    <>
      {!target.enabled ? (
        <Badge tone="neutral">disabled</Badge>
      ) : !target.available ? (
        <Badge tone="warn">unavailable</Badge>
      ) : null}
      {target.deprecated ? <Badge tone="warn">deprecated</Badge> : null}
      {free ? <FreeBadge linked={isAdmin} /> : null}
      <span className="tnum text-faint">ctx {compact(target.context)}</span>
      <span className="tnum text-faint">max {compact(target.max_output)}</span>
      {free ? null : <span className="tnum text-faint">{priceText(target.cost)}</span>}
      <CapabilityChips capabilities={target.capabilities} max={2} subject={target.ref} />
    </>
  );
}

/** Phone rows for a filtered table: one card per row, no drag, no group header. */
export const UnifiedCards = memo(function UnifiedCards({
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
});

const UnifiedCard = memo(function UnifiedCard({
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
          testOutcome={actions.testResults?.get(row.model.ref)}
          toggleTarget={actions.toggleTarget}
          removeTarget={actions.removeTarget}
          onTest={actions.onTest}
          onToggle={actions.onToggle}
          onEdit={actions.onEdit}
          onDelete={actions.onDelete}
          onHide={
            row.entry && controls?.editable ? (ref) => onHide(ref) : undefined
          }
          onMoveTop={actions.onMoveTop ? () => actions.onMoveTop!(row.ref) : undefined}
          onMoveBottom={actions.onMoveBottom ? () => actions.onMoveBottom!(row.ref) : undefined}
          canMoveTop={actions.canMoveTop ? actions.canMoveTop(row.ref) : undefined}
          canMoveBottom={actions.canMoveBottom ? actions.canMoveBottom(row.ref) : undefined}
          groups={controls?.editable ? controls.groups : undefined}
          currentGroup={controls?.groupOf(row.ref)}
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
          <>
            <Link
              to={`/playground/chat?model=${encodeURIComponent(row.name)}`}
              className="inline-flex h-8 items-center rounded-full px-2 text-small text-accent-text underline underline-offset-2"
            >
              Test in Playground →
            </Link>
            <Link
              to={`${row.kind === "alias" ? "/routing/aliases" : "/routing/combos"}?edit=${encodeURIComponent(row.name)}`}
              className="inline-flex h-8 items-center rounded-full px-2 text-small text-accent-text underline underline-offset-2"
            >
              edit {row.kind}
            </Link>
          </>
        )}
      </div>
      {controlsRow}
    </>
  );
});

/** The card's half of the combo disclosure: lazy-mounted while collapsed. */
function CardComboMembers({ row }: { row: ComboRow }) {
  const open = useContext(UnifiedRowExpandedContext).has(row.ref);
  if (!open) return null;
  return (
    <div className="mt-2 rounded-[var(--radius-input)] border border-line bg-surface-2 px-3 py-2">
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

function NamePreviewBody({
  row,
  modelByRef,
}: {
  row: AliasRow | ComboRow;
  modelByRef: ReadonlyMap<string, Model>;
}) {
  if (row.kind === "alias") {
    const target = modelByRef.get(row.alias.target);
    const available = target !== undefined && target.enabled && target.available;
    const cache = cacheText(target?.cost);
    return (
      <div className="flex flex-col gap-1.5">
        <div className="break-all font-mono text-small font-medium text-fg">
          {row.name} → {row.alias.target}
        </div>
        <div className={available ? "text-[var(--color-pos-ink)] font-medium" : "text-[var(--color-warn-ink)] font-medium"}>
          {available
            ? "Target available right now"
            : target && !target.enabled
              ? "Target is disabled globally"
              : "Target unavailable — not connected"}
        </div>
        {target ? (
          <>
            <div className="flex flex-wrap gap-x-3 text-muted tnum">
              <span>Context {compact(target.context)}</span>
              <span>Max output {compact(target.max_output)}</span>
            </div>
            <div className="text-fg tnum">
              {isFree(target.cost) ? "Free · $0.00 /1M" : priceText(target.cost)}
              {cache ? <span className="block text-faint">{cache}</span> : null}
            </div>
          </>
        ) : null}
      </div>
    );
  }

  const aggregate = aggregateOf(row.combo);
  const blocked = comboBlocked(aggregate);
  const ctx = qualifiedTokenRange(aggregate?.context, "context");
  const maxOut = qualifiedTokenRange(aggregate?.max_output, "max output");
  const price = comboPriceText(aggregate);
  const caps = comboCapabilitySplit(aggregate);
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center gap-1.5">
        <span className="break-all font-mono text-small font-medium text-fg">{row.name}</span>
        <span className="text-muted">({STRATEGY_LABEL[row.combo.strategy ?? "fallback"]})</span>
      </div>
      <div className={blocked ? "text-[var(--color-warn-ink)] font-medium" : "text-[var(--color-pos-ink)] font-medium"}>
        {blocked ?? "Ready to route"}
      </div>
      <div className="flex flex-col gap-0.5 text-muted tnum">
        <span>{ctx ?? "Context —"}</span>
        <span>{maxOut ?? "Max output —"}</span>
      </div>
      <div className="text-fg tnum">{price ?? "Metadata —"}</div>
      {caps.known ? (
        <div className="flex flex-col gap-0.5 text-muted">
          {caps.guaranteed.length > 0 ? (
            <span>
              All candidates: {caps.guaranteed.map((k) => CAPABILITY_LABELS[k].chip).join(", ")}
            </span>
          ) : null}
          {caps.someOnly.length > 0 ? (
            <span>
              Some candidates: {caps.someOnly.map((k) => CAPABILITY_LABELS[k].chip).join(", ")}
            </span>
          ) : null}
        </div>
      ) : null}
      {row.combo.models.length > 0 ? (
        <div className="border-t border-line pt-1.5 text-muted">
          <div className="mb-0.5 text-faint">Candidates ({row.combo.models.length}):</div>
          <div className="font-mono text-fg">{row.combo.models.slice(0, 5).join(" → ")}{row.combo.models.length > 5 ? ` (+${row.combo.models.length - 5} more)` : ""}</div>
        </div>
      ) : null}
    </div>
  );
}

export function NameIdentity({ row }: { row: AliasRow | ComboRow }) {
  const { modelByRef, isAdmin, toggle } = useContext(UnifiedRowDataContext);
  const expanded = useContext(UnifiedRowExpandedContext);
  const open = row.kind === "combo" && expanded.has(row.ref);
  const members = row.kind === "combo" ? row.combo.models.length : 0;
  const free = rowIsFree(row, modelByRef);

  const target = row.kind === "alias" ? modelByRef.get(row.alias.target) : undefined;
  const targetAvailable =
    row.kind === "alias" ? Boolean(target && target.enabled && target.available) : true;

  const aggregate = row.kind === "combo" ? aggregateOf(row.combo) : null;
  const blocked = row.kind === "combo" ? comboBlocked(aggregate) : null;
  const capSplit = row.kind === "combo" ? comboCapabilitySplit(aggregate) : null;
  const deprecatedCount = aggregate?.deprecated_members ?? 0;

  return (
    <div className="flex min-w-0 flex-col">
      <div className="flex min-w-0 flex-wrap items-center gap-1.5">
        <span className={PROVIDER_PILL}>rrouter</span>
        <RowPreview
          label={`Details for ${row.kind} ${row.name}`}
          trigger={
            <span className="min-w-0 break-all font-mono text-small text-fg">{row.name}</span>
          }
          content={<NamePreviewBody row={row} modelByRef={modelByRef} />}
        />
        <CopyIconButton value={row.name} label={`Copy ${row.kind} ${row.name}`} />
        <Badge tone="neutral">{row.kind}</Badge>
        {row.kind === "combo" ? (
          <Badge tone="neutral">{STRATEGY_LABEL[row.combo.strategy ?? "fallback"]}</Badge>
        ) : null}
        {free ? <FreeBadge linked={isAdmin} /> : null}
        {row.kind === "alias" && !targetAvailable ? (
          <Badge tone="warn" title="The target model is disabled or not connected">
            target unavailable
          </Badge>
        ) : null}
        {blocked ? (
          <Badge tone="warn" title={blocked}>
            {blocked}
          </Badge>
        ) : null}
        {deprecatedCount > 0 ? (
          <Badge tone="warn" title="One or more candidate models are retired by their provider">
            {aggregate?.all_deprecated ? "all deprecated" : `${deprecatedCount} deprecated`}
          </Badge>
        ) : null}
        {row.status === "hidden" ? <Badge tone="warn">hidden</Badge> : null}
      </div>
      <div className="mt-0.5 flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5 text-caption text-faint tnum">
        <span className="min-w-0 break-all">{nameDetail(row)}</span>
        {row.kind === "alias" && target ? (
          <>
            <span className="whitespace-nowrap">ctx {compact(target.context)}</span>
            {free ? null : <span className="whitespace-nowrap">{priceText(target.cost)}</span>}
          </>
        ) : null}
        {capSplit?.known && (capSplit.guaranteed.length > 0 || capSplit.someOnly.length > 0) ? (
          <span className="flex flex-wrap items-center gap-1">
            {capSplit.guaranteed.slice(0, 2).map((key) => (
              <Badge key={`all-${key}`} tone="neutral" title={`All candidates support ${CAPABILITY_LABELS[key].title}`}>
                {CAPABILITY_LABELS[key].chip}: all
              </Badge>
            ))}
            {capSplit.someOnly.slice(0, 1).map((key) => (
              <Badge key={`some-${key}`} tone="neutral" title={`Some candidates support ${CAPABILITY_LABELS[key].title}`}>
                {CAPABILITY_LABELS[key].chip}: some
              </Badge>
            ))}
          </span>
        ) : null}
      </div>
      {row.kind === "combo" ? (
        <button
          type="button"
          aria-expanded={open}
          aria-controls={open ? membersId(row.ref) : undefined}
          aria-label={`${open ? "Hide" : "Show"} ${members} members for ${row.name}`}
          onClick={() => toggle(row.ref)}
          className="mt-1 inline-flex w-fit items-center gap-1 rounded-full border border-line bg-surface-2 px-2 py-0.5 text-caption text-muted transition-colors hover:border-line-strong hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
        >
          {open ? <CaretUp size={11} aria-hidden /> : <CaretDown size={11} aria-hidden />}
          {open ? "Hide members" : `Show ${members} member${members === 1 ? "" : "s"}`}
        </button>
      ) : null}
    </div>
  );
}
