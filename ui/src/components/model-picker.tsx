import { useMemo, useState, type ReactNode } from "react";
import type { Model } from "../lib/api";
import { useModelProviders, useModels } from "../lib/queries";
import { isFree } from "../pages/models/price";
import { Badge, cx } from "./ui";
import { Checkbox } from "./atoms";
import { compareModels } from "../lib/models";

/**
 * Multi-select over model refs, grouped by provider. Built for a permission list
 * rather than a single choice: an empty selection means "any model", so the list
 * stays filterable and scrolls instead of pushing the dialog past the viewport.
 */
export function ModelPicker({
  selected,
  onChange,
  disabled,
}: {
  selected: string[];
  onChange: (refs: string[]) => void;
  disabled?: boolean;
}) {
  const models = useModels();
  const providers = useModelProviders();
  const [query, setQuery] = useState("");

  const chosen = useMemo(() => new Set(selected), [selected]);
  const display = useMemo(
    () => new Map((providers.data ?? []).map((p) => [p.id, p.display])),
    [providers.data],
  );
  const groups = useMemo(() => group(models.data, query), [models.data, query]);
  /** Selected refs the registry no longer lists: offer them so they can be removed. */
  const missing = useMemo(() => {
    const known = new Set((models.data ?? []).map((m) => m.ref));
    return selected.filter((ref) => !known.has(ref)).sort();
  }, [models.data, selected]);

  const toggle = (ref: string) => {
    const next = new Set(chosen);
    if (next.has(ref)) next.delete(ref);
    else next.add(ref);
    onChange([...next].sort());
  };

  return (
    <div className="flex min-w-0 flex-col gap-1.5">
      <div className="flex items-baseline justify-between gap-3">
        <span className="text-small font-medium text-muted">Allowed models</span>
        <span className="text-caption text-faint tnum" aria-live="polite">
          {selected.length} of {models.data?.length ?? 0} selected
        </span>
      </div>

      <input
        type="search"
        value={query}
        disabled={disabled}
        aria-label="filter models"
        placeholder="filter models (or type free)"
        onChange={(e) => setQuery(e.target.value)}
        className="h-8 w-full min-w-0 rounded-[var(--radius-input)] border border-line-strong bg-surface px-2.5 text-body text-fg placeholder:text-faint hover:border-[var(--color-fg)]/40 focus:border-[var(--color-accent-text)] focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent-text)]/25 disabled:bg-surface-3 disabled:text-muted"
      />

      <div className="overflow-hidden rounded-[var(--radius-card)] border border-line bg-surface-2">
        <div className="flex items-center gap-2 border-b border-line px-2.5 py-1.5 text-small text-fg">
          <Checkbox
            checked={selected.length === 0}
            disabled={disabled}
            onChange={() => onChange([])}
            label="Any model"
          />
          <span className="text-caption text-faint">no restriction</span>
        </div>

        <div className="max-h-[220px] overflow-auto py-1">
          {models.isLoading ? (
            <div className="px-2.5 py-3 text-small text-faint">loading models…</div>
          ) : models.isError ? (
            <div className="px-2.5 py-3 text-small text-neg">
              {models.error instanceof Error ? models.error.message : String(models.error)}
            </div>
          ) : groups.length === 0 && missing.length === 0 ? (
            <div className="px-2.5 py-3 text-small text-faint">
              {query ? "no models match this filter" : "no models listed yet"}
            </div>
          ) : (
            <>
              {missing.length > 0 && !query ? (
                <Group label="Unlisted">
                  {missing.map((ref) => (
                    <Row
                      key={ref}
                      ref_={ref}
                      label={ref}
                      checked
                      disabled={disabled}
                      onToggle={() => toggle(ref)}
                    />
                  ))}
                </Group>
              ) : null}
              {groups.map((bucket) => (
                <Group key={bucket.provider} label={display.get(bucket.provider) ?? bucket.provider}>
                  {bucket.models.map((m) => (
                    <Row
                      key={m.ref}
                      ref_={m.ref}
                      label={m.display ?? m.id}
                      dim={!m.enabled}
                      free={isFree(m.cost)}
                      deprecated={m.deprecated}
                      unavailable={m.enabled && !m.available}
                      checked={chosen.has(m.ref)}
                      disabled={disabled}
                      onToggle={() => toggle(m.ref)}
                    />
                  ))}
                </Group>
              ))}
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function Group({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div>
      <div className="sticky top-0 z-10 bg-surface-2 px-2.5 py-1 text-caption font-medium text-muted">
        {label}
      </div>
      {children}
    </div>
  );
}

function Row({
  ref_,
  label,
  checked,
  disabled,
  dim,
  free,
  deprecated,
  unavailable,
  onToggle,
}: {
  ref_: string;
  label: string;
  checked: boolean;
  disabled?: boolean;
  dim?: boolean;
  free?: boolean;
  deprecated?: boolean;
  unavailable?: boolean;
  onToggle: () => void;
}) {
  return (
    <label
      className="flex min-w-0 items-center gap-2 px-2.5 py-1 text-small hover:bg-[var(--color-row-hover)]"
      title={ref_}
    >
      <Checkbox checked={checked} disabled={disabled} onChange={onToggle} />
      <span className={cx("truncate", dim ? "text-faint" : "text-muted")}>{label}</span>
      {free ? <Badge tone="pos">Free</Badge> : null}
      {unavailable ? <Badge tone="warn">unavailable</Badge> : null}
      {deprecated ? (
        <Badge tone="warn" title="this family is retired by its provider">
          deprecated
        </Badge>
      ) : null}
    </label>
  );
}

/** Models grouped under their provider, narrowed by ref, display name, or "free". */
function group(models: Model[] | undefined, query: string) {
  const needle = query.trim().toLowerCase();
  const buckets = new Map<string, Model[]>();
  for (const m of models ?? []) {
    if (needle) {
      const matchFree = needle === "free" && isFree(m.cost);
      const matchText =
        m.ref.toLowerCase().includes(needle) ||
        (m.display ?? "").toLowerCase().includes(needle);
      if (!matchFree && !matchText) continue;
    }
    const list = buckets.get(m.provider);
    if (list) list.push(m);
    else buckets.set(m.provider, [m]);
  }
  return [...buckets.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([provider, list]) => ({
      provider,
      models: [...list].sort(compareModels),
    }));
}
