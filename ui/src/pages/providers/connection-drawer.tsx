import { useEffect, useMemo, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { SectionLabel } from "../../components/atoms";
import { ProviderIcon } from "../../components/provider-icon";
import {
  AlertBanner,
  Badge,
  Button,
  Dot,
  ErrorNote,
  Field,
  Input,
  Modal,
  Select,
  TabPanel,
  TabStrip,
  cx,
} from "../../components/ui";
import {
  api,
  MAX_CONNECTION_NAME,
  type AccountQuotaRow,
  type AccountSubscription,
  type Connection,
  type ModelProvider,
  type QuotaPingRow,
  type Team,
} from "../../lib/api";
import { usdInput } from "../../lib/format";
import { qk } from "../../lib/queries";
import {
  AutoPingPanel,
  OperatorQuotaPanel,
  WindowBar,
  type AutoPingDraft,
  type OperatorQuotaDraft,
} from "./quota-inspector";
import {
  baseUrlProblem,
  connectionLabel,
  credentialShapeOf,
  derivedIdentifiers,
  emptyQuotaRowFor,
  intProblem,
  needsBaseUrlOverride,
  useInvalidateProviders,
  type ConnectionDrawerTab,
  type ConnectionTestResult,
} from "./types";

const DRAWER_TABS: ReadonlyArray<{ id: ConnectionDrawerTab; label: string }> = [
  { id: "overview", label: "Overview & Test" },
  { id: "routing", label: "Routing & Team" },
  { id: "quotas", label: "Quotas & Auto-Ping" },
];

function quotaDraftFrom(row?: AccountQuotaRow): OperatorQuotaDraft {
  return {
    window: row?.window ?? "daily",
    requests: row && row.max_requests > 0 ? String(row.max_requests) : "",
    tokens: row && row.max_tokens > 0 ? String(row.max_tokens) : "",
    cost: row ? usdInput(row.max_cost_micros) : "",
  };
}

function pingDraftFrom(row?: QuotaPingRow): AutoPingDraft {
  return {
    mode: row?.mode ?? "off",
    anchor: row?.anchor ?? "09:00",
    timezone: row?.timezone ?? "UTC",
    windowHours: row ? String(row.window_hours) : "5",
    slots: row ? String(row.slots_per_day) : "1",
  };
}

/**
 * Unified tabbed slide-over drawer (`Modal variant="drawer"`) consolidating connection
 * inspection, health testing, identity/endpoint editing, routing priority/weight/team,
 * subscription plan windows, operator quota limits, and auto-ping scheduling.
 */
export function ConnectionDrawer({
  connection,
  spec,
  display,
  teams,
  quota,
  quotasLoaded,
  quotasLoading,
  quotasError,
  onRetryQuotas,
  subscription,
  ping,
  pings,
  pingsError,
  isAdmin,
  testing,
  toggling,
  lastTest,
  initialTab = "overview",
  onTest,
  onToggle,
  onRemove,
  onClose,
}: {
  connection: Connection;
  spec?: ModelProvider;
  display: string;
  teams: Team[];
  quota?: AccountQuotaRow;
  quotasLoaded: boolean;
  quotasLoading: boolean;
  quotasError: unknown;
  onRetryQuotas: () => void;
  subscription?: AccountSubscription;
  ping?: QuotaPingRow;
  pings: QuotaPingRow[];
  pingsError: unknown;
  isAdmin: boolean;
  testing: boolean;
  toggling: boolean;
  lastTest: ConnectionTestResult | null;
  initialTab?: ConnectionDrawerTab;
  onTest: () => void;
  onToggle: () => void;
  onRemove: () => void;
  onClose: () => void;
}) {
  const client = useQueryClient();
  const { invalidate } = useInvalidateProviders();
  const [activeTab, setActiveTab] = useState<ConnectionDrawerTab>(initialTab);

  useEffect(() => {
    setActiveTab(initialTab);
  }, [initialTab, connection.id]);

  // Connection identity + routing draft state (shared across Overview & Routing tabs)
  const [name, setName] = useState(connection.name ?? "");
  const [team, setTeam] = useState(connection.team_id ?? "");
  const [priority, setPriority] = useState(String(connection.priority));
  const [weight, setWeight] = useState(String(connection.weight));
  const [baseUrl, setBaseUrl] = useState(connection.base_url_override ?? "");
  const [connTouched, setConnTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<unknown>(null);
  const [savedNotice, setSavedNotice] = useState<string | null>(null);

  // Sync from server when connection row refreshes and user hasn't edited locally
  useEffect(() => {
    if (connTouched) return;
    setName(connection.name ?? "");
    setTeam(connection.team_id ?? "");
    setPriority(String(connection.priority));
    setWeight(String(connection.weight));
    setBaseUrl(connection.base_url_override ?? "");
  }, [connection, connTouched]);

  // Operator quota draft state (held above TabStrip so tab switches preserve edits)
  const effectiveQuotaRow = useMemo(
    () => quota ?? (quotasLoaded ? emptyQuotaRowFor(connection) : undefined),
    [quota, quotasLoaded, connection],
  );
  const [quotaDraft, setQuotaDraft] = useState<OperatorQuotaDraft>(() =>
    quotaDraftFrom(effectiveQuotaRow),
  );
  const [quotaTouched, setQuotaTouched] = useState(false);

  useEffect(() => {
    if (quotaTouched) return;
    setQuotaDraft(quotaDraftFrom(effectiveQuotaRow));
  }, [effectiveQuotaRow, quotaTouched]);

  // Auto-ping draft state (held above TabStrip so tab switches preserve edits)
  const [pingDraft, setPingDraft] = useState<AutoPingDraft>(() => pingDraftFrom(ping));
  const [pingTouched, setPingTouched] = useState(false);

  useEffect(() => {
    if (pingTouched) return;
    setPingDraft(pingDraftFrom(ping));
  }, [ping, pingTouched]);

  const label = connectionLabel(connection, display);
  const fallbackName = derivedIdentifiers(connection, display)[0]?.text ?? `${display} account`;
  const ownerTeamName =
    connection.team_id === null
      ? "Shared"
      : (teams.find((t) => t.id === connection.team_id)?.name ?? connection.team_id.slice(0, 8));

  const trimmedName = name.trim();
  const nameProblem =
    Array.from(trimmedName).length > MAX_CONNECTION_NAME
      ? `a name is limited to ${MAX_CONNECTION_NAME} characters`
      : null;
  const priorityProblem = intProblem(priority, -100, 100);
  const weightProblem = intProblem(weight, 1, 1000);
  const requiredBaseUrl = spec?.requires_base_url_override === true;
  const nextBaseUrl = baseUrl.trim();
  const baseProblem =
    connection.base_url_override && nextBaseUrl === ""
      ? "A saved base URL can be replaced but not cleared."
      : baseUrlProblem(baseUrl, requiredBaseUrl);

  const nextName = trimmedName === "" ? null : trimmedName;
  const nextTeam = team === "" ? null : team;
  const nextPriority = Number(priority.trim());
  const nextWeight = Number(weight.trim());

  const connectionDirty =
    nextName !== connection.name ||
    nextTeam !== connection.team_id ||
    nextBaseUrl !== (connection.base_url_override ?? "") ||
    priority.trim() !== String(connection.priority) ||
    weight.trim() !== String(connection.weight);

  const connectionInvalid =
    nameProblem !== null ||
    priorityProblem !== null ||
    weightProblem !== null ||
    baseProblem !== null;

  const baseQuota = quotaDraftFrom(effectiveQuotaRow);
  const quotaDirty =
    Boolean(effectiveQuotaRow) &&
    (quotaDraft.window !== baseQuota.window ||
      quotaDraft.requests.trim() !== baseQuota.requests.trim() ||
      quotaDraft.tokens.trim() !== baseQuota.tokens.trim() ||
      quotaDraft.cost.trim() !== baseQuota.cost.trim());

  const basePing = pingDraftFrom(ping);
  const pingDirty =
    Boolean(ping) &&
    (pingDraft.mode !== basePing.mode ||
      (pingDraft.mode === "schedule" &&
        (pingDraft.anchor.trim() !== basePing.anchor.trim() ||
          pingDraft.timezone.trim() !== basePing.timezone.trim() ||
          pingDraft.windowHours.trim() !== basePing.windowHours.trim() ||
          pingDraft.slots.trim() !== basePing.slots.trim())));

  const anyDirty = isAdmin && (connectionDirty || quotaDirty || pingDirty);
  const supportsPing = connection.provider === "claude" || connection.provider === "codex";
  const currentTest = lastTest && lastTest.id === connection.id ? lastTest : null;

  const saveConnection = async () => {
    if (!isAdmin || !connectionDirty || connectionInvalid || busy) return;
    setBusy(true);
    setFailure(null);
    setSavedNotice(null);
    try {
      await api.patch<Connection>(`/api/connections/${connection.id}`, {
        name: nextName,
        team_id: nextTeam,
        priority: nextPriority,
        weight: nextWeight,
        ...(nextBaseUrl !== (connection.base_url_override ?? "") && nextBaseUrl !== ""
          ? { credentials: { base_url_override: nextBaseUrl } }
          : {}),
      });
      setConnTouched(false);
      invalidate();
      void client.invalidateQueries({ queryKey: qk.accountQuotas });
      setSavedNotice("Connection updated");
    } catch (error) {
      setFailure(error);
    } finally {
      setBusy(false);
    }
  };

  const patchConnField = (updater: () => void) => {
    setConnTouched(true);
    setFailure(null);
    setSavedNotice(null);
    updater();
  };

  return (
    <Modal
      title={label}
      description={`${display}${spec?.code ? ` · ${spec.code}` : ""} · ${connection.id}`}
      variant="drawer"
      width="lg"
      open
      dirty={anyDirty}
      onClose={onClose}
      footer={
        isAdmin ? (
          <>
            <Button
              variant="danger"
              disabled={busy}
              onClick={() => {
                onClose();
                onRemove();
              }}
              className="mr-auto"
            >
              Remove
            </Button>
            <Button onClick={onClose} disabled={busy}>
              Close
            </Button>
            {activeTab !== "quotas" || connectionDirty ? (
              <Button
                variant="primary"
                loading={busy}
                disabled={!connectionDirty || connectionInvalid || busy}
                onClick={() => void saveConnection()}
              >
                {busy
                  ? "Saving…"
                  : activeTab === "quotas"
                    ? "Save connection"
                    : "Save changes"}
              </Button>
            ) : null}
          </>
        ) : (
          <Button variant="primary" onClick={onClose}>
            Close
          </Button>
        )
      }
    >
      <TabStrip
        options={DRAWER_TABS}
        value={activeTab}
        onChange={setActiveTab}
        ariaLabel="Connection inspector sections"
      >
        {/* ---- Tab 1: Overview & Test ------------------------------------------------ */}
        <TabPanel value="overview" className="mt-4 flex flex-col gap-5 focus:outline-none">
          <div className="flex flex-wrap items-center gap-2 rounded-[var(--radius-card)] border border-line bg-surface-2 px-4 py-3">
            <ProviderIcon provider={connection.provider} size={20} />
            <span className="text-body font-medium text-fg">{display}</span>
            {spec?.code ? <Badge tone="neutral">{spec.code}</Badge> : null}
            <Badge tone={connection.auth_type === "oauth" ? "accent" : "neutral"}>
              {connection.auth_type}
            </Badge>
            <Badge tone={connection.team_id ? "accent" : "neutral"}>{ownerTeamName}</Badge>
            {needsBaseUrlOverride(connection, spec ? [spec] : undefined) ? (
              <Badge tone="warn">base url</Badge>
            ) : null}
            {connection.backoff_level > 0 ? (
              <Badge tone="warn">
                <span className="tnum">backoff {connection.backoff_level}</span>
              </Badge>
            ) : connection.is_active ? (
              <Badge tone="pos">active</Badge>
            ) : (
              <Badge tone="neutral">disabled</Badge>
            )}
            <span className="ml-auto text-caption text-faint">
              {credentialShapeOf(connection)}
            </span>
          </div>

          {connection.last_error ? (
            <AlertBanner tone="neg" title="Last upstream error" className="mb-0">
              <div className="break-words font-mono text-caption text-[var(--color-neg-ink)]">
                {connection.last_error}
              </div>
            </AlertBanner>
          ) : null}

          {isAdmin ? (
            <section className="flex flex-col gap-2.5">
              <div className="flex flex-wrap items-center gap-2">
                <Button
                  variant="secondary"
                  loading={testing}
                  disabled={testing || busy}
                  onClick={onTest}
                >
                  {testing ? "Testing…" : "Test connection"}
                </Button>
                <Button
                  variant="secondary"
                  loading={toggling}
                  disabled={toggling || busy}
                  onClick={onToggle}
                >
                  {connection.is_active ? "Disable account" : "Enable account"}
                </Button>
              </div>
              {currentTest ? (
                <div
                  role="status"
                  className={cx(
                    "flex items-center gap-2 rounded-lg border px-3 py-2 text-small tnum",
                    currentTest.ok
                      ? "border-[var(--color-pos-line)] bg-[var(--color-pos-tint)] text-[var(--color-pos-ink)]"
                      : "border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] text-[var(--color-neg-ink)]",
                  )}
                >
                  <Dot tone={currentTest.ok ? "pos" : "neg"} />
                  <span>
                    {currentTest.ok
                      ? `Connection works · ${currentTest.status} · ${currentTest.latency_ms}ms`
                      : `Connection failed · ${currentTest.status}${currentTest.error ? ` · ${currentTest.error}` : ""}`}
                  </span>
                </div>
              ) : null}
            </section>
          ) : null}

          {isAdmin ? (
            <section className="flex flex-col gap-4 border-t border-line pt-4">
              <SectionLabel>Identity & Endpoint</SectionLabel>
              <Field
                label="Name"
                hint={`${MAX_CONNECTION_NAME} characters at most; empty falls back to “${fallbackName}”`}
                error={nameProblem ?? undefined}
                aside={
                  name.trim() !== "" ? (
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => patchConnField(() => setName(""))}
                      className="text-caption text-muted underline underline-offset-2 hover:text-fg disabled:opacity-50"
                    >
                      clear name
                    </button>
                  ) : null
                }
              >
                <Input
                  value={name}
                  onChange={(v) => patchConnField(() => setName(v))}
                  placeholder={fallbackName}
                  ariaLabel="inspect connection name"
                  disabled={busy}
                />
              </Field>

              <Field
                label={requiredBaseUrl ? "Resource base URL (required)" : "Base URL override"}
                hint={
                  connection.base_url_override
                    ? "Saved overrides can be replaced here, not removed — this is where the account routes."
                    : requiredBaseUrl
                      ? `${display} names your deployment, so this account must point at its own resource.`
                      : "Optional origin override for regional, self-hosted or proxy endpoints."
                }
                error={baseProblem ?? undefined}
              >
                <Input
                  value={baseUrl}
                  onChange={(v) => patchConnField(() => setBaseUrl(v))}
                  placeholder={spec?.base_url_example ?? "https://api.example.com/v1"}
                  ariaLabel="inspect connection base url"
                  mono
                  disabled={busy}
                />
              </Field>
            </section>
          ) : (
            <dl className="grid grid-cols-2 gap-3 rounded-[var(--radius-card)] border border-line bg-surface-2/60 px-4 py-3 text-small">
              <div>
                <dt className="text-caption text-faint">Account label</dt>
                <dd className="mt-0.5 font-medium text-fg">{label}</dd>
              </div>
              <div>
                <dt className="text-caption text-faint">Connection ID</dt>
                <dd className="mt-0.5 break-all font-mono text-caption text-fg">
                  {connection.id}
                </dd>
              </div>
              {connection.base_url_override ? (
                <div className="col-span-2">
                  <dt className="text-caption text-faint">Base URL override</dt>
                  <dd className="mt-0.5 break-all font-mono text-caption text-fg">
                    {connection.base_url_override}
                  </dd>
                </div>
              ) : null}
            </dl>
          )}
        </TabPanel>

        {/* ---- Tab 2: Routing & Team ------------------------------------------------- */}
        <TabPanel value="routing" className="mt-4 flex flex-col gap-5 focus:outline-none">
          <p className="text-small leading-relaxed text-muted">
            Priority groups accounts (lower first); weight only splits traffic inside one priority
            group when{" "}
            <span className="font-mono text-caption text-fg">RROUTER_STRATEGY=weighted</span>.
          </p>

          {isAdmin ? (
            <>
              <Field
                label="Owner team"
                hint="A team account serves only that team's keys; shared serves instance-wide keys and is invisible to team keys."
              >
                <Select
                  value={team}
                  onChange={(v) => patchConnField(() => setTeam(v))}
                  ariaLabel="inspect connection team"
                  disabled={busy}
                >
                  <option value="">Shared / instance-wide</option>
                  {teams.map((row) => (
                    <option key={row.id} value={row.id}>
                      {row.name}
                    </option>
                  ))}
                </Select>
              </Field>

              <p className="-mt-2 text-caption text-faint">
                Moving an account takes effect on the next request: a team key with no team-owned
                account of this provider fails with “no eligible upstream” until one is assigned.
              </p>

              <div className="grid grid-cols-2 gap-3">
                <Field
                  label="Priority"
                  hint="−100…100; lower groups are tried first"
                  error={priorityProblem ?? undefined}
                >
                  <Input
                    type="number"
                    value={priority}
                    onChange={(v) => patchConnField(() => setPriority(v))}
                    ariaLabel="inspect connection priority"
                    className="tnum"
                    disabled={busy}
                  />
                </Field>
                <Field
                  label="Weight"
                  hint="1…1000; traffic share inside this priority group"
                  error={weightProblem ?? undefined}
                >
                  <Input
                    type="number"
                    value={weight}
                    onChange={(v) => patchConnField(() => setWeight(v))}
                    ariaLabel="inspect connection weight"
                    className="tnum"
                    disabled={busy}
                  />
                </Field>
              </div>
            </>
          ) : (
            <dl className="grid grid-cols-2 gap-3 rounded-[var(--radius-card)] border border-line bg-surface-2/60 px-4 py-3 text-small">
              <div>
                <dt className="text-caption text-faint">Owner scope</dt>
                <dd className="mt-0.5 font-medium text-fg">{ownerTeamName}</dd>
              </div>
              <div>
                <dt className="text-caption text-faint">Priority</dt>
                <dd className="mt-0.5 font-medium text-fg tnum">{connection.priority}</dd>
              </div>
              <div>
                <dt className="text-caption text-faint">Weight</dt>
                <dd className="mt-0.5 font-medium text-fg tnum">{connection.weight}</dd>
              </div>
            </dl>
          )}

          <div className="flex items-center justify-between gap-3 border-t border-line pt-3 text-caption text-faint">
            <span>{label}</span>
            <span className="font-mono">{connection.id}</span>
          </div>
        </TabPanel>

        {/* ---- Tab 3: Quotas & Auto-Ping --------------------------------------------- */}
        <TabPanel value="quotas" className="mt-4 flex flex-col gap-5 focus:outline-none">
          <section className="flex flex-col gap-2.5 rounded-[var(--radius-card)] border border-line bg-surface-2/40 p-4">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <SectionLabel>Plan windows</SectionLabel>
              {subscription?.plan ? <Badge tone="neutral">{subscription.plan}</Badge> : null}
            </div>
            {subscription && subscription.windows.length > 0 ? (
              <div className="flex flex-col gap-1.5">
                {subscription.windows.map((window) => (
                  <WindowBar key={window.id} window={window} />
                ))}
              </div>
            ) : subscription?.error ? (
              <p className="text-small text-muted">{subscription.error}</p>
            ) : (
              <p className="text-small text-muted">
                No subscription plan windows reported for this connection.
              </p>
            )}
          </section>

          <OperatorQuotaPanel
            row={effectiveQuotaRow}
            isAdmin={isAdmin}
            loading={quotasLoading}
            error={quotasError}
            onRetry={onRetryQuotas}
            draft={quotaDraft}
            dirty={quotaDirty}
            onChange={(patch) => {
              setQuotaTouched(true);
              setQuotaDraft((prev) => ({ ...prev, ...patch }));
            }}
            onCommitted={() => setQuotaTouched(false)}
          />

          <AutoPingPanel
            row={ping}
            rows={pings}
            isAdmin={isAdmin}
            supportsPing={supportsPing}
            pingsError={pingsError}
            draft={pingDraft}
            dirty={pingDirty}
            onChange={(patch) => {
              setPingTouched(true);
              setPingDraft((prev) => ({ ...prev, ...patch }));
            }}
            onCommitted={() => setPingTouched(false)}
          />
        </TabPanel>
      </TabStrip>

      {savedNotice ? (
        <p role="status" className="mt-4 text-small text-[var(--color-pos-ink)]">
          {savedNotice}
        </p>
      ) : null}
      {failure ? (
        <div className="mt-4">
          <ErrorNote error={failure} />
        </div>
      ) : null}
    </Modal>
  );
}
