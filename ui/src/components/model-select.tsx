import { useMemo } from "react";
import type { Model } from "../lib/api";
import { useModels } from "../lib/queries";
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
}: ModelSelectProps) {
  const models = useModels();
  const groups = useGroups(models.data, provider);
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
    >
      {allowEmpty !== undefined ? <option value="">{allowEmpty}</option> : null}
      {missing ? <option value={value}>{value} (missing)</option> : null}
      {groups.map(([group, items]) => (
        <optgroup key={group} label={group}>
          {items.map((m) => (
            <option key={m.ref} value={m.ref}>
              {m.id}
              {m.available ? "" : " · unavailable"}
            </option>
          ))}
        </optgroup>
      ))}
    </Select>
  );
}

function useGroups(models: Model[] | undefined, provider: string | null | undefined) {
  return useMemo(() => {
    const map = new Map<string, Model[]>();
    for (const m of models ?? []) {
      if (provider && m.provider !== provider) continue;
      const bucket = map.get(m.provider);
      if (bucket) bucket.push(m);
      else map.set(m.provider, [m]);
    }
    return [...map.entries()].sort(([a], [b]) => a.localeCompare(b));
  }, [models, provider]);
}

/** Ordered list editor used by combos: each step is a model select. */
export function ModelChain({
  models,
  onChange,
}: {
  models: string[];
  onChange: (models: string[]) => void;
}) {
  const set = (index: number, ref: string) => {
    const next = [...models];
    next[index] = ref;
    onChange(next);
  };
  const move = (index: number, delta: number) => {
    const target = index + delta;
    if (target < 0 || target >= models.length) return;
    const next = [...models];
    const [item] = next.splice(index, 1);
    if (item === undefined) return;
    next.splice(target, 0, item);
    onChange(next);
  };

  return (
    <div className="flex flex-col gap-2">
      {models.map((ref, index) => (
        <div key={`${index}-${ref}`} className="flex items-center gap-2">
          <span className="w-5 text-right text-[11px] text-faint tnum">{index + 1}</span>
          <div className={cx("min-w-0 flex-1")}>
            <ModelSelect
              ariaLabel={`model ${index + 1}`}
              value={ref}
              onChange={(v) => set(index, v)}
              allowEmpty="choose a model"
            />
          </div>
          <button
            type="button"
            aria-label={`move model ${index + 1} up`}
            onClick={() => move(index, -1)}
            className="h-8 w-7 rounded-md text-muted hover:bg-surface-2 hover:text-fg"
          >
            ↑
          </button>
          <button
            type="button"
            aria-label={`move model ${index + 1} down`}
            onClick={() => move(index, 1)}
            className="h-8 w-7 rounded-md text-muted hover:bg-surface-2 hover:text-fg"
          >
            ↓
          </button>
          <button
            type="button"
            aria-label={`remove model ${index + 1}`}
            onClick={() => onChange(models.filter((_, i) => i !== index))}
            className="h-8 w-7 rounded-md text-muted hover:bg-[var(--color-neg-tint)] hover:text-neg"
          >
            ×
          </button>
        </div>
      ))}
      <div>
        <button
          type="button"
          onClick={() => onChange([...models, ""])}
          className="text-[12px] text-muted underline underline-offset-2 hover:text-fg"
        >
          add model
        </button>
      </div>
    </div>
  );
}
