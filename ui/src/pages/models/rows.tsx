import { useRef, type KeyboardEvent as ReactKeyboardEvent } from "react";
import * as Popover from "@radix-ui/react-popover";
import { CircleNotch, DotsThree, Play, Star } from "@phosphor-icons/react";
import { Badge, Button, Card, CopyIconButton, cx } from "../../components/ui";
import { Heading, IconButton, Toggle } from "../../components/atoms";
import { CapabilityChips } from "../../components/capability-chips";
import { ProviderIcon } from "../../components/provider-icon";
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
            <button type="button" role="menuitem" tabIndex={-1} onClick={onEdit} className={item}>
              Edit limits and price
            </button>
          </Popover.Close>
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
                Hide from the board and /v1/models
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

/** Below `sm` a row becomes a card: four table columns at 390 would scroll sideways. */
export function ModelCards({
  rows,
  isAdmin,
  testTarget,
  toggleTarget,
  removeTarget,
  onTest,
  onToggle,
  onEdit,
  onDelete,
  coverageFor,
  displayOfProvider,
  onPickProvider,
  onPreferPreference,
}: {
  rows: Model[];
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
  onPreferPreference: (model: Model, provider: string | null) => void;
}) {
  return (
    <ul aria-label="Models registry" className="flex flex-col">
      {rows.map((m) => (
        <li
          key={m.ref}
          className="border-b border-[var(--color-rule)] px-4 py-3 last:border-b-0"
        >
          <ModelCardBody
            model={m}
            isAdmin={isAdmin}
            testTarget={testTarget}
            toggleTarget={toggleTarget}
            removeTarget={removeTarget}
            onTest={onTest}
            onToggle={onToggle}
            onEdit={onEdit}
            onDelete={onDelete}
            coverageFor={coverageFor}
            displayOfProvider={displayOfProvider}
            onPickProvider={onPickProvider}
            onPreferPreference={onPreferPreference}
          />
        </li>
      ))}
    </ul>
  );
}

/** One model as a phone card. Shared by the registry and the sortable active list. */
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
  coverageFor,
  displayOfProvider,
  onPickProvider,
  onPreferPreference,
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
  groups?: Array<DraftGroup & { implicit?: boolean }>;
  currentGroup?: string;
  onMoveGroup?: (group: string | null) => void;
  coverageFor: (id: string) => Model[];
  displayOfProvider: (id: string) => string;
  onPickProvider: (id: string) => void;
  onPreferPreference: (model: Model, provider: string | null) => void;
}) {
  return (
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0">
              <div className="flex min-w-0 flex-wrap items-center gap-1.5">
                <span className="shrink-0 rounded-full border border-line bg-surface-2 px-1.5 py-px font-mono text-[10px] uppercase tracking-[0.06em] text-muted">
                  {m.provider}
                </span>
                <h3
                  className={cx(
                    "min-w-0 truncate font-mono text-small",
                    m.enabled ? "text-fg" : "text-faint",
                  )}
                >
                  {m.id}
                </h3>
                <CopyIconButton
                  value={groupModelRef(m, currentGroup, groups)}
                  label={`Copy model reference ${m.id}`}
                />
                <CoveragePeers
                  model={m}
                  peers={coverageFor(m.id)}
                  displayOf={displayOfProvider}
                  onPick={onPickProvider}
                  onPrefer={(provider) => onPreferPreference(m, provider)}
                />
                {m.preferred_provider === m.provider ? (
                  <Badge tone="accent" title={`preferred provider for ${m.id}`}>
                    preferred
                  </Badge>
                ) : null}
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

/** The `/v1/models` id for a row: group name for a free-form group, provider otherwise. */
export function groupModelRef(
  model: Model,
  groupId: string | undefined,
  groups: Array<DraftGroup & { implicit?: boolean }> | undefined,
): string {
  const group = groups?.find((candidate) => candidate.id === groupId);
  return group && !group.provider ? `${group.name}/${model.id}` : model.ref;
}
export const rowKey = (model: Model) => model.ref;

/** Nothing here means nothing is connected: models come from providers, not catalogs. */
export function EmptyRegistry({
  onSync,
  syncing,
  canAdmin,
}: {
  onSync: () => void;
  syncing: boolean;
  canAdmin: boolean;
}) {
  return (
    <Card className="px-6 py-12 text-center">
      <Heading level={2} className="text-heading font-medium">No models yet</Heading>
      <p className="mx-auto mt-1.5 max-w-[440px] text-body text-muted">
        Models are listed from the providers you connected — add one under{" "}
        <a href="/app/providers" className="underline underline-offset-2">
          Providers
        </a>
        . Metadata such as context window, capabilities, and price is filled in from models.dev when you sync.
      </p>
      {canAdmin ? (
        <div className="mt-5 flex justify-center gap-2">
          <Button variant="primary" onClick={onSync} disabled={syncing}>
            {syncing ? "syncing…" : "sync metadata"}
          </Button>
          <a
            href="/app/providers"
            className="inline-flex h-8 items-center rounded-lg border border-line-strong bg-surface-2 px-3 text-body text-fg hover:border-[var(--color-line-strong)]"
          >
            go to providers
          </a>
        </div>
      ) : null}
    </Card>
  );
}

/** `$2.50 / $10.00`, in the units the `price /1M` column names. */
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

/**
 * "2 providers" for a canonical model id, with the peers behind a popover. The row itself
 * stays one provider: enabling, testing and pricing are per connection, so rows are never
 * merged; this only says the same id is routable elsewhere and lets the operator jump there.
 */
export function CoveragePeers({
  model,
  peers,
  displayOf,
  onPick,
  onPrefer,
}: {
  model: Model;
  peers: Model[];
  displayOf: (provider: string) => string;
  onPick: (provider: string) => void;
  /** `null` clears the preference; the same id carries it on every provider row. */
  onPrefer: (provider: string | null) => void;
}) {
  if (peers.length < 2) return null;
  const preferred = model.preferred_provider ?? null;
  const preferredMissing = preferred !== null && !peers.some((peer) => peer.provider === preferred);
  const refs = peers.map((peer) => peer.ref).join("\n");
  return (
    <Popover.Root>
      <Popover.Trigger asChild>
        <button
          type="button"
          aria-label={`${peers.length} providers serve ${model.id}`}
          aria-haspopup="dialog"
          title={refs}
          className="inline-flex h-5 shrink-0 items-center rounded-full border border-line bg-surface-2 px-1.5 text-caption text-muted transition-colors hover:bg-surface-3 hover:text-fg focus-visible:outline-none! outline-0!"
        >
          {peers.length} providers
        </button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content
          side="bottom"
          align="start"
          sideOffset={4}
          aria-label={`Providers serving ${model.id}`}
          className="popover-panel z-50 w-80 rounded-[var(--radius-card)] border border-line bg-surface p-1.5 shadow-lg focus:outline-none"
        >
          <p className="px-1 pb-1 text-caption text-faint">
            <span className="font-mono text-muted">{model.id}</span> is served by
          </p>
          <ul className="flex flex-col">
            {peers.map((peer) => {
              const isPreferred = preferred === peer.provider;
              return (
                <li key={peer.ref} className="flex items-center gap-1">
                  <Popover.Close asChild>
                    <button
                      type="button"
                      onClick={() => onPick(peer.provider)}
                      className="flex min-w-0 flex-1 items-center gap-2 rounded-[var(--radius-input)] px-1.5 py-1.5 text-left text-small hover:bg-surface-3 focus-visible:outline-none! outline-0!"
                    >
                      <ProviderIcon provider={peer.provider} size={14} />
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-fg">{displayOf(peer.provider)}</span>
                        <span className="block truncate font-mono text-caption text-faint">
                          {peer.ref}
                        </span>
                      </span>
                      <span className="shrink-0 text-caption text-faint tnum">
                        {priceLabel(peer) ?? "no price"}
                      </span>
                      <Badge tone={peer.enabled ? "pos" : "neutral"}>
                        {peer.enabled ? "on" : "off"}
                      </Badge>
                    </button>
                  </Popover.Close>
                  <IconButton
                    label={
                      isPreferred
                        ? `Clear the preferred provider for ${model.id}`
                        : `Use ${displayOf(peer.provider)} as the preferred provider for ${model.id}`
                    }
                    aria-pressed={isPreferred}
                    onClick={() => onPrefer(isPreferred ? null : peer.provider)}
                    className={isPreferred ? "text-[var(--color-accent-text)]" : undefined}
                  >
                    <Star size={14} weight={isPreferred ? "fill" : "regular"} aria-hidden />
                  </IconButton>
                </li>
              );
            })}
          </ul>
          {preferredMissing ? (
            <p
              role="status"
              className="mt-1 flex items-center justify-between gap-2 rounded-[6px] border border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] px-2 py-1.5 text-caption leading-relaxed text-[var(--color-warn-ink)]"
            >
              <span className="min-w-0">
                preferred provider <span className="font-mono">{preferred}</span> is not connected
                right now
              </span>
              <button
                type="button"
                onClick={() => onPrefer(null)}
                className="shrink-0 underline underline-offset-2"
              >
                clear
              </button>
            </p>
          ) : null}
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}
