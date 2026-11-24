import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
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
} from "@dnd-kit/core";
import {
  SortableContext,
  arrayMove,
  sortableKeyboardCoordinates,
  useSortable,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { restrictToParentElement, restrictToVerticalAxis } from "@dnd-kit/modifiers";
import { DotsSixVertical } from "@phosphor-icons/react";
import type { Model } from "../lib/api";
import { useReducedMotion } from "../lib/media";
import { useModelProviders, useModels } from "../lib/queries";
import { isFree } from "../pages/models/price";
import { Select, cx } from "./ui";

/**
 * Every routing model field in this app is a select grouped by provider.
 * A stored value that is no longer listed stays selectable, marked "(missing)",
 * so a stale alias is visible instead of silently retargeted.
 */

export type ModelSelectProps = {
  value: string;
  onChange: (value: string) => void;
  ariaLabel: string;
  allowEmpty?: string;
  provider?: string | null;
  disabled?: boolean;
  className?: string;
  /** Borderless trigger for the composer. */
  plain?: boolean;
  /** Persistent controls under the option list. */
  footer?: ReactNode;
};

export function ModelSelect({
  value,
  onChange,
  ariaLabel,
  allowEmpty,
  provider,
  disabled,
  className,
  plain,
  footer,
}: ModelSelectProps) {
  const models = useModels();
  const providers = useModelProviders();
  const display = useMemo(
    () => new Map((providers.data ?? []).map((entry) => [entry.id, entry.display])),
    [providers.data],
  );
  const groups = useGroups(models.data, provider, display);
  const known = useMemo(() => (models.data ?? []).some((m) => m.ref === value), [models.data, value]);
  const missing = value !== "" && !known;

  return (
    <Select
      ariaLabel={ariaLabel}
      value={value}
      disabled={disabled || models.isLoading}
      onChange={onChange}
      className={className}
      plain={plain}
      footer={footer}
    >
      {allowEmpty !== undefined ? <option value="">{allowEmpty}</option> : null}
      {missing ? <option value={value}>{value} (missing)</option> : null}
      {groups.map(([group, items]) => (
        <optgroup key={group} label={group}>
          {items.map((m) => (
            <option key={m.ref} value={m.ref}>
              {m.id}
              {isFree(m.cost) ? " · Free" : ""}
              {m.deprecated ? " (deprecated)" : ""}
              {m.available ? "" : " · unavailable"}
            </option>
          ))}
        </optgroup>
      ))}
    </Select>
  );
}

function useGroups(
  models: Model[] | undefined,
  provider: string | null | undefined,
  display: Map<string, string>,
) {
  return useMemo(() => {
    const map = new Map<string, Model[]>();
    for (const m of models ?? []) {
      if (provider && m.provider !== provider) continue;
      const bucket = map.get(m.provider);
      if (bucket) bucket.push(m);
      else map.set(m.provider, [m]);
    }
    return [...map.entries()]
      .map(([id, items]) => [display.get(id) ?? id, items] as const)
      .sort(([a], [b]) => a.localeCompare(b));
  }, [display, models, provider]);
}

/** Ordered list editor used by combos: each step is a model select. */
export function ModelChain({
  models,
  onChange,
}: {
  models: string[];
  onChange: (models: string[]) => void;
}) {
  const reducedMotion = useReducedMotion();
  const nextId = useRef(models.length);
  const [slotIds, setSlotIds] = useState<string[]>(() =>
    models.map((_, index) => `chain-slot-${index}`),
  );
  const [draggingId, setDraggingId] = useState<string | null>(null);

  useEffect(() => {
    setSlotIds((prev) => {
      if (prev.length === models.length) return prev;
      if (prev.length > models.length) return prev.slice(0, models.length);
      const added = Array.from(
        { length: models.length - prev.length },
        () => `chain-slot-${nextId.current++}`,
      );
      return [...prev, ...added];
    });
  }, [models.length]);

  const itemIds = useMemo(
    () => models.map((_, index) => slotIds[index] ?? `chain-slot-pending-${index}`),
    [models, slotIds],
  );

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 6 } }),
    useSensor(TouchSensor, { activationConstraint: { delay: 180, tolerance: 8 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  const labelFor = (id: string | number | null) => {
    if (id === null) return "model";
    const index = itemIds.indexOf(String(id));
    if (index === -1) return "model";
    return models[index] || `slot ${index + 1}`;
  };

  const positionFor = (id: string | number | null) => {
    if (id === null) return 0;
    return itemIds.indexOf(String(id)) + 1;
  };

  const announcements: Announcements = useMemo(
    () => ({
      onDragStart: ({ active }) => `Picked up ${labelFor(active.id)}.`,
      onDragOver: ({ active, over }) =>
        over
          ? `${labelFor(active.id)} is over position ${positionFor(over.id)} of ${models.length}.`
          : `${labelFor(active.id)} is outside the chain.`,
      onDragEnd: ({ active, over }) =>
        over
          ? `Moved ${labelFor(active.id)} to position ${positionFor(over.id)} of ${models.length}.`
          : `Cancelled; ${labelFor(active.id)} stayed where it was.`,
      onDragCancel: ({ active }) => `Cancelled; ${labelFor(active.id)} stayed where it was.`,
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [models, itemIds],
  );

  const set = (index: number, ref: string) => {
    const next = [...models];
    next[index] = ref;
    onChange(next);
  };

  const move = (index: number, delta: number) => {
    const target = index + delta;
    if (target < 0 || target >= models.length) return;
    setSlotIds((prev) => arrayMove(prev, index, target));
    onChange(arrayMove(models, index, target));
  };

  const remove = (index: number) => {
    setSlotIds((prev) => prev.filter((_, i) => i !== index));
    onChange(models.filter((_, i) => i !== index));
  };

  const add = () => {
    const fresh = `chain-slot-${nextId.current++}`;
    setSlotIds((prev) => [...prev, fresh]);
    onChange([...models, ""]);
  };

  const onDragStart = (event: DragStartEvent) => {
    setDraggingId(String(event.active.id));
  };

  const onDragEnd = (event: DragEndEvent) => {
    setDraggingId(null);
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const oldIndex = itemIds.indexOf(String(active.id));
    const newIndex = itemIds.indexOf(String(over.id));
    if (oldIndex === -1 || newIndex === -1) return;
    setSlotIds(arrayMove(itemIds, oldIndex, newIndex));
    onChange(arrayMove(models, oldIndex, newIndex));
  };

  const draggingIndex = draggingId ? itemIds.indexOf(draggingId) : -1;
  const draggingModel = draggingIndex >= 0 ? models[draggingIndex] : undefined;

  return (
    <div className="flex flex-col gap-2">
      <DndContext
        sensors={sensors}
        collisionDetection={closestCenter}
        modifiers={[restrictToVerticalAxis, restrictToParentElement]}
        accessibility={{
          announcements,
          screenReaderInstructions: {
            draggable:
              "Press space to pick up a chain model, arrow keys to move it, space to drop it, escape to cancel.",
          },
        }}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
        onDragCancel={() => setDraggingId(null)}
      >
        <SortableContext items={itemIds} strategy={verticalListSortingStrategy}>
          {models.map((ref, index) => {
            const id = itemIds[index] ?? `chain-slot-pending-${index}`;
            return (
              <SortableChainRow
                key={id}
                id={id}
                index={index}
                total={models.length}
                value={ref}
                reducedMotion={reducedMotion}
                onChange={(v) => set(index, v)}
                onMove={(delta) => move(index, delta)}
                onRemove={() => remove(index)}
              />
            );
          })}
        </SortableContext>
        <DragOverlay dropAnimation={reducedMotion ? null : undefined}>
          {draggingIndex >= 0 ? (
            <div className="flex items-center gap-2 rounded-[var(--radius-card)] border border-line-strong bg-surface px-3 py-1.5 shadow-[var(--shadow-popover)]">
              <DotsSixVertical size={15} aria-hidden className="text-faint" />
              <span className="text-caption text-faint tnum">{draggingIndex + 1}</span>
              <span className="truncate font-mono text-small text-fg">
                {draggingModel || "choose a model"}
              </span>
            </div>
          ) : null}
        </DragOverlay>
      </DndContext>
      <div>
        <button
          type="button"
          onClick={add}
          className="text-small text-muted underline underline-offset-2 hover:text-fg"
        >
          add model
        </button>
      </div>
    </div>
  );
}

function SortableChainRow({
  id,
  index,
  total,
  value,
  reducedMotion,
  onChange,
  onMove,
  onRemove,
}: {
  id: string;
  index: number;
  total: number;
  value: string;
  reducedMotion: boolean;
  onChange: (value: string) => void;
  onMove: (delta: number) => void;
  onRemove: () => void;
}) {
  const {
    attributes,
    listeners,
    setNodeRef,
    setActivatorNodeRef,
    transform,
    transition,
    isDragging,
  } = useSortable({ id });

  return (
    <div
      ref={setNodeRef}
      style={{
        transform: CSS.Translate.toString(transform),
        transition: reducedMotion ? undefined : transition,
      }}
      data-dragging={isDragging || undefined}
      className={cx(
        "flex items-center gap-1.5 rounded-[var(--radius-input)]",
        isDragging && "relative z-10 opacity-60",
      )}
    >
      <button
        ref={setActivatorNodeRef}
        type="button"
        aria-label={`Drag to reorder model ${index + 1}`}
        {...attributes}
        {...listeners}
        className="flex h-8 w-6 shrink-0 cursor-grab items-center justify-center rounded-[var(--radius-input)] text-faint transition-colors hover:bg-surface-3 hover:text-fg active:cursor-grabbing"
      >
        <DotsSixVertical size={15} aria-hidden />
      </button>
      <span className="w-4 shrink-0 text-right text-caption text-faint tnum">{index + 1}</span>
      <div className="min-w-0 flex-1">
        <ModelSelect
          ariaLabel={`model ${index + 1}`}
          value={value}
          onChange={onChange}
          allowEmpty="choose a model"
        />
      </div>
      <button
        type="button"
        aria-label={`move model ${index + 1} up`}
        disabled={index === 0}
        onClick={() => onMove(-1)}
        className="h-8 w-7 shrink-0 rounded-[var(--radius-input)] text-muted transition-colors hover:bg-surface-2 hover:text-fg disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-transparent disabled:hover:text-muted"
      >
        ↑
      </button>
      <button
        type="button"
        aria-label={`move model ${index + 1} down`}
        disabled={index === total - 1}
        onClick={() => onMove(1)}
        className="h-8 w-7 shrink-0 rounded-[var(--radius-input)] text-muted transition-colors hover:bg-surface-2 hover:text-fg disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-transparent disabled:hover:text-muted"
      >
        ↓
      </button>
      <button
        type="button"
        aria-label={`remove model ${index + 1}`}
        onClick={onRemove}
        className="h-8 w-7 shrink-0 rounded-[var(--radius-input)] text-muted transition-colors hover:bg-[var(--color-neg-tint)] hover:text-neg"
      >
        ×
      </button>
    </div>
  );
}
