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
import { modelRefFor } from "../../lib/model-ref";
import { RANGES, incidentOf, rangeOf, type Incident, type RangeId } from "../../lib/activity";
import {
  useAccountQuotas,
  useConnections,
  useIsAdmin,
  useKeys,
  useLiveUsage,
  useModels,
  useTeams,
  useUsage,
  useUsageSummary,
} from "../../lib/queries";
import type { UsageEvent } from "../../lib/api";
import { RequestDetailDialog, RequestLog } from "./request-log";
import { QuotaTotalLine, hasLimit } from "./quota-total";
import { LiveRoutingCard } from "../../components/live-routing";
import { Onboarding } from "./onboarding";

/**
 * Dismissed incidents are remembered per model ref, on the device, forever: the user's
 * rule is "dismiss a warning once, never see that warning again". The list is bounded so
 * a device cannot accumulate refs after models stop being used.
 */
const DISMISSED_INCIDENTS = "rrouter.activity.dismissed-incidents";
const DISMISSED_LIMIT = 32;

/** A corrupt or foreign value reads as "nothing dismissed" instead of throwing. */
function readDismissed(): string[] {
  try {
    const raw = localStorage.getItem(DISMISSED_INCIDENTS);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((value): value is string => typeof value === "string");
  } catch {
    return [];
  }
}

/** Most recent first, one entry per ref, oldest pruned once the cap is reached. */
function rememberDismissed(ref: string): string[] {
  const next = [ref, ...readDismissed().filter((value) => value !== ref)].slice(
    0,
    DISMISSED_LIMIT,
  );
  try {
    localStorage.setItem(DISMISSED_INCIDENTS, JSON.stringify(next));
  } catch {
    // Storage disabled (private mode, quota): the banner returns next load, which is
    // still better than breaking the page.
  }
  return next;
}

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
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(25);
  const [dismissedIncidents, setDismissedIncidents] = useState<string[]>(readDismissed);
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
  // Canonical model refs: the registry is what turns a row's recorded spelling into the
  // one ref every surface uses (table, filter, incident banner, replay).
  const models = useModels();
  // Operator limits per account: the only place a quota is stated, and the current-window
  // consumption that goes with it.
  const quotas = useAccountQuotas(isAdmin);

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
          (!modelFilter || modelRefFor(e.provider, e.model, models.data) === modelFilter) &&
          (!keyFilter || e.key_id === keyFilter),
      ),
    [events, errorsOnly, providerFilter, providerOf, modelFilter, models.data, keyFilter],
  );
  const logRows = useMemo(
    () => filtered.map((e) => ({ ...e, model: modelRefFor(e.provider, e.model, models.data) })),
    [filtered, models.data],
  );
  // A request is opened by clicking a row; nothing is selected until then.
  const selected = filtered.find((e) => e.id !== null && e.id === selectedId) ?? null;
  const selectedRow = selected
    ? { ...selected, model: modelRefFor(selected.provider, selected.model, models.data) }
    : null;
  const selectedIndex =
    selectedId === null ? -1 : logRows.findIndex((row) => row.id !== null && row.id === selectedId);
  // Stepping past the last row of a page turns the page, so the drawer never shows a
  // request the table is not displaying.
  const pageCount = Math.max(1, Math.ceil(logRows.length / pageSize));
  const currentPage = Math.min(page, pageCount);
  const stepSelected = (delta: -1 | 1) => {
    if (selectedIndex < 0) return;
    const nextIndex = selectedIndex + delta;
    const target = logRows[nextIndex];
    if (!target || target.id === null) return;
    setSelectedId(target.id);
    setPage(Math.floor(nextIndex / pageSize) + 1);
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
    const agg = new Map<string, { ref: string; requests: number }>();
    for (const row of summary.data ?? []) {
      const ref = modelRefFor(row.provider, row.model, models.data);
      const entry = agg.get(ref);
      if (entry) entry.requests += row.requests;
      else agg.set(ref, { ref, requests: row.requests });
    }
    return [...agg.values()].sort((a, b) => b.requests - a.requests);
  }, [summary.data, models.data]);

  const incident: Incident | null = incidentOf(summary.data ?? []);
  const incidentRef = incident ? modelRefFor(incident.provider, incident.model, models.data) : null;
  const incidentFailures = useMemo(
    () =>
      incidentRef
        ? events.filter(
            (e) => isFailure(e) && modelRefFor(e.provider, e.model, models.data) === incidentRef,
          )
        : [],
    [events, incidentRef, models.data],
  );
  const dominant = incidentRef ? dominantStatus(incidentFailures) : null;
  // A dismissed ref stays dismissed; a different model failing raises its own banner.
  const showIncident =
    incident !== null && incidentRef !== null && !dismissedIncidents.includes(incidentRef);
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

  // Any change to what the log contains starts the reader back at its first page.
  useEffect(
    () => setPage(1),
    [rangeId, errorsOnly, providerFilter, modelFilter, keyFilter, teamFilter, pageSize],
  );

  /** The incident's primary action stays on the page: one click, filtered in place. */
  const showFailures = () => {
    setErrorsOnly(true);
    setProviderFilter("");
    setModelFilter("");
    setKeyFilter("");
    setSelectedId(null);
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
          </>
        }
      />

      {incident && showPanels && showIncident ? (
        <div className="mb-5">
          <AlertBanner
            title={`${incidentRef ?? incident.model} is failing`}
            onDismiss={() => {
              if (incidentRef) setDismissedIncidents(rememberDismissed(incidentRef));
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

          {isAdmin && (quotas.data ?? []).some(hasLimit) ? (
            <Card className="px-5 py-3">
              <QuotaTotalLine
                quotas={quotas.data ?? []}
                connections={connections.data ?? []}
              />
            </Card>
          ) : null}

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
                          {m.ref}
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
                <div className="px-1">
                  <RequestLog
                    rows={[]}
                    page={1}
                    size={pageSize}
                    onPage={setPage}
                    onSize={setPageSize}
                    selectedId={null}
                    onSelect={() => {}}
                    loading
                    empty=""
                    providerOf={providerOf}
                    keyNames={keyNames}
                  />
                </div>
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
                <RequestLog
                  rows={logRows}
                  page={currentPage}
                  size={pageSize}
                  onPage={setPage}
                  onSize={setPageSize}
                  selectedId={selected?.id ?? null}
                  onSelect={(row) => {
                    if (row.id !== null) setSelectedId(row.id);
                  }}
                  empty="No requests in this window."
                  providerOf={providerOf}
                  keyNames={keyNames}
                  canDelete={isAdmin}
                  onDeleted={(row) => {
                    // The inspector must not outlive the row it describes.
                    if (row.id !== null && row.id === selectedId) setSelectedId(null);
                  }}
                />
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
