import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
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
} from "../../components/ui";
import {
  api,
  MAX_CONNECTION_NAME,
  type AccountQuotaRow,
  type AccountSubscription,
  type Connection,
  type ModelProvider,
  type Team,
} from "../../lib/api";
import { ms } from "../../lib/format";
import { reenableConsequence } from "../../lib/refusal";
import { paceSentence } from "../usage/headline";
import { accountRisk, isAlarming } from "../usage/pace";
import { QuotaCell as UsageQuotaCell } from "../usage/quota";
import {
  accountStatus,
  baseUrlProblem,
  connectionLabel,
  credentialShapeOf,
  derivedIdentifiers,
  freeRpmProblem,
  intProblem,
  needsBaseUrlOverride,
  proxyProblem,
  useInvalidateProviders,
  type ConnectionTestResult,
} from "./types";

const STATUS_TEXT = {
  muted: "text-muted",
  warn: "text-[var(--color-warn)]",
  neg: "text-[var(--color-neg)]",
  info: "text-[var(--color-info)]",
} as const;

/**
 * One-level inspection and configuration drawer for a connection: standing status,
 * shared limit bars (operator caps + plan windows), live test/toggle/remove, identity,
 * routing, and folded egress/free-mode settings.
 */
export function ConnectionDrawer({
  connection,
  spec,
  display,
  quota,
  subscription,
  quotaLoaded,
  teams,
  isAdmin,
  testing,
  toggling,
  lastTest,
  onTest,
  onToggle,
  onRemove,
  onClose,
}: {
  connection: Connection;
  spec?: ModelProvider;
  display: string;
  quota?: AccountQuotaRow;
  subscription?: AccountSubscription;
  quotaLoaded: boolean;
  teams: Team[];
  isAdmin: boolean;
  testing: boolean;
  toggling: boolean;
  lastTest: ConnectionTestResult | null;
  onTest: () => void;
  onToggle: () => void;
  onRemove: () => void;
  onClose: () => void;
}) {
  const { invalidate } = useInvalidateProviders();

  const [name, setName] = useState(connection.name ?? "");
  const [team, setTeam] = useState(connection.team_id ?? "");
  const [priority, setPriority] = useState(String(connection.priority));
  const [weight, setWeight] = useState(String(connection.weight));
  const [baseUrl, setBaseUrl] = useState(connection.base_url_override ?? "");
  /** Empty by design: the server only ever returns the masked proxy, never the URL. */
  const [proxy, setProxy] = useState("");
  const [clearProxy, setClearProxy] = useState(false);
  const [freeRpm, setFreeRpm] = useState(
    connection.free_rpm === null || connection.free_rpm === undefined
      ? ""
      : String(connection.free_rpm),
  );
  const [touched, setTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<unknown>(null);
  const [savedNotice, setSavedNotice] = useState<string | null>(null);

  useEffect(() => {
    if (touched) return;
    setName(connection.name ?? "");
    setTeam(connection.team_id ?? "");
    setPriority(String(connection.priority));
    setWeight(String(connection.weight));
    setBaseUrl(connection.base_url_override ?? "");
    setFreeRpm(
      connection.free_rpm === null || connection.free_rpm === undefined
        ? ""
        : String(connection.free_rpm),
    );
  }, [connection, touched]);

  const label = connectionLabel(connection, display);
  const fallbackName = derivedIdentifiers(connection, display)[0]?.text ?? `${display} account`;
  const ownerTeamName =
    connection.team_id === null
      ? "Shared"
      : (teams.find((row) => row.id === connection.team_id)?.name ?? connection.team_id.slice(0, 8));

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
  const proxyEdit = clearProxy || proxy.trim() !== "";
  const proxyNext = clearProxy ? "" : proxy.trim();
  const proxyProblemText = proxyProblem(clearProxy ? "" : proxy);
  const freeRpmNext = freeRpm.trim() === "" ? null : Number(freeRpm.trim());
  const freeRpmProblemText = freeRpmProblem(freeRpm);
  const freeRpmDirty = freeRpmNext !== (connection.free_rpm ?? null);

  const dirty =
    nextName !== connection.name ||
    nextTeam !== connection.team_id ||
    nextBaseUrl !== (connection.base_url_override ?? "") ||
    priority.trim() !== String(connection.priority) ||
    weight.trim() !== String(connection.weight) ||
    proxyEdit ||
    freeRpmDirty;

  const invalid =
    nameProblem !== null ||
    priorityProblem !== null ||
    weightProblem !== null ||
    baseProblem !== null ||
    proxyProblemText !== null ||
    freeRpmProblemText !== null;

  const currentTest = lastTest && lastTest.id === connection.id ? lastTest : null;
  const status = accountStatus(connection, quota, subscription, spec ? [spec] : undefined);
  const refusal = status.kind === "refusal" ? status.refusal : null;

  const now = Date.now();
  const risk = subscription ? accountRisk(subscription, now) : null;
  const alarmingWindows =
    risk?.windows.filter((entry) => isAlarming(entry.pace.state)) ?? [];
  const egressOpenByDefault = Boolean(
    connection.proxy || connection.free_rpm || connection.free_mode,
  );
  const showTeamField = teams.length > 0 || connection.team_id !== null;

  const patchField = (updater: () => void) => {
    setTouched(true);
    setFailure(null);
    setSavedNotice(null);
    updater();
  };

  const save = async () => {
    if (!isAdmin || !dirty || invalid || busy) return;
    setBusy(true);
    setFailure(null);
    setSavedNotice(null);
    const credentials: Record<string, unknown> = {};
    if (nextBaseUrl !== (connection.base_url_override ?? "") && nextBaseUrl !== "") {
      credentials.base_url_override = nextBaseUrl;
    }
    if (proxyEdit) credentials.proxy = proxyNext;
    if (freeRpmDirty) credentials.free_rpm = freeRpmNext;
    try {
      await api.patch<Connection>(`/api/connections/${connection.id}`, {
        name: nextName,
        team_id: nextTeam,
        priority: Number(priority.trim()),
        weight: Number(weight.trim()),
        ...(proxyEdit ? { proxy: proxyNext } : {}),
        ...(freeRpmDirty ? { free_rpm: freeRpmNext } : {}),
        ...(Object.keys(credentials).length > 0 ? { credentials } : {}),
      });
      setTouched(false);
      setProxy("");
      setClearProxy(false);
      invalidate();
      setSavedNotice("Connection updated");
    } catch (error) {
      setFailure(error);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title={label}
      description={`${display}${spec?.code ? ` · ${spec.code}` : ""} · ${connection.id}`}
      variant="drawer"
      width="lg"
      open
      dirty={isAdmin && dirty}
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
            <Button
              variant="primary"
              loading={busy}
              disabled={!dirty || invalid || busy}
              onClick={() => void save()}
            >
              {busy ? "Saving…" : "Save changes"}
            </Button>
          </>
        ) : (
          <Button variant="primary" onClick={onClose}>
            Close
          </Button>
        )
      }
    >
      <div className="flex flex-col gap-5">
        <section className="flex flex-wrap items-center gap-2 border-b border-line pb-4">
          <ProviderIcon provider={connection.provider} size={20} />
          <Link
            to={`/models?provider=${encodeURIComponent(connection.provider)}`}
            onClick={onClose}
            className="text-body font-medium text-fg underline-offset-2 hover:text-[var(--color-accent-text)] hover:underline"
          >
            {display}
          </Link>
          {spec?.code ? <Badge tone="neutral">{spec.code}</Badge> : null}
          <span className="text-caption text-faint">{credentialShapeOf(connection)}</span>
          <span className="ml-auto flex items-center gap-1.5 text-caption">
            {refusal ? (
              <Badge tone={refusal.tone}>{refusal.label}</Badge>
            ) : (
              <>
                <Dot tone={status.tone} />
                <span className={STATUS_TEXT[status.tone]}>{status.label}</span>
              </>
            )}
          </span>
        </section>

        {refusal ? (
          <AlertBanner tone={refusal.tone} title={refusal.title} className="mb-0">
            <p>{refusal.description}</p>
            <p className="mt-1.5">{refusal.fix}</p>
            <p className="mt-1.5 font-medium text-ink-soft">{reenableConsequence(refusal)}</p>
          </AlertBanner>
        ) : null}

        {connection.last_error ? (
          <AlertBanner tone="neg" title="Last upstream error" className="mb-0">
            <div className="break-words font-mono text-caption text-[var(--color-neg-ink)]">
              {connection.last_error}
            </div>
          </AlertBanner>
        ) : null}

        {isAdmin ? (
          <section className="flex flex-wrap items-center gap-2">
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
              {connection.is_active
                ? "Disable account"
                : refusal
                  ? "Re-enable refused account…"
                  : "Enable account"}
            </Button>
            {currentTest ? (
              <span role="status" className="flex min-w-0 items-center gap-2 text-small tnum">
                <Dot tone={currentTest.ok ? "pos" : "neg"} />
                <span
                  className={
                    currentTest.ok ? "text-[var(--color-pos)]" : "text-[var(--color-neg)]"
                  }
                >
                  {currentTest.ok
                    ? `works · ${currentTest.status} · ${ms(currentTest.latency_ms)}`
                    : `failed${currentTest.status ? ` · ${currentTest.status}` : ""}${currentTest.error ? ` · ${currentTest.error}` : ""}`}
                </span>
              </span>
            ) : null}
          </section>
        ) : null}

        {/* Shared limits section from /usage: operator caps, plan windows, and pace warnings. */}
        <section className="flex flex-col gap-2.5 border-t border-line pt-4">
          <div className="flex items-baseline justify-between gap-3">
            <h3 className="text-small font-semibold text-fg">Limits</h3>
            <Link
              to={`/usage?provider=${encodeURIComponent(connection.provider)}`}
              onClick={onClose}
              className="text-caption font-medium text-[var(--color-accent-text)] underline-offset-2 hover:underline"
            >
              Usage &amp; history →
            </Link>
          </div>
          <UsageQuotaCell
            quota={quota}
            subscription={subscription}
            loaded={quotaLoaded}
          />
          {alarmingWindows.length > 0 ? (
            <ul className="flex flex-col gap-1 text-caption text-[var(--color-warn)] tnum">
              {alarmingWindows.map((entry) => {
                const line = paceSentence(entry.pace, now);
                if (!line) return null;
                return (
                  <li key={entry.window.id}>
                    {entry.window.label}: {line}
                  </li>
                );
              })}
            </ul>
          ) : null}
        </section>

        {isAdmin ? (
          <>
            <section className="flex flex-col gap-4 border-t border-line pt-4">
              <h3 className="text-small font-semibold text-fg">Identity &amp; endpoint</h3>
              <Field
                label="Name"
                hint={`${MAX_CONNECTION_NAME} characters at most; empty falls back to “${fallbackName}”`}
                error={nameProblem ?? undefined}
                aside={
                  name.trim() !== "" ? (
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => patchField(() => setName(""))}
                      className="text-caption text-muted underline underline-offset-2 hover:text-fg disabled:opacity-50"
                    >
                      clear name
                    </button>
                  ) : null
                }
              >
                <Input
                  value={name}
                  onChange={(value) => patchField(() => setName(value))}
                  placeholder={fallbackName}
                  ariaLabel="connection name"
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
                  onChange={(value) => patchField(() => setBaseUrl(value))}
                  placeholder={spec?.base_url_example ?? "https://api.example.com/v1"}
                  ariaLabel="connection base url"
                  mono
                  disabled={busy}
                />
              </Field>
            </section>

            <section className="flex flex-col gap-4 border-t border-line pt-4">
              <h3 className="text-small font-semibold text-fg">
                {showTeamField ? "Routing & team" : "Routing"}
              </h3>
              <p className="text-small leading-relaxed text-muted">
                Lower priority is tried first. Weight splits traffic inside one priority tier only
                under <span className="font-mono text-caption text-fg">RROUTER_STRATEGY=weighted</span>.
              </p>
              {showTeamField ? (
                <Field
                  label="Owner team"
                  hint="Team accounts serve only that team's keys; shared serves instance-wide keys."
                >
                  <Select
                    value={team}
                    onChange={(value) => patchField(() => setTeam(value))}
                    ariaLabel="connection team"
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
              ) : null}
              <div className="grid grid-cols-2 gap-3">
                <Field
                  label="Priority"
                  hint="−100…100; lower tried first"
                  error={priorityProblem ?? undefined}
                >
                  <Input
                    type="number"
                    value={priority}
                    onChange={(value) => patchField(() => setPriority(value))}
                    ariaLabel="connection priority"
                    className="tnum"
                    disabled={busy}
                  />
                </Field>
                <Field
                  label="Weight"
                  hint="1…1000; weighted strategy only"
                  error={weightProblem ?? undefined}
                >
                  <Input
                    type="number"
                    value={weight}
                    onChange={(value) => patchField(() => setWeight(value))}
                    ariaLabel="connection weight"
                    className="tnum"
                    disabled={busy}
                  />
                </Field>
              </div>
            </section>

            <details
              open={egressOpenByDefault}
              className="rounded-lg border border-line bg-surface-2/60"
            >
              <summary className="flex cursor-pointer list-none items-center justify-between gap-3 rounded-lg px-3 py-2.5 text-small font-medium text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]">
                <span>Egress &amp; free mode</span>
                <span className="text-caption font-normal text-faint">
                  {connection.proxy ? "proxy set" : "proxy · free-mode cap"}
                </span>
              </summary>
              <div className="flex flex-col gap-4 border-t border-line px-3 py-3">
                <p className="text-caption leading-relaxed text-muted">
                  A proxy set here applies to every upstream request this account makes, including
                  token refresh. The dashboard only ever displays it masked.
                </p>

                <Field
                  label="Egress proxy"
                  optional
                  hint={
                    clearProxy
                      ? "Saving removes the proxy: this account connects directly again."
                      : connection.proxy
                        ? "Saved already — leave empty to keep it, or type a new URL to replace it."
                        : "scheme://[user:pass@]host[:port] — http, https, socks5 or socks5h."
                  }
                  error={proxyProblemText ?? undefined}
                  aside={
                    connection.proxy ? (
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() =>
                          patchField(() => {
                            setClearProxy((value) => !value);
                            setProxy("");
                          })
                        }
                        className="text-caption text-muted underline underline-offset-2 hover:text-fg disabled:opacity-50"
                      >
                        {clearProxy ? "keep proxy" : "clear proxy"}
                      </button>
                    ) : null
                  }
                >
                  <Input
                    value={proxy}
                    onChange={(value) => patchField(() => setProxy(value))}
                    placeholder={
                      connection.proxy ?? (clearProxy ? "" : "http://user:pass@10.0.0.1:8080")
                    }
                    ariaLabel="connection egress proxy"
                    mono
                    disabled={busy || clearProxy}
                  />
                </Field>

                {connection.proxy ? (
                  <p className="text-caption leading-relaxed text-faint">
                    Saved proxy{" "}
                    <span className="font-mono text-muted">{connection.proxy}</span>
                    {clearProxy ? " — will be removed when this drawer saves." : " (masked)."}
                  </p>
                ) : null}

                <Field
                  label="Free-mode requests per minute"
                  optional
                  hint="Proactive cap for this account. Empty uses the free-mode default."
                  error={freeRpmProblemText ?? undefined}
                >
                  <Input
                    type="number"
                    value={freeRpm}
                    onChange={(value) => patchField(() => setFreeRpm(value))}
                    placeholder="default"
                    ariaLabel="connection free-mode requests per minute"
                    className="tnum"
                    disabled={busy}
                  />
                </Field>
              </div>
            </details>
          </>
        ) : (
          <dl className="grid grid-cols-2 gap-3 border-t border-line pt-4 text-small">
            <div>
              <dt className="text-caption text-faint">Account label</dt>
              <dd className="mt-0.5 font-medium text-fg">{label}</dd>
            </div>
            <div>
              <dt className="text-caption text-faint">Owner scope</dt>
              <dd className="mt-0.5 font-medium text-fg">{ownerTeamName}</dd>
            </div>
            <div>
              <dt className="text-caption text-faint">Priority · weight</dt>
              <dd className="mt-0.5 font-medium text-fg tnum">
                p{connection.priority} · w{connection.weight}
              </dd>
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

        {needsBaseUrlOverride(connection, spec ? [spec] : undefined) && !isAdmin ? (
          <p className="text-small text-muted">
            This account cannot route until an administrator sets its resource base URL.
          </p>
        ) : null}
      </div>

      {savedNotice ? (
        <p role="status" className="mt-4 text-small text-muted">
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
