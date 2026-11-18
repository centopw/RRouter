import { useCallback, useEffect, useState } from "react";

/**
 * Typed client-side feature flags backed by compile-time defaults and safe
 * `localStorage` overrides.
 */
export const FLAGS_STORAGE_KEY = "rrouter:flags";
const FLAGS_CHANGE_EVENT = "rrouter:flags-changed";

export const DEFAULT_FLAGS = {
  /** Unified Diagnostics hub (`/system/diagnostics`) combining Health and System Logs. */
  unifiedDiagnostics: true,
  /** In-page `PresetManagerDrawer` inside `/models`. */
  presetManagerDrawer: true,
  /** Quick Actions section inside the `⌘K` Command Palette. */
  commandPaletteActions: true,
} as const;

export type FlagMap = Record<string, boolean>;
export type FeatureFlagKey = keyof typeof DEFAULT_FLAGS;
export type FeatureFlags = Record<FeatureFlagKey, boolean>;
export type FeatureFlagOverrides = Partial<FeatureFlags>;

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Build an isolated typed flag store with `localStorage` persistence and reactive React hooks.
 */
export function createFlagStore<T extends FlagMap>(storageKey: string, defaults: T) {
  type Key = keyof T & string;
  type Overrides = Partial<Record<Key, boolean>>;

  const knownKeys = Object.keys(defaults) as Key[];

  const readOverrides = (): Overrides => {
    if (typeof window === "undefined") return {};
    try {
      const raw = window.localStorage.getItem(storageKey);
      if (!raw) return {};
      const parsed: unknown = JSON.parse(raw);
      if (!isPlainRecord(parsed)) return {};
      const out: Overrides = {};
      for (const key of knownKeys) {
        const value = parsed[key];
        if (typeof value === "boolean") out[key] = value;
      }
      return out;
    } catch {
      return {};
    }
  };

  const writeOverrides = (next: Overrides): void => {
    if (typeof window === "undefined") return;
    try {
      if (Object.keys(next).length === 0) {
        window.localStorage.removeItem(storageKey);
      } else {
        window.localStorage.setItem(storageKey, JSON.stringify(next));
      }
      window.dispatchEvent(new CustomEvent(FLAGS_CHANGE_EVENT, { detail: { storageKey } }));
    } catch {
      // Storage may be blocked in private browsing; callers still get default behavior.
    }
  };

  const getAll = (overrides?: Overrides): Record<Key, boolean> => {
    const stored = readOverrides();
    const resolved = { ...defaults } as Record<Key, boolean>;
    for (const key of knownKeys) {
      if (overrides && typeof overrides[key] === "boolean") {
        resolved[key] = overrides[key];
      } else if (typeof stored[key] === "boolean") {
        resolved[key] = stored[key];
      }
    }
    return resolved;
  };

  const get = (key: Key, overrides?: Overrides): boolean => {
    if (overrides && typeof overrides[key] === "boolean") return overrides[key];
    const stored = readOverrides();
    if (typeof stored[key] === "boolean") return stored[key];
    return Boolean(defaults[key]);
  };

  const set = (key: Key, value: boolean): Record<Key, boolean> => {
    const next = { ...readOverrides(), [key]: value };
    writeOverrides(next);
    return getAll();
  };

  const clear = (key: Key): Record<Key, boolean> => {
    const next = { ...readOverrides() };
    delete next[key];
    writeOverrides(next);
    return getAll();
  };

  const reset = (): Record<Key, boolean> => {
    writeOverrides({});
    return getAll();
  };

  const subscribe = (listener: () => void): (() => void) => {
    if (typeof window === "undefined") return () => {};
    const onCustom = (event: Event) => {
      const detail = (event as CustomEvent<{ storageKey?: string }>).detail;
      if (!detail?.storageKey || detail.storageKey === storageKey) listener();
    };
    const onStorage = (event: StorageEvent) => {
      if (event.key === null || event.key === storageKey) listener();
    };
    window.addEventListener(FLAGS_CHANGE_EVENT, onCustom);
    window.addEventListener("storage", onStorage);
    return () => {
      window.removeEventListener(FLAGS_CHANGE_EVENT, onCustom);
      window.removeEventListener("storage", onStorage);
    };
  };

  return {
    defaults,
    storageKey,
    readOverrides,
    getAll,
    get,
    set,
    clear,
    reset,
    subscribe,
  };
}

const defaultStore = createFlagStore<FeatureFlags>(FLAGS_STORAGE_KEY, { ...DEFAULT_FLAGS });

export const readFlagOverrides = defaultStore.readOverrides;
export const getFlags = defaultStore.getAll;
export const getFlag = defaultStore.get;
export const setFlag = defaultStore.set;
export const clearFlag = defaultStore.clear;
export const resetFlags = defaultStore.reset;
export const subscribeFlags = defaultStore.subscribe;

/** Read a single feature flag reactively, reflecting localStorage & same-tab updates. */
export function useFeatureFlag(key: FeatureFlagKey, override?: boolean): boolean {
  const [value, setValue] = useState<boolean>(() =>
    override !== undefined ? override : getFlag(key),
  );

  useEffect(() => {
    if (override !== undefined) {
      setValue(override);
      return;
    }
    setValue(getFlag(key));
    return subscribeFlags(() => setValue(getFlag(key)));
  }, [key, override]);

  return value;
}

/** Read and mutate all default feature flags reactively. */
export function useFeatureFlags() {
  const [flags, setFlags] = useState<FeatureFlags>(() => getFlags());

  useEffect(() => {
    setFlags(getFlags());
    return subscribeFlags(() => setFlags(getFlags()));
  }, []);

  const updateFlag = useCallback((key: FeatureFlagKey, next: boolean) => {
    setFlags(setFlag(key, next));
  }, []);

  const removeOverride = useCallback((key: FeatureFlagKey) => {
    setFlags(clearFlag(key));
  }, []);

  const resetAll = useCallback(() => {
    setFlags(resetFlags());
  }, []);

  return {
    flags,
    setFlag: updateFlag,
    clearFlag: removeOverride,
    resetFlags: resetAll,
  };
}
