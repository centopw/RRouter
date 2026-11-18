import { useRef, type KeyboardEvent as ReactKeyboardEvent } from "react";
import * as Popover from "@radix-ui/react-popover";
import { CircleNotch, DotsThree, Play } from "@phosphor-icons/react";
import { Link } from "react-router-dom";
import { Badge, Button, Card, CopyIconButton, cx } from "../../components/ui";
import { Heading, Toggle } from "../../components/atoms";
import { CapabilityChips } from "../../components/capability-chips";
import { compact } from "../../lib/format";
import type { Model } from "../../lib/api";
import type { DraftGroup } from "../../lib/layout-draft";

/** A test is one icon: a word per row made the name repeat and the column jump. */
export function TestButton({
  model,
  pending,
  onTest,
}: {
  model: Model;
  pending: boolean;
  onTest: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onTest}
      disabled={pending}
      aria-busy={pending || undefined}
      aria-label={
        pending ? `Testing ${model.ref}` : `Send a test completion to ${model.ref}`
      }
      title={`Send a 16-token completion to ${model.ref}`}
      className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-muted transition-colors hover:bg-surface-3 hover:text-fg disabled:opacity-70"
    >
      {pending ? (
        <CircleNotch size={15} className="animate-spin" aria-hidden />
      ) : (
        <Play size={14} aria-hidden />
      )}
    </button>
  );
}

/**
 * Edit and delete live behind one trigger, so the row keeps a stable width.
 *
 * Menu semantics (one tab stop, arrows/Home/End inside) are hand-rolled: Radix Popover
 * already owns focus restore and Escape, and a `@radix-ui/react-dropdown-menu` dependency
 * for two items in one page was not worth the bundle.
 */
export function RowMenu({
  model,
  pendingDelete,
  onEdit,
  onDelete,
  onHide,
  onMoveTop,
  onMoveBottom,
  canMoveTop,
  canMoveBottom,
  groups,
  currentGroup,
  onMoveGroup,
}: {
  model: Model;
  pendingDelete: boolean;
  onEdit: () => void;
  onDelete: () => void;
  /** Only while a preset is active: hides the row here, not from routing. */
  onHide?: () => void;
  onMoveTop?: () => void;
  onMoveBottom?: () => void;
  canMoveTop?: boolean;
  canMoveBottom?: boolean;
  /** Groups on this preset; omitted for callers without a layout. */
  groups?: Array<DraftGroup & { implicit?: boolean }>;
  currentGroup?: string;
  onMoveGroup?: (group: string | null) => void;
}) {
  const surface = useRef<HTMLDivElement>(null);
  const items = () =>
    [...(surface.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [])];
  const focusItem = (index: number) => {
    const list = items();
    if (list.length === 0) return;
    list[(index + list.length) % list.length]?.focus();
  };
  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const index = items().indexOf(document.activeElement as HTMLElement);
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        focusItem(index + 1);
        break;
      case "ArrowUp":
        event.preventDefault();
        focusItem(index <= 0 ? -1 : index - 1);
        break;
      case "Home":
        event.preventDefault();
        focusItem(0);
        break;
      case "End":
        event.preventDefault();
        focusItem(-1);
        break;
      default:
        break;
    }
  };
  const item =
    "flex h-8 items-center rounded-[var(--radius-input)] px-2 text-left text-small text-fg hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-accent-text";
  const movableGroups = groups?.filter(
    (group) => !group.provider || group.provider === model.provider,
  );
  return (
    <Popover.Root>
      <Popover.Trigger asChild>
        <button
          type="button"
          aria-label={`More actions for ${model.ref}`}
          aria-haspopup="menu"
          className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-muted transition-colors hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
        >
          <DotsThree size={18} weight="bold" aria-hidden />
        </button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content
          ref={surface}
          role="menu"
          side="bottom"
          align="end"
          sideOffset={4}
          aria-label={`Actions for ${model.ref}`}
          onOpenAutoFocus={(event) => {
            event.preventDefault();
            window.requestAnimationFrame(() => focusItem(0));
          }}
          onKeyDown={onKeyDown}
          className="popover-panel z-50 flex w-56 flex-col rounded-[var(--radius-card)] border border-line bg-surface p-1 shadow-lg focus:outline-none"
        >
          <Popover.Close asChild>
            <Link
              to={`/playground/chat?model=${encodeURIComponent(model.ref)}`}
              role="menuitem"
              tabIndex={-1}
              className={item}
            >
              Test in Playground
            </Link>
          </Popover.Close>
          <Popover.Close asChild>
            <button type="button" role="menuitem" tabIndex={-1} onClick={onEdit} className={item}>
              Edit limits and price
            </button>
          </Popover.Close>
          {onMoveTop || onMoveBottom ? (
            <div className="mt-1 border-t border-line pt-1">
              {onMoveTop ? (
                <Popover.Close asChild>
                  <button
                    type="button"
                    role="menuitem"
                    tabIndex={-1}
                    disabled={canMoveTop === false}
                    onClick={onMoveTop}
                    className={cx(item, "w-full disabled:cursor-not-allowed disabled:text-faint")}
                  >
                    Move to top
                  </button>
                </Popover.Close>
              ) : null}
              {onMoveBottom ? (
                <Popover.Close asChild>
                  <button
                    type="button"
                    role="menuitem"
                    tabIndex={-1}
                    disabled={canMoveBottom === false}
                    onClick={onMoveBottom}
                    className={cx(item, "w-full disabled:cursor-not-allowed disabled:text-faint")}
                  >
                    Move to bottom
                  </button>
                </Popover.Close>
              ) : null}
            </div>
          ) : null}
          {groups && onMoveGroup ? (
            <div className="mt-1 border-t border-line pt-1">
              <p className="px-2 py-1 text-caption text-faint">Move to group</p>
              <div className="max-h-48 overflow-y-auto">
                {movableGroups?.map((group) => {
                  const checked = currentGroup === group.id;
                  return (
                    <Popover.Close asChild key={group.id}>
                      <button
                        type="button"
                        role="menuitem"
                        tabIndex={-1}
                        aria-current={checked ? "true" : undefined}
                        onClick={() => onMoveGroup(group.provider ? null : group.id)}
                        className={cx(item, checked && "bg-surface-2 text-accent-text")}
                      >
                        <span className="min-w-0 flex-1 truncate">{group.name}</span>
                        {group.provider ? (
                          <span className="ml-2 shrink-0 text-caption text-faint">provider</span>
                        ) : null}
                        {checked ? <span className="ml-2 shrink-0 text-caption">current</span> : null}
                      </button>
                    </Popover.Close>
                  );
                })}
              </div>
            </div>
          ) : null}
          {onHide ? (
            <Popover.Close asChild>
              <button type="button" role="menuitem" tabIndex={-1} onClick={onHide} className={item}>
                Remove from this preset
              </button>
            </Popover.Close>
          ) : null}
          {model.source === "custom" ? (
            <Popover.Close asChild>
              <button
                type="button"
                role="menuitem"
                tabIndex={-1}
                onClick={onDelete}
                disabled={pendingDelete}
                className={cx(item, "text-[var(--color-neg-ink)] hover:bg-[var(--color-neg-tint)]")}
              >
                Delete custom model
              </button>
            </Popover.Close>
          ) : null}
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}

/** One model as a phone card. Shared by the board and every filtered row set. */
export function ModelCardBody({
  model: m,
  isAdmin,
  testTarget,
  toggleTarget,
  removeTarget,
  onTest,
  onToggle,
  onEdit,
  onDelete,
  onHide,
  onMoveTop,
  onMoveBottom,
  canMoveTop,
  canMoveBottom,
  groups,
  currentGroup,
  onMoveGroup,
}: {
  model: Model;
  isAdmin: boolean;
  testTarget: string | null;
  toggleTarget: string | null;
  removeTarget: string | null;
  onTest: (model: Model) => void;
  onToggle: (model: Model) => void;
  onEdit: (model: Model) => void;
  onDelete: (model: Model) => void;
  onHide?: (ref: string) => void;
  onMoveTop?: () => void;
  onMoveBottom?: () => void;
  canMoveTop?: boolean;
  canMoveBottom?: boolean;
  groups?: Array<DraftGroup & { implicit?: boolean }>;
  currentGroup?: string;
  onMoveGroup?: (group: string | null) => void;
}) {
  const reference = groupProviderRef(m, currentGroup, groups);
  return (
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0">
              <div className="flex min-w-0 flex-wrap items-center gap-1.5">
                <span className="shrink-0 rounded-full border border-line bg-surface-2 px-1.5 py-px font-mono text-caption uppercase tracking-[0.06em] text-muted">
                  {m.provider}
                </span>
                <h3
                  className={cx(
                    "min-w-0 break-all font-mono text-small",
                    m.enabled ? "text-fg" : "text-faint",
                  )}
                >
                  {m.id}
                </h3>
                <CopyIconButton
                  value={reference}
                  label={`Copy model reference ${reference}`}
                />
                {m.source === "custom" ? (
                  <Badge tone="accent" title="added by an operator, not synced from the provider">
                    custom
                  </Badge>
                ) : null}
                {m.deprecated ? (
                  <Badge tone="warn" title="this family is retired by its provider">
                    deprecated
                  </Badge>
                ) : null}
              </div>
              {m.display && m.display !== m.id ? (
                <p className="mt-0.5 truncate text-caption text-faint">{m.display}</p>
              ) : null}
              <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-caption text-faint">
                <span className="whitespace-nowrap tnum">ctx {compact(m.context)}</span>
                <span className="whitespace-nowrap tnum">max {compact(m.max_output)}</span>
                {m.cost ? (
                  <span className="whitespace-nowrap tnum" title={priceTitle(m)}>
                    {cardPrice(m)}
                  </span>
                ) : null}
              </div>
              <div className="mt-1.5">
                <CapabilityChips capabilities={m.capabilities} max={2} subject={m.ref} />
              </div>
            </div>
            <div className="flex shrink-0 flex-col items-end gap-2">
              {isAdmin ? (
                <Toggle
                  size="sm"
                  checked={m.enabled}
                  label={`${m.ref} ${m.enabled ? "enabled" : "disabled"}`}
                  disabled={toggleTarget === m.id}
                  onChange={() => onToggle(m)}
                />
              ) : (
                <Badge tone={m.enabled ? "pos" : "neutral"}>{m.enabled ? "on" : "off"}</Badge>
              )}
              {isAdmin ? (
                <div className="flex items-center gap-1">
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
                    onHide={onHide ? () => onHide(m.ref) : undefined}
                    onMoveTop={onMoveTop}
                    onMoveBottom={onMoveBottom}
                    canMoveTop={canMoveTop}
                    canMoveBottom={canMoveBottom}
                    groups={groups}
                    currentGroup={currentGroup}
                    onMoveGroup={onMoveGroup}
                  />
                </div>
              ) : null}
            </div>
          </div>
  );
}

/**
 * The routable ref for a row: a free-form group namespaces it and still pins the provider
 * (`group/provider/model`), so two providers serving one id can never be confused. A
 * provider bucket keeps the plain `provider/model` ref.
 */
export function groupProviderRef(
  model: Model,
  groupId: string | undefined,
  groups: Array<DraftGroup & { implicit?: boolean }> | undefined,
): string {
  const group = groups?.find((candidate) => candidate.id === groupId);
  const code = model.code || model.provider;
  const pinned = `${code}/${model.id}`;
  return group && !group.provider ? `${group.name}/${pinned}` : pinned;
}
/** Nothing here means nothing is connected: models come from providers, not catalogs. */
export function EmptyRegistry({
  onSync,
  syncing,
  canAdmin,
  onAddCustom,
}: {
  onSync: () => void;
  syncing: boolean;
  canAdmin: boolean;
  /** The second way out of an empty registry: a row no provider lists. */
  onAddCustom?: () => void;
}) {
  return (
    <Card className="px-6 py-12 text-center">
      <Heading level={2} className="text-heading font-medium">No models yet</Heading>
      <p className="mx-auto mt-1.5 max-w-[440px] text-body text-muted">
        Models are listed from the providers you connected — add one under{" "}
        <Link to="/providers" className="underline underline-offset-2">
          Providers
        </Link>
        . Metadata such as context window, capabilities, and price is filled in from models.dev when you sync.
      </p>
      {canAdmin ? (
        <div className="mt-5 flex flex-wrap justify-center gap-2">
          <Button variant="primary" onClick={onSync} disabled={syncing}>
            {syncing ? "syncing…" : "sync metadata"}
          </Button>
          {onAddCustom ? (
            <Button variant="ghost" onClick={onAddCustom}>
              add a custom model
            </Button>
          ) : null}
          <Link
            to="/providers"
            className="inline-flex h-8 items-center rounded-lg border border-line-strong bg-surface-2 px-3 text-body text-fg hover:border-[var(--color-line-strong)]"
          >
            add a provider
          </Link>
        </div>
      ) : null}
    </Card>
  );
}

/** `$2.50 / $10.00`, the compact price line every model row shows. */
export function priceLabel(model: Model): string | null {
  return model.cost
    ? `$${model.cost.input.toFixed(2)} / $${model.cost.output.toFixed(2)}`
    : null;
}

/** A card has no column header, so the unit is spelled out. */
function cardPrice(model: Model): string | null {
  return model.cost
    ? `$${model.cost.input.toFixed(2)} in / $${model.cost.output.toFixed(2)} out`
    : null;
}

export function priceTitle(model: Model): string {
  if (!model.cost) return "";
  const cache =
    model.cost.cache_read === undefined ? "" : ` · cache read $${model.cost.cache_read}`;
  return `input $${model.cost.input} · output $${model.cost.output}${cache} · per 1M tokens`;
}

/** "" and non-positive input mean "unknown", never zero. */
export function numeric(value: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) && parsed > 0 ? Math.floor(parsed) : null;
}
