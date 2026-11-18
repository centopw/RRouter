import { useEffect, useState } from "react";
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
  SectionLabel,
  Select,
} from "../../components/ui";
import {
  api,
  MAX_CONNECTION_NAME,
  type Connection,
  type ModelProvider,
  type Team,
} from "../../lib/api";
import {
  baseUrlProblem,
  connectionLabel,
  credentialShapeOf,
  derivedIdentifiers,
  intProblem,
  needsBaseUrlOverride,
  useInvalidateProviders,
  type ConnectionTestResult,
} from "./types";

/**
 * One-level configuration surface for a connection: identity, endpoint, routing and team,
 * plus test/enable/remove. Quota detail and plan windows live on `/usage`, which this page
 * links to per row — the drawer never nests another drawer or dialog.
 */
export function ConnectionDrawer({
  connection,
  spec,
  display,
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
  const [touched, setTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<unknown>(null);
  const [savedNotice, setSavedNotice] = useState<string | null>(null);

  // Sync from the server when the row refreshes, unless the operator has edited locally.
  useEffect(() => {
    if (touched) return;
    setName(connection.name ?? "");
    setTeam(connection.team_id ?? "");
    setPriority(String(connection.priority));
    setWeight(String(connection.weight));
    setBaseUrl(connection.base_url_override ?? "");
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

  const dirty =
    nextName !== connection.name ||
    nextTeam !== connection.team_id ||
    nextBaseUrl !== (connection.base_url_override ?? "") ||
    priority.trim() !== String(connection.priority) ||
    weight.trim() !== String(connection.weight);

  const invalid =
    nameProblem !== null ||
    priorityProblem !== null ||
    weightProblem !== null ||
    baseProblem !== null;

  const currentTest = lastTest && lastTest.id === connection.id ? lastTest : null;
  const state = !connection.is_active
    ? { label: "disabled", dot: "muted" as const, text: "text-faint" }
    : connection.backoff_level > 0
      ? {
          label: `backoff ${connection.backoff_level}`,
          dot: "warn" as const,
          text: "text-[var(--color-warn)]",
        }
      : { label: "active", dot: "muted" as const, text: "text-muted" };
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
    try {
      await api.patch<Connection>(`/api/connections/${connection.id}`, {
        name: nextName,
        team_id: nextTeam,
        priority: Number(priority.trim()),
        weight: Number(weight.trim()),
        ...(nextBaseUrl !== (connection.base_url_override ?? "") && nextBaseUrl !== ""
          ? { credentials: { base_url_override: nextBaseUrl } }
          : {}),
      });
      setTouched(false);
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
          <span className="text-body font-medium text-fg">{display}</span>
          {spec?.code ? <Badge tone="neutral">{spec.code}</Badge> : null}
          <span className="text-caption text-faint">{credentialShapeOf(connection)}</span>
          <span className="ml-auto flex items-center gap-1.5 text-caption">
            <Dot tone={state.dot} />
            <span className={state.text}>{state.label}</span>
          </span>
        </section>

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
              {connection.is_active ? "Disable account" : "Enable account"}
            </Button>
            {currentTest ? (
              <span role="status" className="flex min-w-0 items-center gap-2 text-small tnum">
                <Dot tone={currentTest.ok ? "muted" : "neg"} />
                <span
                  className={
                    currentTest.ok ? "text-muted" : "text-[var(--color-neg)]"
                  }
                >
                  {currentTest.ok
                    ? `works · ${currentTest.status} · ${currentTest.latency_ms}ms`
                    : `failed · ${currentTest.status}${currentTest.error ? ` · ${currentTest.error}` : ""}`}
                </span>
              </span>
            ) : null}
          </section>
        ) : null}

        {isAdmin ? (
          <>
            <section className="flex flex-col gap-4">
              <SectionLabel>Identity & endpoint</SectionLabel>
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
              <SectionLabel>Routing & team</SectionLabel>
              <p className="text-small leading-relaxed text-muted">
                Priority groups accounts (lower first); weight only splits traffic inside one
                priority group when{" "}
                <span className="font-mono text-caption text-fg">RROUTER_STRATEGY=weighted</span>.
              </p>
              <Field
                label="Owner team"
                hint="A team account serves only that team's keys; shared serves instance-wide keys and is invisible to team keys."
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
              <div className="grid grid-cols-2 gap-3">
                <Field
                  label="Priority"
                  hint="−100…100; lower groups are tried first"
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
                  hint="1…1000; traffic share inside this priority group"
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
          </>
        ) : (
          <dl className="grid grid-cols-2 gap-3 text-small">
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
                {connection.priority} · {connection.weight}
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
