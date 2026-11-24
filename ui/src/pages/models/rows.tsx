import {
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import * as Popover from "@radix-ui/react-popover";
import { CheckCircle, CircleNotch, DotsThree, Play, WarningCircle } from "@phosphor-icons/react";
import { Link } from "react-router-dom";
import { Badge, Button, Card, CopyIconButton, cx } from "../../components/ui";
import { Heading, Toggle } from "../../components/atoms";
import { CAPABILITY_KEYS, CAPABILITY_LABELS, CapabilityChips } from "../../components/capability-chips";
import { compact, ms } from "../../lib/format";
import { alsoMatches, canonicalRef } from "../../lib/model-ref";
import type { Model } from "../../lib/api";
import type { DraftGroup } from "../../lib/layout-draft";
import type { ModelTestOutcome } from "./actions";
import { cacheText, isFree, priceText } from "./price";

/** Shared styling for a provider ownership badge (plain mono pill, no tracked-caps). */
export const PROVIDER_PILL =
  "inline-flex shrink-0 items-center rounded-full border border-line bg-surface-2 px-2 py-px font-mono text-caption text-muted";

export const PROVIDER_LINK = cx(
  PROVIDER_PILL,
  "transition-colors hover:border-line-strong hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text",
);

/**
 * Non-modal, non-focus-stealing task-scoped preview. Opens after ~350 ms mouse hover or
 * keyboard focus on its trigger, never contains interactive controls, and closes on blur,
 * pointer leave, or Escape.
 */
export function RowPreview({
  trigger,
  content,
  label,
}: {
  trigger: ReactNode;
  content: ReactNode;
  label: string;
}) {
  const [open, setOpen] = useState(false);
  const timer = useRef<number | null>(null);
  const previewId = useId();

  const clearTimer = () => {
    if (timer.current !== null) {
      window.clearTimeout(timer.current);
      timer.current = null;
    }
  };

  useEffect(() => clearTimer, []);

  return (
    <Popover.Root open={open} onOpenChange={setOpen}>
      <Popover.Anchor asChild>
        <span
          tabIndex={0}
          aria-describedby={open ? previewId : undefined}
          onPointerEnter={(event) => {
            if (event.pointerType !== "mouse") return;
            clearTimer();
            timer.current = window.setTimeout(() => setOpen(true), 350);
          }}
          onPointerLeave={() => {
            clearTimer();
            setOpen(false);
          }}
          onFocus={() => setOpen(true)}
          onBlur={() => {
            clearTimer();
            setOpen(false);
          }}
          onKeyDown={(event) => {
            if (event.key === "Escape" && open) {
              event.stopPropagation();
              setOpen(false);
            }
          }}
          className="inline-flex min-w-0 max-w-full items-center rounded-[4px] focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
        >
          {trigger}
        </span>
      </Popover.Anchor>
      {open ? (
        <Popover.Portal>
          <Popover.Content
            id={previewId}
            role="tooltip"
            aria-label={label}
            side="bottom"
            align="start"
            sideOffset={6}
            collisionPadding={12}
            onOpenAutoFocus={(event) => event.preventDefault()}
            onCloseAutoFocus={(event) => event.preventDefault()}
            className="popover-panel pointer-events-none z-[var(--z-popover-portal)] w-80 max-w-[calc(100vw-24px)] rounded-[var(--radius-overlay)] border border-line-strong bg-surface p-3 text-caption text-fg shadow-[var(--shadow-popover)]"
          >
            {content}
          </Popover.Content>
        </Popover.Portal>
      ) : null}
    </Popover.Root>
  );
}

/** Control-free preview body for one registry model. */
export function ModelPreviewBody({
  model,
  reference,
  otherSpellings,
  testOutcome,
}: {
  model: Model;
  reference: string;
  otherSpellings?: readonly string[];
  testOutcome?: ModelTestOutcome;
}) {
  const caps = CAPABILITY_KEYS.filter((key) => model.capabilities?.[key] === true);
  const cache = cacheText(model.cost);
  const status = !model.enabled
    ? "Disabled globally (refused on every endpoint)"
    : !model.available
      ? "Unavailable — no healthy connection for this provider"
      : "Available right now";
  return (
    <div className="flex flex-col gap-1.5">
      <div className="break-all font-mono text-small font-medium text-fg">{reference}</div>
      {model.display && model.display !== model.id ? (
        <div className="text-caption text-muted">{model.display}</div>
      ) : null}
      <div
        className={cx(
          "text-caption font-medium",
          !model.enabled
            ? "text-faint"
            : !model.available
              ? "text-[var(--color-warn-ink)]"
              : "text-[var(--color-pos-ink)]",
        )}
      >
        {status}
      </div>
      <div className="flex flex-wrap gap-x-3 gap-y-0.5 text-caption text-muted tnum">
        <span>Context {compact(model.context)}</span>
        <span>Max output {compact(model.max_output)}</span>
      </div>
      <div className="text-caption text-fg tnum">
        {isFree(model.cost) ? "Free · $0.00 /1M" : priceText(model.cost)}
        {cache ? <span className="block text-faint">{cache}</span> : null}
      </div>
      {caps.length > 0 ? (
        <div className="text-caption text-muted">
          Capabilities: {caps.map((key) => CAPABILITY_LABELS[key].chip).join(", ")}
        </div>
      ) : null}
      {model.reasoning?.supported_efforts && model.reasoning.supported_efforts.length > 0 ? (
        <div className="text-caption text-muted">
          Reasoning efforts: {model.reasoning.supported_efforts.join(", ")}
        </div>
      ) : null}
      {otherSpellings && otherSpellings.length > 0 ? (
        <div className="border-t border-line pt-1.5 text-caption text-muted">
          <div className="mb-0.5 text-faint">Also matches</div>
          <div className="flex flex-col gap-0.5 font-mono text-fg">
            {otherSpellings.map((spelling) => (
              <span key={spelling} className="break-all">
                {spelling}
              </span>
            ))}
          </div>
        </div>
      ) : null}
      {testOutcome ? (
        <div
          className={cx(
            "border-t border-line pt-1.5 text-caption tnum",
            testOutcome.ok ? "text-[var(--color-pos-ink)]" : "text-[var(--color-neg-ink)]",
          )}
        >
          {testOutcome.ok
            ? `Last test passed · HTTP ${testOutcome.status} · ${ms(testOutcome.latency_ms)}`
            : `Last test failed · ${testOutcome.status > 0 ? `HTTP ${testOutcome.status} · ` : ""}${testOutcome.error ?? "request failed"}`}
        </div>
      ) : null}
    </div>
  );
}

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
      className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-muted transition-colors hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text disabled:opacity-70"
    >
      {pending ? (
        <CircleNotch size={15} className="animate-spin" aria-hidden />
      ) : (
        <Play size={14} aria-hidden />
      )}
    </button>
  );
}

/** Inline test outcome displayed directly on the row so the operator never hunts a toast. */
export function TestOutcomeInline({ outcome }: { outcome?: ModelTestOutcome }) {
  if (!outcome) return null;
  if (outcome.ok) {
    return (
      <span
        role="status"
        aria-live="polite"
        className="inline-flex items-center gap-1 text-caption text-[var(--color-pos-ink)] tnum"
      >
        <CheckCircle size={13} weight="fill" aria-hidden />
        <span>works · {ms(outcome.latency_ms)}</span>
      </span>
    );
  }
  const reason = outcome.error
    ? outcome.error.length > 72
      ? `${outcome.error.slice(0, 72)}…`
      : outcome.error
    : outcome.status > 0
      ? `HTTP ${outcome.status}`
      : "failed";
  return (
    <span
      role="status"
      aria-live="polite"
      className="inline-flex min-w-0 items-center gap-1 text-caption text-[var(--color-neg-ink)] tnum"
    >
      <WarningCircle size={13} weight="fill" className="shrink-0" aria-hidden />
      <span className="truncate">
        test failed{outcome.status > 0 ? ` (${outcome.status})` : ""}: {reason}
      </span>
    </span>
  );
}

/**
 * Edit and delete live behind one trigger, so the row keeps a stable width.
 *
 * Menu semantics (one tab stop, arrows/Home/End inside) are hand-rolled: Radix Popover
 * already owns focus restore and Escape, and a `@radix-ui/react-dropdown-menu` dependency
 * for one page was not worth the bundle.
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
  groupMenuClassName,
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
  /** Groups on this preset; omitted when an inline Group select is already shown. */
  groups?: Array<DraftGroup & { implicit?: boolean }>;
  currentGroup?: string;
  onMoveGroup?: (group: string | null) => void;
  /** Optional responsive visibility class (e.g. `md:hidden` when desktop has a Group column). */
  groupMenuClassName?: string;
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
          className="popover-panel z-[var(--z-popover-portal)] flex w-56 flex-col rounded-[var(--radius-card)] border border-line bg-surface p-1 shadow-[var(--shadow-popover)] focus:outline-none"
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
            <div className={cx("mt-1 border-t border-line pt-1", groupMenuClassName)}>
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
  testOutcome,
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
}: {
  model: Model;
  isAdmin: boolean;
  testTarget: string | null;
  testOutcome?: ModelTestOutcome;
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
}) {
  const reference = canonicalRef(m, groups, currentGroup);
  const otherSpellings = alsoMatches(m, groups, currentGroup);
  return (
    <div className="flex items-start justify-between gap-3">
      <div className="min-w-0">
        <div className="flex min-w-0 flex-wrap items-center gap-1.5">
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
              <h3
                className={cx(
                  "min-w-0 break-all font-mono text-small",
                  m.enabled ? "text-fg" : "text-faint",
                )}
              >
                {m.id}
              </h3>
            }
            content={
              <ModelPreviewBody
                model={m}
                reference={reference}
                otherSpellings={otherSpellings}
                testOutcome={testOutcome}
              />
            }
          />
          <CopyIconButton
            value={reference}
            label={`Copy model name ${reference}`}
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
          {!m.enabled ? (
            <Badge tone="neutral" title="the gateway refuses this id on every endpoint">
              disabled
            </Badge>
          ) : !m.available ? (
            <AvailabilityBadge linked={isAdmin} />
          ) : null}
          {isFree(m.cost) ? <FreeBadge linked={isAdmin} /> : null}
        </div>
        {m.display && m.display !== m.id ? (
          <p className="mt-0.5 truncate text-caption text-faint">{m.display}</p>
        ) : null}
        {reference !== m.ref ? (
          <p className="mt-0.5 break-all font-mono text-caption text-faint">{reference}</p>
        ) : null}
        <ModelMetaLine model={m} />
        <div className="mt-1.5 flex flex-wrap items-center gap-2">
          <CapabilityChips capabilities={m.capabilities} max={2} subject={m.ref} />
          <TestOutcomeInline outcome={testOutcome} />
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
            />
          </div>
        ) : null}
      </div>
    </div>
  );
}

/**
 * The canonical routable ref for a row; delegates to `canonicalRef` in `lib/model-ref`
 * so ungrouped rows always yield `provider/id` and grouped rows yield the one
 * group-qualified form.
 */
export function groupProviderRef(
  model: Model,
  groupId: string | undefined,
  groups: Array<DraftGroup & { implicit?: boolean }> | undefined,
): string {
  return canonicalRef(model, groups, groupId);
}

/** Nothing here means nothing is connected: models come from providers or Free mode. */
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
      <p className="mx-auto mt-1.5 max-w-[480px] text-body text-muted">
        Models come from connected accounts under{" "}
        <Link to="/providers" className="text-accent-text underline underline-offset-2">
          Providers
        </Link>
        {canAdmin ? (
          <>
            {" "}or from{" "}
            <Link to="/free-mode" className="text-accent-text underline underline-offset-2">
              Free mode
            </Link>
          </>
        ) : null}
        . Context limits, capabilities, and pricing are filled in from models.dev when you sync.
      </p>
      {canAdmin ? (
        <div className="mt-5 flex flex-wrap justify-center gap-2">
          <Link
            to="/providers"
            className="inline-flex h-8 items-center rounded-full bg-accent px-3.5 text-body font-semibold text-[var(--color-accent-ink)] transition-colors hover:bg-fg"
          >
            Add a provider
          </Link>
          <Link
            to="/free-mode"
            className="inline-flex h-8 items-center rounded-full border border-line-strong bg-surface px-3.5 text-body text-fg transition-colors hover:bg-surface-3"
          >
            Set up Free mode
          </Link>
          <Button variant="quiet" onClick={onSync} disabled={syncing}>
            {syncing ? "Syncing…" : "Sync metadata"}
          </Button>
          {onAddCustom ? (
            <Button variant="quiet" onClick={onAddCustom}>
              Add custom model
            </Button>
          ) : null}
        </div>
      ) : null}
    </Card>
  );
}

/**
 * Visible availability warning when a model is enabled globally but its provider currently
 * has no healthy connection. Links to Providers so the operator can fix the cause in one click.
 */
export function AvailabilityBadge({ linked }: { linked: boolean }) {
  const badge = (
    <Badge tone="warn" title="No healthy connection for this provider right now">
      unavailable
    </Badge>
  );
  if (!linked) return <span className="shrink-0">{badge}</span>;
  return (
    <Link
      to="/providers"
      aria-label="Model unavailable: no healthy connection. Open Providers to check accounts."
      className="shrink-0 rounded-full focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
    >
      {badge}
    </Link>
  );
}

/**
 * The free marker. Free is a price state (every published dimension is zero), so it sits
 * with the other badges and its filter is the toolbar's `Free` chip. For an admin it links
 * to Free mode, where free models are found and fallback to them is set up.
 */
export function FreeBadge({ linked }: { linked: boolean }) {
  if (!linked) {
    return (
      <span className="shrink-0">
        <Badge tone="pos">Free</Badge>
      </span>
    );
  }
  return (
    <Link
      to="/free-mode"
      aria-label="Free model ($0/1M). Open Free mode to set up free models and fallback."
      className="shrink-0 rounded-full focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
    >
      <Badge tone="pos">Free</Badge>
    </Link>
  );
}

/**
 * Context, max output and price, in one line, for the table and the phone card alike.
 * The unit is printed, not hidden in a tooltip; a free model's price is its badge, so the
 * line omits it rather than repeating `Free`.
 */
export function ModelMetaLine({ model, children }: { model: Model; children?: ReactNode }) {
  const cache = cacheText(model.cost);
  return (
    <div className="mt-1 flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-caption text-faint">
      <span className="whitespace-nowrap tnum">ctx {compact(model.context)}</span>
      <span className="whitespace-nowrap tnum">max {compact(model.max_output)}</span>
      {isFree(model.cost) ? null : (
        <span className="whitespace-nowrap tnum">{priceText(model.cost)}</span>
      )}
      {cache ? <span className="whitespace-nowrap tnum">{cache}</span> : null}
      {children}
    </div>
  );
}

/** "" and non-positive input mean "unknown", never zero. */
export function numeric(value: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) && parsed > 0 ? Math.floor(parsed) : null;
}
