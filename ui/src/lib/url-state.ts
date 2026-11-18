import { useCallback, useDeferredValue, useEffect, useMemo, useRef, useState } from "react";
import { useSearchParams, type SetURLSearchParams } from "react-router-dom";

export type UrlFilterDefaults = Record<string, string>;

export type UseUrlFilterStateOptions<T extends UrlFilterDefaults> = {
  /** Default values for each managed query parameter. Defaulted/empty values are omitted from the URL. */
  defaults: T;
  /** Optional key managed as a debounced text search field (e.g. `"q"`). */
  searchKey?: keyof T & string;
  /** Debounce duration in milliseconds before committing `searchKey` to the URL (default `200`). */
  debounceMs?: number;
  /** Optional pagination key (e.g. `"page"`) reset to its default when search changes or `resetPage` is true. */
  pageKey?: keyof T & string;
  /** Optional normalizers applied when reading each raw URL parameter. */
  sanitize?: Partial<{ [K in keyof T]: (raw: string) => T[K] }>;
  /** Custom check for values that represent defaults and should be removed from the URL. */
  isDefault?: <K extends keyof T & string>(key: K, value: string, defaults: T) => boolean;
};

export type UrlFilterPatch<T extends UrlFilterDefaults> = Partial<Record<keyof T & string, string | null>>;

export type UrlFilterState<T extends UrlFilterDefaults> = {
  params: URLSearchParams;
  setParams: SetURLSearchParams;
  values: T;
  searchDraft: string;
  setSearchDraft: (next: string) => void;
  appliedSearch: string;
  hasActiveFilters: boolean;
  setFilter: <K extends keyof T & string>(
    key: K,
    value: string | null,
    options?: { replace?: boolean; resetPage?: boolean },
  ) => void;
  setFilters: (patch: UrlFilterPatch<T>, replace?: boolean) => void;
  resetFilters: (keys?: ReadonlyArray<keyof T & string>, replace?: boolean) => void;
};

/**
 * URL-synced search and filter state backed by `react-router-dom`'s `useSearchParams`.
 *
 * Keeps default values out of the query string, preserves unrelated URL parameters
 * (such as `?preset=` or `?inspect=`), and optionally debounces a search input with
 * `history.replaceState` semantics so typing does not pollute browser history.
 */
export function useUrlFilterState<T extends UrlFilterDefaults>({
  defaults,
  searchKey,
  debounceMs = 200,
  pageKey,
  sanitize,
  isDefault,
}: UseUrlFilterStateOptions<T>): UrlFilterState<T> {
  const [params, setParams] = useSearchParams();

  const defaultsRef = useRef(defaults);
  defaultsRef.current = defaults;
  const sanitizeRef = useRef(sanitize);
  sanitizeRef.current = sanitize;
  const isDefaultRef = useRef(isDefault);
  isDefaultRef.current = isDefault;
  const debounceTimerRef = useRef<number | null>(null);

  const keys = Object.keys(defaults) as Array<keyof T & string>;
  const keysSignature = keys.join("|");

  const values = useMemo(() => {
    const out = {} as T;
    for (const key of keys) {
      const raw = params.get(key);
      if (raw === null) {
        out[key] = defaults[key];
      } else {
        const normalizer = sanitizeRef.current?.[key];
        out[key] = (normalizer ? normalizer(raw) : raw) as T[typeof key];
      }
    }
    return out;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [params, keysSignature]);

  const checkDefault = useCallback(<K extends keyof T & string>(key: K, value: string | null): boolean => {
    const currentDefaults = defaultsRef.current;
    if (value === null || value === "") return true;
    if (value === currentDefaults[key]) return true;
    return isDefaultRef.current ? isDefaultRef.current(key, value, currentDefaults) : false;
  }, []);

  const setFilters = useCallback(
    (patch: UrlFilterPatch<T>, replace = false) => {
      setParams(
        (prev) => {
          const next = new URLSearchParams(prev);
          for (const [key, value] of Object.entries(patch) as Array<[keyof T & string, string | null | undefined]>) {
            if (value === undefined) continue;
            if (value === null || checkDefault(key, value)) next.delete(key);
            else next.set(key, value);
          }
          return next;
        },
        { replace },
      );
    },
    [setParams, checkDefault],
  );

  const setFilter = useCallback(
    <K extends keyof T & string>(
      key: K,
      value: string | null,
      options?: { replace?: boolean; resetPage?: boolean },
    ) => {
      const patch: UrlFilterPatch<T> = { [key]: value } as UrlFilterPatch<T>;
      if (options?.resetPage && pageKey && pageKey !== key) {
        patch[pageKey] = null;
      }
      setFilters(patch, options?.replace ?? false);
    },
    [setFilters, pageKey],
  );

  const urlSearch: string = searchKey ? (values[searchKey] ?? "") : "";
  const [searchDraft, setSearchDraft] = useState<string>(urlSearch);

  useEffect(() => {
    setSearchDraft(urlSearch);
  }, [urlSearch]);

  useEffect(() => {
    if (!searchKey || searchDraft === urlSearch) return;
    if (debounceTimerRef.current !== null) window.clearTimeout(debounceTimerRef.current);
    debounceTimerRef.current = window.setTimeout(() => {
      debounceTimerRef.current = null;
      const patch: UrlFilterPatch<T> = {
        [searchKey]: searchDraft,
      } as UrlFilterPatch<T>;
      if (pageKey && pageKey !== searchKey) {
        patch[pageKey] = null;
      }
      setFilters(patch, true);
    }, debounceMs);
    return () => {
      if (debounceTimerRef.current !== null) {
        window.clearTimeout(debounceTimerRef.current);
        debounceTimerRef.current = null;
      }
    };
  }, [searchKey, searchDraft, urlSearch, debounceMs, pageKey, setFilters]);

  const appliedSearch: string = useDeferredValue(urlSearch);

  const resetFilters = useCallback(
    (targetKeys?: ReadonlyArray<keyof T & string>, replace = false) => {
      const toReset = targetKeys ?? (Object.keys(defaultsRef.current) as Array<keyof T & string>);
      if (searchKey && toReset.includes(searchKey)) {
        if (debounceTimerRef.current !== null) {
          window.clearTimeout(debounceTimerRef.current);
          debounceTimerRef.current = null;
        }
        setSearchDraft(defaultsRef.current[searchKey] ?? "");
      }
      const patch: UrlFilterPatch<T> = {};
      for (const key of toReset) {
        patch[key] = null;
      }
      setFilters(patch, replace);
    },
    [searchKey, setFilters],
  );

  const hasActiveFilters = useMemo(
    () => keys.some((key) => !checkDefault(key, values[key] ?? null)),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [keysSignature, values, checkDefault],
  );

  return {
    params,
    setParams,
    values,
    searchDraft,
    setSearchDraft,
    appliedSearch,
    hasActiveFilters,
    setFilter,
    setFilters,
    resetFilters,
  };
}
