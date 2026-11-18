import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import {
  AlertBanner,
  Badge,
  Button,
  Card,
  Divider,
  EmptyState,
  ErrorNote,
  FilterChip,
  Heading,
  PageHeader,
  Segmented,
  Select,
  cx,
  statusText,
} from "../../components/ui";
import { pct } from "../../lib/format";
import { RANGES, incidentOf, rangeOf, type Incident, type RangeId } from "../../lib/activity";
import {
  useConnections,
  useIsAdmin,
  useKeys,
  useLiveUsage,
  useTeams,
  useUsage,
  useUsageSummary,
} from "../../lib/queries";
import type { UsageEvent } from "../../lib/api";
import { LOG_PAGE, RequestDetailDialog, RequestLog } from "./request-log";
import { LiveRoutingCard } from "../../components/live-routing";
import { Onboarding } from "./onboarding";

/** Dismissed incident survives a route change or reload inside the tab. */
const DISMISSED_INCIDENT = "rrouter.activity.dismissed-incident";

/**
 * Events, not aggregates. Spend, totals and per-model consumption belong to /usage;
 * this page answers "what happened, to which request, and what failed".
 */
export default function Activity() {
  const navigate = useNavigate();
  const [rangeId, setRangeId] = useState<RangeId>("7d");
  const [providerFilter, setProviderFilter] = useState<string>("");
  const [modelFilter, setModelFilter] = useState<string>("");
  const [keyFilter, setKeyFilter] = useState<string>("");
  const [teamFilter, setTeamFilter] = useState<string | null>(null);
  const [errorsOnly, setErrorsOnly] = useState(false);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [showAll, setShowAll] = useState(false);
  const [dismissedIncident, setDismissedIncident] = useState<string | null>(() =>
    sessionStorage.getItem(DISMISSED_INCIDENT),
  );
  const logRef = useRef<HTMLDivElement>(null);

  const spec = rangeOf(rangeId);

  // Tenancy filter: an admin narrows the whole page to one team; a reader's choices are
  // already their own teams, and the server scopes the unfiltered view anyway.
  const teams = useTeams();
  const teamParam = teamFilter ?? undefined;
  const usage = useUsage(200, spec.hours, teamParam);
  // Per-model window totals: the only server-side source for incident detection and
  // for the model filter's options. Not rendered as a table — /usage owns aggregates.
  const summary = useUsageSummary(spec.hours, teamParam);
  // Key names and the provider roster come from admin surface. A reader gets the log
  // and the routing card instead of requests that would be refused.
  const isAdmin = useIsAdmin();
  const keys = useKeys(isAdmin);
  // One row proves the gateway has ever served traffic; the window-scoped
  // queries cannot tell "never used" from "quiet right now".
  const ever = useUsage(1);
  const live = useLiveUsage(200, teamParam);
  const connections = useConnections(isAdmin);

  const providerIds = useMemo(() => {
    const seen = isAdmin
      ? (connections.data ?? []).filter((c) => c.is_active).map((c) => c.provider)
      : (live.data ?? []).map((event) => event.provider);
    return [...new Set(seen)].sort();
  }, [isAdmin, connections.data, live.data]);
  // The log stores ids; the rosters turn them into the labels the operator knows.
  // Readers get no roster, so a row falls back to its recorded provider and a dash.
  const keyNames = useMemo(
    () => new Map((keys.data ?? []).map((k) => [k.id, k.name ?? "—"] as const)),
    [keys.data],
  );
  const providerById = useMemo(
    () => new Map((connections.data ?? []).map((c) => [c.id, c.name ?? c.provider] as const)),
    [connections.data],
  );
  const providerOf = useCallback(
    (row: UsageEvent) =>
      (row.connection_id ? providerById.get(row.connection_id) : undefined) ?? row.provider,
    [providerById],
  );

  const events = usage.data ?? [];
  const failedEvents = useMemo(() => events.filter(isFailure), [events]);
  const filtered = useMemo(
    () =>
      events.filter(
        (e) =>
          (!errorsOnly || isFailure(e)) &&
          (!providerFilter || providerOf(e) === providerFilter) &&
          (!modelFilter || splitModel(e.provider, e.model).ref === modelFilter) &&
          (!keyFilter || e.key_id === keyFilter),
      ),
    [events, errorsOnly, providerFilter, providerOf, modelFilter, keyFilter],
  );
  const logRows = useMemo(
    () => filtered.map((e) => ({ ...e, model: splitModel(e.provider, e.model).name })),
    [filtered],
  );
  // A request is opened by clicking a row; nothing is selected until then.
  const selected = filtered.find((e) => e.id !== null && e.id === selectedId) ?? null;
  const selectedRow = selected
    ? { ...selected, model: splitModel(selected.provider, selected.model).name }
    : null;
  const selectedIndex =
    selectedId === null ? -1 : logRows.findIndex((row) => row.id !== null && row.id === selectedId);
  const stepSelected = (delta: -1 | 1) => {
    if (selectedIndex < 0) return;
    const nextIndex = selectedIndex + delta;
    const target = logRows[nextIndex];
    if (!target || target.id === null) return;
    if (!showAll && nextIndex >= LOG_PAGE) setShowAll(true);
    setSelectedId(target.id);
  };

  /** Filter options come from what this page can actually show, so no dead choices. */
  const providerOptions = useMemo(() => {
    const seen = new Set<string>();
    for (const event of events) {
      const provider = providerOf(event);
      if (provider) seen.add(provider);
    }
    return [...seen].sort();
  }, [events, providerOf]);
  const modelOptions = useMemo(() => {
    const agg = new Map<string, { ref: string; provider: string; name: string; requests: number }>();
    for (const row of summary.data ?? []) {
      const { ref, name } = splitModel(row.provider, row.model);
      const entry = agg.get(ref);
      if (entry) entry.requests += row.requests;
      else agg.set(ref, { ref, provider: row.provider, name, requests: row.requests });
    }
    return [...agg.values()].sort((a, b) => b.requests - a.requests);
  }, [summary.data]);

  const incident: Incident | null = incidentOf(summary.data ?? []);
  const incidentRef = incident ? splitModel(incident.provider, incident.model).ref : null;
  const incidentFailures = useMemo(
    () =>
      incidentRef
        ? events.filter((e) => isFailure(e) && splitModel(e.provider, e.model).ref === incidentRef)
        : [],
    [events, incidentRef],
  );
  const dominant = incidentRef ? dominantStatus(incidentFailures) : null;
  const showIncident = incident !== null && incidentRef !== dismissedIncident;
  const providerKnown = incident !== null && incident.provider !== "";
  const incidentConnectionId = useMemo(() => {
    if (!incident || !providerKnown) return null;
    const fromEvent = incidentFailures.find((e) => e.connection_id)?.connection_id;
    if (fromEvent) return fromEvent;
    const matching = (connections.data ?? []).filter((c) => c.provider === incident.provider);
    const degradedMatch = matching.find(
      (c) => c.backoff_level > 0 || (c.is_active && Boolean(c.last_error?.trim())),
    );
    if (degradedMatch) return degradedMatch.id;
    return matching.length === 1 ? (matching[0]?.id ?? null) : null;
  }, [incident, providerKnown, incidentFailures, connections.data]);

  // A resolved incident clears its dismissal, so a later recurrence alerts again.
  useEffect(() => {
    if (!summary.isSuccess || incidentRef !== null || dismissedIncident === null) return;
    sessionStorage.removeItem(DISMISSED_INCIDENT);
    setDismissedIncident(null);
  }, [summary.isSuccess, incidentRef, dismissedIncident]);

  const degradedConnections = useMemo(
    () =>
      (connections.data ?? []).filter(
        (c) => c.backoff_level > 0 || (c.is_active && Boolean(c.last_error?.trim())),
      ),
    [connections.data],
  );

  const onboard = ever.isSuccess && (ever.data?.length ?? 0) === 0;
  const showPanels = !onboard;
  const baseUrl = `${window.location.origin}/v1`;

  const filtersActive =
    errorsOnly || providerFilter !== "" || modelFilter !== "" || keyFilter !== "" || teamFilter !== null;

  /** The incident's primary action stays on the page: one click, filtered in place. */
  const showFailures = () => {
    setErrorsOnly(true);
    setProviderFilter("");
    setModelFilter("");
    setKeyFilter("");
    setSelectedId(null);
    setShowAll(false);
    requestAnimationFrame(() => logRef.current?.scrollIntoView({ block: "start" }));
  };
  const clearFilters = () => {
    setErrorsOnly(false);
    setProviderFilter("");
    setModelFilter("");
    setKeyFilter("");
    setSelectedId(null);
  };

  return (
    <div className="mx-auto max-w-[1160px] max-[820px]:-mx-4">
      <PageHeader
        title="Activity"
        subtitle="Every request this gateway served, newest first. Spend and totals live in Usage."
        actions={
          <>
            <LiveStamp at={usage.dataUpdatedAt} waiting={onboard} />
            <Segmented
              options={RANGES.map((r) => ({ id: r.id, label: r.id }))}
              value={rangeId}
              onChange={(id) => {
                setRangeId(id);
                setSelectedId(null);
              }}
              ariaLabel="time range"
            />
            <Link
              to="/usage"
              className="text-small text-muted underline-offset-4 hover:text-fg hover:underline"
            >
              Open Usage →
            </Link>
          </>
        }
      />

      {incident && showPanels && showIncident ? (
        <div className="mb-5">
          <AlertBanner
            title={
              providerKnown
                ? `${incident.provider}/${incident.model} is failing`
                : `${incident.model} is failing`
            }
            onDismiss={() => {
              if (incidentRef) sessionStorage.setItem(DISMISSED_INCIDENT, incidentRef);
              setDismissedIncident(incidentRef);
            }}
            actions={
              <>
                <Button variant="primary" onClick={showFailures}>
                  Show failing requests
                </Button>
                {providerKnown ? (
                  <Button
                    variant="quiet"
                    onClick={() =>
                      navigate(
                        incidentConnectionId
                          ? `/providers?inspect=${encodeURIComponent(incidentConnectionId)}`
                          : "/providers",
                      )
                    }
                  >
                    Open providers
                  </Button>
                ) : null}
              </>
            }
          >
            <p className="tnum">
              {incident.errors} of its {incident.requests} requests{" "}
              {dominant !== null ? `returned ${dominant} ${statusText(dominant)}` : "failed"} (
              {pct(incident.rate * 100)}).
            </p>
            <p className="mt-1 text-faint">
              {providerKnown
                ? "Check the provider connection."
                : "No provider was recorded for these requests, so they failed before dispatch. Check the model id clients send and that a connection serves it."}
            </p>
          </AlertBanner>
        </div>
      ) : null}

      {isAdmin && showPanels && degradedConnections.length > 0 ? (
        <div className="mb-5">
          <AlertBanner
            tone="warn"
            title="Upstream health"
          >
            <div className="flex flex-wrap items-center gap-1.5">
              <span className="mr-1 text-muted tnum">
                {degradedConnections.length}{" "}
                {degradedConnections.length === 1 ? "connection degraded" : "connections degraded"}
              </span>
              {degradedConnections.map((c) => {
                const label = c.name ? `${c.name} (${c.provider})` : c.provider;
                return (
                  <Link
                    key={c.id}
                    to={`/providers?inspect=${encodeURIComponent(c.id)}`}
                    title={c.last_error ?? undefined}
                    className="inline-flex items-center gap-1.5 rounded-full border border-[var(--color-warn-line)] bg-surface px-2.5 py-1 text-caption font-medium text-fg transition-colors hover:border-[var(--color-warn)]"
                  >
                    <span className="max-w-[180px] truncate">{label}</span>
                    {c.backoff_level > 0 ? (
                      <Badge tone="warn">
                        <span className="font-mono tnum">backoff {c.backoff_level}</span>
                      </Badge>
                    ) : (
                      <Badge tone="neg">error</Badge>
                    )}
                    <span aria-hidden className="text-faint">
                      →
                    </span>
                  </Link>
                );
              })}
            </div>
          </AlertBanner>
        </div>
      ) : null}

      {onboard ? <Onboarding baseUrl={baseUrl} canAdmin={isAdmin} /> : null}

      {showPanels ? (
        <div className="space-y-5">
          <LiveRoutingCard
            events={live.data ?? []}
            providers={providerIds}
            loading={live.isLoading}
          />

          <Card>
            <div ref={logRef} className="scroll-mt-4" />
            <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 px-5 pb-1 pt-4">
              <div className="flex flex-wrap items-baseline gap-2">
                <Heading level={2}>Requests</Heading>
                <span className="text-small text-faint tnum">
                  latest {Math.min(200, events.length)} in this range
                </span>
              </div>
              <span className="text-small text-faint">
                Select a request to inspect it
              </span>
            </div>

            <div className="flex flex-col gap-2 px-5 py-3 min-[1100px]:flex-row min-[1100px]:items-center">
              <div className="flex shrink-0 items-center gap-2">
                <FilterChip
                  on={errorsOnly}
                  bad={failedEvents.length > 0}
                  onClick={() => {
                    setErrorsOnly((v) => !v);
                    setSelectedId(null);
                  }}
                >
                  Failures <span className="tnum">{failedEvents.length}</span>
                </FilterChip>
              </div>
              <Divider vertical className="hidden min-[1100px]:block" />
              <div className="flex min-w-0 flex-1 flex-wrap gap-2">
                {providerOptions.length > 1 ? (
                  <div className="w-[168px] max-[520px]:w-full">
                    <Select
                      ariaLabel="filter by provider"
                      value={providerFilter}
                      onChange={(v) => {
                        setProviderFilter(v);
                        setSelectedId(null);
                      }}
                      className="w-full"
                    >
                      <option value="">All providers</option>
                      {providerOptions.map((provider) => (
                        <option key={provider} value={provider}>
                          {provider}
                        </option>
                      ))}
                    </Select>
                  </div>
                ) : null}
                {modelOptions.length > 1 ? (
                  <div className="w-[196px] max-[520px]:w-full">
                    <Select
                      ariaLabel="filter by model"
                      value={modelFilter}
                      onChange={(v) => {
                        setModelFilter(v);
                        setSelectedId(null);
                      }}
                      className="w-full"
                    >
                      <option value="">All models</option>
                      {modelOptions.map((m) => (
                        <option key={m.ref} value={m.ref}>
                          {m.provider ? `${m.provider}/${m.name}` : m.name}
                        </option>
                      ))}
                    </Select>
                  </div>
                ) : null}
                {isAdmin ? (
                  <div className="w-[168px] max-[520px]:w-full">
                    <Select
                      ariaLabel="filter by key"
                      value={keyFilter}
                      onChange={(v) => {
                        setKeyFilter(v);
                        setSelectedId(null);
                      }}
                      className="w-full"
                    >
                      <option value="">All keys</option>
                      {(keys.data ?? []).map((k) => (
                        <option key={k.id} value={k.id}>
                          {k.name ?? k.id.slice(0, 8)}
                        </option>
                      ))}
                    </Select>
                  </div>
                ) : null}
                {(teams.data ?? []).length > 0 ? (
                  <div className="w-[168px] max-[520px]:w-full">
                    <Select
                      ariaLabel="filter by team"
                      value={teamFilter ?? ""}
                      onChange={(v) => {
                        setTeamFilter(v || null);
                        setSelectedId(null);
                      }}
                      className="w-full"
                    >
                      <option value="">{isAdmin ? "All teams" : "My teams"}</option>
                      {(teams.data ?? []).map((team) => (
                        <option key={team.id} value={team.id}>
                          {team.name}
                        </option>
                      ))}
                    </Select>
                  </div>
                ) : null}
              </div>
              {filtersActive ? (
                <Button variant="quiet" onClick={clearFilters}>
                  Clear
                </Button>
              ) : null}
            </div>

            <div className="border-t border-line">
              {usage.isError ? (
                <div className="p-5">
                  <ErrorNote error={usage.error} onRetry={() => void usage.refetch()} />
                </div>
              ) : usage.isLoading && !usage.data ? (
                <RowsSkeleton rows={6} />
              ) : events.length === 0 ? (
                <EmptyState>
                  No requests in this window.
                  {rangeId !== "30d" ? (
                    <div className="mt-3">
                      <Button variant="quiet" onClick={() => setRangeId("30d")}>
                        Widen to 30d
                      </Button>
                    </div>
                  ) : null}
                </EmptyState>
              ) : filtered.length === 0 ? (
                <EmptyState>
                  No loaded requests match these filters.
                  {errorsOnly && failedEvents.length === 0
                    ? " The failures shown above are older than the latest loaded requests."
                    : ""}
                  <div className="mt-3">
                    <Button variant="quiet" onClick={clearFilters}>
                      Clear filters
                    </Button>
                  </div>
                </EmptyState>
              ) : (
                <div className="pb-1">
                  <RequestLog
                    rows={logRows}
                    total={logRows.length}
                    selectedId={selected?.id ?? null}
                    onSelect={(row) => {
                      if (row.id !== null) setSelectedId(row.id);
                    }}
                    showAll={showAll}
                    onToggleAll={() => setShowAll((v) => !v)}
                    empty=""
                    providerOf={providerOf}
                    keyNames={keyNames}
                  />
                </div>
              )}
            </div>
          </Card>

          {selectedRow ? (
            <RequestDetailDialog
              row={selectedRow}
              baseUrl={baseUrl}
              keyNames={keyNames}
              providerName={providerOf(selectedRow)}
              onClose={() => setSelectedId(null)}
              onPrev={selectedIndex > 0 ? () => stepSelected(-1) : null}
              onNext={
                selectedIndex >= 0 && selectedIndex < logRows.length - 1
                  ? () => stepSelected(1)
                  : null
              }
              positionLabel={
                selectedIndex >= 0 ? `${selectedIndex + 1} / ${logRows.length}` : undefined
              }
            />
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/* ---------------------------------------------------------------- helpers */

/**
 * Usage rows may store the client's model string, which already carries the
 * provider prefix (`codex/gpt-5.6-luna`). This keeps one canonical ref without
 * ever doubling the prefix; the name is what the tables show under `provider/`.
 */
function splitModel(provider: string, model: string): { ref: string; name: string } {
  if (provider && model.startsWith(`${provider}/`)) {
    return { ref: model, name: model.slice(provider.length + 1) };
  }
  return { ref: provider ? `${provider}/${model}` : model, name: model };
}

function isFailure(row: UsageEvent): boolean {
  return row.status >= 400 || row.error !== null;
}

/** The status most of a model's failures share; null when the sample has none. */
function dominantStatus(rows: UsageEvent[]): number | null {
  if (rows.length === 0) return null;
  const counts = new Map<number, number>();
  for (const row of rows) {
    const status = row.status || 0;
    counts.set(status, (counts.get(status) ?? 0) + 1);
  }
  let best: number | null = null;
  let bestCount = 0;
  for (const [status, count] of counts) {
    if (count > bestCount) {
      best = status;
      bestCount = count;
    }
  }
  return best && best >= 400 ? best : null;
}

/**
 * The clock lives in its own component so a one-second tick never re-renders
 * the page or the selected row.
 */
function LiveStamp({ at, waiting }: { at: number; waiting: boolean }) {
  const [, setTick] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => setTick((n) => n + 1), 1000);
    return () => window.clearInterval(id);
  }, []);

  const secs = at > 0 ? Math.max(0, Math.floor((Date.now() - at) / 1000)) : 0;
  const text = waiting
    ? "Waiting for the first request"
    : at === 0
      ? "Live · connecting"
      : secs < 5
        ? "Live · updated just now"
        : `Live · updated ${secs}s ago`;

  return (
    <span className="inline-flex items-center gap-2 text-small text-muted tnum">
      <span
        aria-hidden
        className={cx(
          "h-2 w-2 rounded-full",
          waiting || at === 0 ? "bg-faint" : "bg-accent pulse",
        )}
      />
      <span>{text}</span>
    </span>
  );
}

function Skeleton({
  height,
  width = "100%",
  className,
}: {
  height: number;
  width?: number | string;
  className?: string;
}) {
  return (
    <div
      aria-hidden
      className={cx("rounded bg-[var(--color-surface-2)]", className)}
      style={{ height, width }}
    />
  );
}

function RowsSkeleton({ rows }: { rows: number }) {
  return (
    <div aria-hidden className="divide-y divide-[var(--color-rule)]">
      {Array.from({ length: rows }, (_, i) => (
        <div key={i} className="flex items-center gap-3 px-6 py-3.5">
          <Skeleton height={12} width="38%" />
          <div className="ml-auto">
            <Skeleton height={12} width={72} />
          </div>
        </div>
      ))}
    </div>
  );
}
