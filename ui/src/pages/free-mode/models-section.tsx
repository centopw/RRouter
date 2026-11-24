import { useMemo, useState } from "react";
import { CaretDown } from "@phosphor-icons/react";
import { Link } from "react-router-dom";
import { ProviderIcon } from "../../components/provider-icon";
import {
  Button,
  Card,
  CopyIconButton,
  Dot,
  FilterChip,
  Heading,
  Toggle,
  cx,
} from "../../components/ui";
import { ago, ms, when } from "../../lib/format";
import {
  authModeLabel,
  compactEndpoint,
  filterUsableModels,
  modelKey,
  modelStatus,
  partitionModels,
  type FreeModel,
  type ModelFilter,
} from "./model";

function StatusCell({
  row,
  rowError,
}: {
  row: FreeModel;
  rowError: string | null;
}) {
  const status = modelStatus(row);
  const probe = row.probe;
  const detail = rowError ?? status.detail;
  const labelTone =
    status.tone === "pos"
      ? "text-[var(--color-pos-ink)]"
      : status.tone === "warn"
        ? "text-[var(--color-warn-ink)]"
        : status.tone === "neg"
          ? "text-[var(--color-neg-ink)]"
          : "text-muted";

  return (
    <div className="min-w-0">
      <div className="flex flex-wrap items-center gap-x-1.5 gap-y-0.5 text-small tnum">
        <Dot tone={rowError ? "neg" : status.tone} className="shrink-0" />
        <span className={cx("font-medium", rowError ? "text-[var(--color-neg-ink)]" : labelTone)}>
          {rowError ? "probe error" : status.label}
        </span>
        {probe?.ok && !rowError ? (
          <>
            <span aria-hidden className="text-faint">
              ·
            </span>
            <span className="text-muted">{ms(probe.latency_ms)}</span>
          </>
        ) : null}
        {probe && probe.at > 0 && !rowError ? (
          <>
            <span aria-hidden className="text-faint">
              ·
            </span>
            <span className="text-caption text-faint" title={when(probe.at)}>
              {ago(probe.at)}
            </span>
          </>
        ) : null}
      </div>

      {detail || status.showAddKeyLink ? (
        <p className="mt-1 break-words text-caption leading-relaxed text-muted">
          {detail ? <span>{detail}</span> : null}
          {detail && status.showAddKeyLink ? " " : null}
          {status.showAddKeyLink ? (
            <Link
              to={`/providers?add=${encodeURIComponent(row.provider)}`}
              className="whitespace-nowrap font-medium text-[var(--color-accent-text)] underline underline-offset-2 hover:text-fg"
            >
              Add {/^[aeiou]/i.test(row.provider_display) ? "an" : "a"} {row.provider_display} key →
            </Link>
          ) : null}
        </p>
      ) : null}
    </div>
  );
}

export function ModelsSection({
  models,
  loading,
  refreshing,
  onToggleModel,
  isRowTogglePending,
  onProbeModel,
  isRowProbing,
  errorForRow,
  onRefresh,
}: {
  models: FreeModel[];
  loading: boolean;
  refreshing: boolean;
  onToggleModel: (row: FreeModel, enabled: boolean) => void;
  isRowTogglePending: (provider: string, model: string) => boolean;
  onProbeModel: (row: FreeModel) => void;
  isRowProbing: (provider: string, model: string) => boolean;
  errorForRow: (provider: string, model: string) => string | null;
  onRefresh: () => void;
}) {
  const [filter, setFilter] = useState<ModelFilter>("all");
  const [providerFilter, setProviderFilter] = useState<string | null>(null);
  const [showUnusable, setShowUnusable] = useState(false);

  const buckets = useMemo(() => partitionModels(models), [models]);

  // If the chosen provider disappears after a refresh, reset cleanly to all providers.
  const activeProvider =
    providerFilter !== null && buckets.providers.some((entry) => entry.id === providerFilter)
      ? providerFilter
      : null;

  const activeFilter: ModelFilter =
    filter === "needs_key" && buckets.counts.needs_key === 0
      ? "all"
      : filter === "off" && buckets.counts.off === 0
        ? "all"
        : filter;

  const visible = useMemo(
    () => filterUsableModels(buckets.usable, activeFilter, activeProvider),
    [buckets.usable, activeFilter, activeProvider],
  );

  const providerTarget = (row: FreeModel) =>
    row.has_account ? "/providers" : `/providers?add=${encodeURIComponent(row.provider)}`;

  return (
    <Card>
      <div className="flex flex-col gap-3 px-5 pt-4 pb-3 sm:flex-row sm:items-center sm:justify-between">
        <Heading level={3} className="shrink-0">
          Free models
        </Heading>
        <div className="flex flex-wrap items-center gap-1.5" role="group" aria-label="Filter free models">
          <FilterChip
            on={activeFilter === "all"}
            onClick={() => setFilter("all")}
            ariaLabel={`All usable models (${buckets.counts.all})`}
          >
            All <span className="tnum opacity-80">{buckets.counts.all}</span>
          </FilterChip>
          <FilterChip
            on={activeFilter === "working"}
            onClick={() => setFilter("working")}
            ariaLabel={`Working models (${buckets.counts.working})`}
          >
            Working <span className="tnum opacity-80">{buckets.counts.working}</span>
          </FilterChip>
          {buckets.counts.needs_key > 0 ? (
            <FilterChip
              on={activeFilter === "needs_key"}
              onClick={() => setFilter("needs_key")}
              ariaLabel={`Models needing a provider key (${buckets.counts.needs_key})`}
            >
              Needs key <span className="tnum opacity-80">{buckets.counts.needs_key}</span>
            </FilterChip>
          ) : null}
          {buckets.counts.off > 0 ? (
            <FilterChip
              on={activeFilter === "off"}
              onClick={() => setFilter("off")}
              ariaLabel={`Switched-off models (${buckets.counts.off})`}
            >
              Off <span className="tnum opacity-80">{buckets.counts.off}</span>
            </FilterChip>
          ) : null}
        </div>
      </div>

      {buckets.providers.length > 1 ? (
        <div
          className="flex flex-wrap items-center gap-1.5 px-5 pb-3"
          role="group"
          aria-label="Filter by provider"
        >
          <FilterChip
            on={activeProvider === null}
            onClick={() => setProviderFilter(null)}
            ariaLabel="All providers"
          >
            All providers
          </FilterChip>
          {buckets.providers.map((entry) => (
            <FilterChip
              key={entry.id}
              on={activeProvider === entry.id}
              onClick={() => setProviderFilter(activeProvider === entry.id ? null : entry.id)}
              ariaLabel={`${entry.display} (${entry.count})`}
            >
              <ProviderIcon provider={entry.id} size={13} />
              {entry.display}
              <span className="tnum opacity-80">{entry.count}</span>
            </FilterChip>
          ))}
        </div>
      ) : null}

      {loading ? (
        <div className="px-5 py-6 text-small text-muted">Loading discovered free models…</div>
      ) : buckets.usable.length === 0 ? (
        <div className="flex flex-wrap items-center justify-between gap-3 border-t border-rule px-5 py-6">
          <p className="text-small text-muted">
            No usable free models discovered yet. Refresh to fetch the public catalogs.
          </p>
          <Button variant="secondary" size="sm" loading={refreshing} onClick={onRefresh}>
            Refresh
          </Button>
        </div>
      ) : visible.length === 0 ? (
        <div className="flex flex-wrap items-center justify-between gap-3 border-t border-rule px-5 py-6">
          <p className="text-small text-muted">No free models match this filter.</p>
          <Button
            variant="quiet"
            size="sm"
            onClick={() => {
              setFilter("all");
              setProviderFilter(null);
            }}
          >
            Show all
          </Button>
        </div>
      ) : (
        <>
          {/* Desktop table */}
          <div className="hidden overflow-x-auto border-t border-rule md:block">
            <table className="w-full border-collapse text-left">
              <thead>
                <tr className="border-b border-rule bg-surface-2 text-caption font-medium text-muted">
                  <th scope="col" className="w-16 py-2.5 pl-5 pr-2">
                    On
                  </th>
                  <th scope="col" className="py-2.5 px-3">
                    Model
                  </th>
                  <th scope="col" className="w-48 py-2.5 px-3">
                    Provider
                  </th>
                  <th scope="col" className="py-2.5 px-3">
                    Status
                  </th>
                  <th scope="col" className="w-24 py-2.5 pl-3 pr-5 text-right">
                    Test
                  </th>
                </tr>
              </thead>
              <tbody className="divide-y divide-rule">
                {visible.map((row) => {
                  const key = modelKey(row);
                  const pending = isRowTogglePending(row.provider, row.model);
                  const probing = isRowProbing(row.provider, row.model);
                  const rowError = errorForRow(row.provider, row.model);
                  return (
                    <tr
                      key={key}
                      className="align-top transition-colors hover:bg-[var(--color-row-hover)]"
                    >
                      <td className="py-3 pl-5 pr-2">
                        <div className="pt-0.5">
                          <Toggle
                            size="sm"
                            checked={row.enabled}
                            disabled={pending}
                            onChange={(next) => onToggleModel(row, next)}
                            label={`Enable ${row.model} on ${row.provider_display}`}
                          />
                        </div>
                      </td>

                      <td className="max-w-[22rem] py-3 px-3">
                        <div className="flex items-center gap-1 min-w-0">
                          <Link
                            to={`/models?q=${encodeURIComponent(row.model)}&status=all`}
                            className="truncate font-mono text-small font-medium text-fg hover:text-[var(--color-accent-text)] hover:underline underline-offset-2"
                            title={`Inspect ${row.model} on Models`}
                          >
                            {row.model}
                          </Link>
                          <CopyIconButton
                            value={row.ref}
                            label={`Copy ${row.ref}`}
                            className="shrink-0"
                          />
                        </div>
                        {row.endpoint ? (
                          <p
                            className="mt-0.5 truncate font-mono text-caption text-faint"
                            title={row.endpoint}
                          >
                            {compactEndpoint(row.endpoint)}
                          </p>
                        ) : null}
                      </td>

                      <td className="py-3 px-3">
                        <Link
                          to={providerTarget(row)}
                          className="inline-flex items-center gap-1.5 text-small text-fg hover:text-[var(--color-accent-text)] hover:underline underline-offset-2"
                        >
                          <ProviderIcon provider={row.provider} size={15} />
                          <span className="truncate">{row.provider_display}</span>
                        </Link>
                        <p className="mt-0.5 text-caption text-faint">{authModeLabel(row)}</p>
                      </td>

                      <td className="py-3 px-3">
                        <StatusCell row={row} rowError={rowError} />
                      </td>

                      <td className="py-3 pl-3 pr-5 text-right">
                        <Button
                          variant="quiet"
                          size="sm"
                          loading={probing}
                          disabled={probing}
                          onClick={() => onProbeModel(row)}
                          ariaLabel={`Try ${row.model}`}
                        >
                          {probing ? "Testing…" : "Try"}
                        </Button>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>

          {/* Mobile card list */}
          <ul className="divide-y divide-rule border-t border-rule md:hidden">
            {visible.map((row) => {
              const key = modelKey(row);
              const pending = isRowTogglePending(row.provider, row.model);
              const probing = isRowProbing(row.provider, row.model);
              const rowError = errorForRow(row.provider, row.model);
              return (
                <li key={key} className="flex flex-col gap-2.5 px-4 py-3.5">
                  <div className="flex items-start justify-between gap-2">
                    <div className="flex items-start gap-2.5 min-w-0">
                      <div className="pt-0.5">
                        <Toggle
                          size="sm"
                          checked={row.enabled}
                          disabled={pending}
                          onChange={(next) => onToggleModel(row, next)}
                          label={`Enable ${row.model} on ${row.provider_display}`}
                        />
                      </div>
                      <div className="min-w-0">
                        <div className="flex items-center gap-1 min-w-0">
                          <Link
                            to={`/models?q=${encodeURIComponent(row.model)}&status=all`}
                            className="truncate font-mono text-small font-medium text-fg hover:text-[var(--color-accent-text)] hover:underline underline-offset-2"
                          >
                            {row.model}
                          </Link>
                          <CopyIconButton value={row.ref} label={`Copy ${row.ref}`} />
                        </div>
                        <div className="mt-0.5 flex flex-wrap items-center gap-x-1.5 gap-y-0.5 text-caption text-faint">
                          <Link
                            to={providerTarget(row)}
                            className="inline-flex items-center gap-1 text-muted hover:text-[var(--color-accent-text)] hover:underline underline-offset-2"
                          >
                            <ProviderIcon provider={row.provider} size={13} />
                            {row.provider_display}
                          </Link>
                          <span aria-hidden>·</span>
                          <span>{authModeLabel(row)}</span>
                        </div>
                      </div>
                    </div>

                    <Button
                      variant="quiet"
                      size="sm"
                      loading={probing}
                      disabled={probing}
                      onClick={() => onProbeModel(row)}
                      ariaLabel={`Try ${row.model}`}
                    >
                      {probing ? "Testing…" : "Try"}
                    </Button>
                  </div>

                  {row.endpoint ? (
                    <p
                      className="truncate font-mono text-caption text-faint"
                      title={row.endpoint}
                    >
                      {compactEndpoint(row.endpoint)}
                    </p>
                  ) : null}

                  <StatusCell row={row} rowError={rowError} />
                </li>
              );
            })}
          </ul>
        </>
      )}

      {buckets.unusable.length > 0 ? (
        <div className="border-t border-rule bg-surface-2 px-5 py-2.5">
          <button
            type="button"
            aria-expanded={showUnusable}
            onClick={() => setShowUnusable((open) => !open)}
            className="inline-flex items-center gap-1.5 text-caption font-medium text-muted hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
          >
            <CaretDown
              size={12}
              aria-hidden
              className={cx(
                "transition-transform duration-[var(--duration-micro)]",
                showUnusable ? "rotate-180" : "",
              )}
            />
            Not usable here ({buckets.unusable.length})
          </button>

          {showUnusable ? (
            <ul className="mt-2 space-y-1.5 text-caption text-muted">
              {buckets.unusable.map((row) => (
                <li key={modelKey(row)} className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
                  <span className="font-mono text-fg">{row.model}</span>
                  <span className="text-faint">{row.provider_display}</span>
                  <span aria-hidden className="text-faint">
                    ·
                  </span>
                  <span>
                    {row.unusable_reason ?? "Uses a protocol RRouter does not support."}
                  </span>
                  {row.endpoint ? (
                    <span className="font-mono text-faint" title={row.endpoint}>
                      ({compactEndpoint(row.endpoint)})
                    </span>
                  ) : null}
                </li>
              ))}
            </ul>
          ) : null}
        </div>
      ) : null}
    </Card>
  );
}
