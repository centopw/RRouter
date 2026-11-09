import { useMemo, useState, type CSSProperties, type ReactNode } from "react";
import {
  DndContext,
  DragOverlay,
  KeyboardSensor,
  PointerSensor,
  TouchSensor,
  closestCenter,
  useSensor,
  useSensors,
  type Announcements,
  type DragEndEvent,
  type DragStartEvent,
  type DraggableAttributes,
  type DraggableSyntheticListeners,
  type ScreenReaderInstructions,
} from "@dnd-kit/core";
import {
  SortableContext,
  sortableKeyboardCoordinates,
  useSortable,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { restrictToParentElement, restrictToVerticalAxis } from "@dnd-kit/modifiers";
import { DotsSixVertical, EyeSlash } from "@phosphor-icons/react";
import { Badge, Button, EmptyState, cx } from "./ui";
import type { Column } from "./table";
import type { Model } from "../lib/api";

/**
 * The sortable face of a preset layout.
 *
 * dnd-kit owns the drag: pointer, touch (delayed, so a scroll swipe is not a drag) and
 * keyboard (space to lift, arrows to move, space to drop, escape to cancel) sensors all
 * feed one `onReorder(ref, overRef)`. Rows are real table rows so the registry columns and
 * the sticky actions cell keep working; dnd-kit needs a ref per row, which the shared
 * `DataTable` cannot hand out.
 */

/** A row is a registry model, or an entry whose model no connection lists (unavailable). */
export type LayoutRow = {
  ref: string;
  model: Model | null;
  visible: boolean;
  /** Registry models the preset has never stored. */
  isNew: boolean;
};

const INSTRUCTIONS: ScreenReaderInstructions = {
  draggable:
    "Press space to pick up a model, arrow keys to move it, space to drop it, escape to cancel.",
};

export type RowDrag = {
  attributes: DraggableAttributes;
  listeners: DraggableSyntheticListeners;
  setNodeRef: (node: HTMLElement | null) => void;
  setActivatorNodeRef: (node: HTMLElement | null) => void;
  isDragging: boolean;
  style: CSSProperties;
};

/** The per-row drag state, shared by the table and the phone cards. */
export function useRowDrag(ref: string, reducedMotion: boolean): RowDrag {
  const { attributes, listeners, setNodeRef, setActivatorNodeRef, transform, transition, isDragging } =
    useSortable({ id: ref });
  return {
    attributes,
    listeners,
    setNodeRef,
    setActivatorNodeRef,
    isDragging,
    style: {
      transform: CSS.Translate.toString(transform),
      // The lift animation is the one thing that moves on its own; drop it for reduced motion.
      transition: reducedMotion ? undefined : transition,
    },
  };
}

export function SortableLayout({
  rows,
  onReorder,
  reducedMotion,
  overlay,
  empty,
  children,
}: {
  rows: LayoutRow[];
  onReorder: (from: string, to: string) => void;
  reducedMotion: boolean;
  /** What the drag preview shows; the list itself renders from `children`. */
  overlay: (row: LayoutRow) => ReactNode;
  empty?: ReactNode;
  children: ReactNode;
}) {
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 6 } }),
    useSensor(TouchSensor, { activationConstraint: { delay: 180, tolerance: 8 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );
  const [dragging, setDragging] = useState<string | null>(null);
  const position = (ref: string | number | null) =>
    rows.findIndex((row) => row.ref === ref) + 1;

  const announcements: Announcements = useMemo(
    () => ({
      onDragStart: ({ active }) => `Picked up ${active.id}.`,
      onDragOver: ({ active, over }) =>
        over
          ? `${active.id} is over position ${position(over.id)} of ${rows.length}.`
          : `${active.id} is outside the list.`,
      onDragEnd: ({ active, over }) =>
        over
          ? `Moved ${active.id} to position ${position(over.id)} of ${rows.length}.`
          : `${active.id} went back to its place.`,
      onDragCancel: ({ active }) => `Cancelled; ${active.id} stayed where it was.`,
    }),
    // `position` closes over rows, so rows is the only dependency that matters.
    [rows],
  );

  const onDragEnd = (event: DragEndEvent) => {
    setDragging(null);
    const { active, over } = event;
    if (over && active.id !== over.id) onReorder(String(active.id), String(over.id));
  };

  if (rows.length === 0) return <>{empty ?? <EmptyState>nothing here</EmptyState>}</>;
  const overlayRow = dragging ? rows.find((row) => row.ref === dragging) : undefined;

  return (
    <DndContext
      sensors={sensors}
      collisionDetection={closestCenter}
      modifiers={[restrictToVerticalAxis, restrictToParentElement]}
      accessibility={{ announcements, screenReaderInstructions: INSTRUCTIONS }}
      onDragStart={(event: DragStartEvent) => setDragging(String(event.active.id))}
      onDragEnd={onDragEnd}
      onDragCancel={() => setDragging(null)}
    >
      <SortableContext items={rows.map((row) => row.ref)} strategy={verticalListSortingStrategy}>
        {children}
      </SortableContext>
      <DragOverlay dropAnimation={reducedMotion ? null : undefined}>
        {overlayRow ? overlay(overlayRow) : null}
      </DragOverlay>
    </DndContext>
  );
}

/** The drag preview for both list shapes. */
export function DragPreview({ row }: { row: LayoutRow }) {
  return (
    <div className="flex items-center gap-2 rounded-[var(--radius-card)] border border-line-strong bg-surface px-3 py-2 shadow-lg">
      <DotsSixVertical size={14} aria-hidden className="text-faint" />
      <span className="truncate font-mono text-small">{row.ref}</span>
      {row.model?.cost ? (
        <span className="text-caption text-muted tnum">
          ${row.model.cost.input.toFixed(2)} / ${row.model.cost.output.toFixed(2)}
        </span>
      ) : null}
    </div>
  );
}

/** The handle: a 28 px target that is the only drag activator, so row clicks still work. */
export function DragHandle({
  label,
  drag,
  disabled,
}: {
  label: string;
  drag: RowDrag;
  disabled?: boolean;
}) {
  return (
    <button
      ref={drag.setActivatorNodeRef}
      type="button"
      aria-label={label}
      {...drag.attributes}
      {...drag.listeners}
      disabled={disabled}
      className="flex h-8 w-7 cursor-grab items-center justify-center rounded-[var(--radius-input)] text-faint transition-colors hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text active:cursor-grabbing disabled:cursor-not-allowed disabled:opacity-40"
    >
      <DotsSixVertical size={16} aria-hidden />
    </button>
  );
}

/** Desktop active list: the registry columns plus a leading handle column. */
export function SortableTable({
  rows,
  columns,
  onReorder,
  onHide,
  reducedMotion,
  canHide,
  scrollLabel,
  maxBodyHeight,
  empty,
}: {
  rows: LayoutRow[];
  columns: Array<Column<Model>>;
  onReorder: (from: string, to: string) => void;
  onHide: (ref: string) => void;
  reducedMotion: boolean;
  canHide: boolean;
  scrollLabel: string;
  maxBodyHeight?: string;
  empty?: ReactNode;
}) {
  return (
    <SortableLayout
      rows={rows}
      onReorder={onReorder}
      reducedMotion={reducedMotion}
      overlay={(row) => <DragPreview row={row} />}
      empty={empty}
    >
      <div
        className="table-scroll overflow-auto"
        role="region"
        aria-label={scrollLabel}
        tabIndex={0}
        style={maxBodyHeight ? { maxHeight: maxBodyHeight } : undefined}
      >
        <table className="w-full table-fixed border-separate border-spacing-0 text-body">
          <caption className="sr-only">{scrollLabel}</caption>
          <thead className="sticky top-0 z-20">
            <tr>
              <th scope="col" className="w-10 border-b border-line bg-surface px-1 py-2.5">
                <span className="sr-only">reorder</span>
              </th>
              {columns.map((column) => (
                <th
                  key={column.key}
                  scope="col"
                  style={column.width ? { width: column.width } : undefined}
                  className={cx(
                    "sticky top-0 z-20 border-b border-line bg-surface px-4 py-2.5 text-small font-semibold text-ink-soft",
                    column.align === "right" && "text-right",
                    column.className,
                    column.key === "actions" &&
                      "sticky right-0 z-30 border-l border-line bg-surface shadow-[-8px_0_8px_-8px_rgba(28,43,66,0.16)]",
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
              <SortableTableRow
                key={row.ref}
                row={row}
                columns={columns}
                reducedMotion={reducedMotion}
                onHide={onHide}
                canHide={canHide}
              />
            ))}
          </tbody>
        </table>
      </div>
    </SortableLayout>
  );
}

function SortableTableRow({
  row,
  columns,
  reducedMotion,
  onHide,
  canHide,
}: {
  row: LayoutRow;
  columns: Array<Column<Model>>;
  reducedMotion: boolean;
  onHide: (ref: string) => void;
  canHide: boolean;
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
      {columns.map((column) => {
        const content = row.model
          ? column.render(row.model)
          : column.key === "model"
            ? (
                <div className="flex min-w-0 flex-col">
                  <span className="truncate font-mono text-small text-faint">{row.ref}</span>
                  <span className="mt-0.5 flex flex-wrap gap-1">
                    <Badge tone="warn" title="no connection lists this model right now">
                      unavailable
                    </Badge>
                    {row.isNew ? <Badge tone="accent">new</Badge> : null}
                  </span>
                </div>
              )
            : column.key === "actions"
              ? canHide
                ? (
                    <Button
                      variant="quiet"
                      ariaLabel={`Hide ${row.ref} from the active list`}
                      onClick={() => onHide(row.ref)}
                    >
                      hide
                    </Button>
                  )
                : null
              : <span className="text-faint">—</span>;
        const shared = cx(
          "border-b border-[var(--color-rule)] px-4 py-2.5 align-middle text-fg",
          column.align === "right" && "text-right tnum",
          column.key === "actions" &&
            "sticky right-0 z-10 border-l border-line bg-surface shadow-[-8px_0_8px_-8px_rgba(28,43,66,0.16)] group-hover:bg-[var(--color-row-hover)]",
          column.className,
        );
        if (column.key === "model") {
          return (
            <th key={column.key} scope="row" className={cx(shared, "text-left font-normal group-hover:bg-[var(--color-row-hover)]")}>
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
    </tr>
  );
}

/** Phone active list: one card per model, drag from the handle only. */
export function SortableCards({
  rows,
  onReorder,
  reducedMotion,
  canHide,
  onHide,
  render,
}: {
  rows: LayoutRow[];
  onReorder: (from: string, to: string) => void;
  reducedMotion: boolean;
  canHide: boolean;
  onHide: (ref: string) => void;
  /** The card body the page already knows how to draw for a model. */
  render: (model: Model) => ReactNode;
}) {
  return (
    <SortableLayout
      rows={rows}
      onReorder={onReorder}
      reducedMotion={reducedMotion}
      overlay={(row) => <DragPreview row={row} />}
    >
      <ul aria-label="Models registry" className="flex flex-col">
        {rows.map((row) => (
          <SortableCard
            key={row.ref}
            row={row}
            reducedMotion={reducedMotion}
            canHide={canHide}
            onHide={onHide}
            render={render}
          />
        ))}
      </ul>
    </SortableLayout>
  );
}

function SortableCard({
  row,
  reducedMotion,
  canHide,
  onHide,
  render,
}: {
  row: LayoutRow;
  reducedMotion: boolean;
  canHide: boolean;
  onHide: (ref: string) => void;
  render: (model: Model) => ReactNode;
}) {
  const drag = useRowDrag(row.ref, reducedMotion);
  return (
    <li
      ref={drag.setNodeRef}
      style={drag.style}
      data-dragging={drag.isDragging || undefined}
      className={cx(
        "relative border-b border-[var(--color-rule)] px-4 py-3 last:border-b-0",
        drag.isDragging && "z-10 opacity-70",
      )}
    >
      <div className="absolute left-1 top-2">
        <DragHandle label={`Reorder ${row.ref}`} drag={drag} />
      </div>
      <div className="pl-7">
        {row.model ? (
          render(row.model)
        ) : (
          <span className="flex flex-wrap items-center gap-2">
            <span className="truncate font-mono text-small text-faint">{row.ref}</span>
            <Badge tone="warn" title="no connection lists this model right now">
              unavailable
            </Badge>
          </span>
        )}
        {canHide ? (
          <div className="mt-1 flex justify-end">
            <Button
              variant="quiet"
              ariaLabel={`Hide ${row.ref} from the active list`}
              onClick={() => onHide(row.ref)}
            >
              hide
            </Button>
          </div>
        ) : null}
      </div>
    </li>
  );
}

/** Inactive rows: compact, no columns, one restore each plus a bulk action. */
export function InactiveList({
  rows,
  onRestore,
  onRestoreAll,
  note,
}: {
  rows: LayoutRow[];
  onRestore: (ref: string) => void;
  onRestoreAll: () => void;
  note?: ReactNode;
}) {
  if (rows.length === 0) {
    return <EmptyState>{note ?? "Every model in this preset is visible."}</EmptyState>;
  }
  return (
    <ul aria-label="Hidden models" className="flex flex-col">
      {rows.map((row) => (
        <li
          key={row.ref}
          className="flex flex-wrap items-center justify-between gap-2 border-b border-[var(--color-rule)] px-5 py-2 last:border-b-0"
        >
          <span className="flex min-w-0 flex-wrap items-center gap-2">
            <span className="truncate font-mono text-small text-muted">{row.ref}</span>
            {row.model === null ? (
              <Badge tone="warn" title="no connection lists this model right now">
                unavailable
              </Badge>
            ) : null}
            {row.isNew ? (
              <Badge tone="accent" title="the registry gained this model after the preset was saved">
                new
              </Badge>
            ) : null}
          </span>
          <Button
            variant="quiet"
            ariaLabel={`Restore ${row.ref} to the active list`}
            onClick={() => onRestore(row.ref)}
          >
            restore
          </Button>
        </li>
      ))}
      <li className="flex justify-end px-5 py-2">
        <Button
          variant="ghost"
          onClick={onRestoreAll}
          ariaLabel="Restore every hidden model"
          className="gap-1.5"
        >
          <EyeSlash size={14} aria-hidden /> restore all {rows.length}
        </Button>
      </li>
    </ul>
  );
}
