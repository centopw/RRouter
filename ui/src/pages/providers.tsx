import { useMemo, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";
import { useSearchParams } from "react-router-dom";
import { ProviderIcon, providerBrand } from "../components/provider-icon";
import { Grid, SectionLabel, Stack } from "../components/atoms";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  Dot,
  ErrorNote,
  Field,
  Input,
  Modal,
  PageHeader,
  ConfirmDialog,
  Segmented,
  Select,
  cx,
} from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { OAuthDialog } from "../components/oauth-dialog";
import {
  qk,
  useAccountQuotas,
  useAccountSubscriptions,
  useClearAccountQuota,
  useConnections,
  useIsAdmin,
  useModelProviders,
  useOAuthProviders,
  useTeams,
  useUpdateAccountQuota,
  useUpdateConnection,
} from "../lib/queries";
import { apiCode } from "../lib/auth";
import {
  api,
  type AccountQuotaRow,
  type AccountQuotaWindow,
  type AccountSubscription,
  type SubscriptionWindow,
  type Connection,
  type ModelProvider,
  type OAuthProvider,
} from "../lib/api";
import { compact, parseUsd, usd, usdInput, when } from "../lib/format";
import { useToast } from "../components/toast";
import { Heading } from "../components/atoms";

type AddStep = "kind" | "provider" | "opencode" | "key";

export default function Providers() {
  const client = useQueryClient();
  const isAdmin = useIsAdmin();
  const [params, setParams] = useSearchParams();
  // `?team_id=` narrows the list inside the caller's own scope; the API is what decides
  // which teams those are, so a foreign id is refused rather than silently widened.
  const teamFilter = params.get("team_id") ?? undefined;
  const connections = useConnections(true, teamFilter);
  const providers = useModelProviders();
  // OAuth sign-in, subscription limits and operator quotas are admin surface; a reader
  // must not fire requests that would only answer 403.
  const oauth = useOAuthProviders(isAdmin);
  const teams = useTeams();

  const [adding, setAdding] = useState(false);
  const [step, setStep] = useState<AddStep>("kind");
  const [kind, setKind] = useState<"subscription" | "api_key" | null>(null);
  const [signIn, setSignIn] = useState<OAuthProvider | null>(null);
  const [keyProvider, setKeyProvider] = useState<string | null>(null);
  const [customProtocol, setCustomProtocol] = useState<"openai" | "anthropic" | null>(null);
  const [authWindow, setAuthWindow] = useState<Window | null>(null);
  const [notice, setNotice] = useState<{ tone: "pos" | "neg"; text: string } | null>(null);
  const [routing, setRouting] = useState<Connection | null>(null);
  const [baseUrlFor, setBaseUrlFor] = useState<Connection | null>(null);
  const [teamFor, setTeamFor] = useState<Connection | null>(null);
  const [quotaFor, setQuotaFor] = useState<AccountQuotaRow | null>(null);
  const [removing, setRemoving] = useState<Connection | null>(null);
  const quotas = useAccountQuotas(isAdmin);
  const subscriptions = useAccountSubscriptions(isAdmin);

  const accounts = connections.data ?? [];
  const connectedIds = useMemo(() => new Set(accounts.map((a) => a.provider)), [accounts]);
  const oauthIds = useMemo(() => new Set((oauth.data ?? []).map((p) => p.id)), [oauth.data]);

  const displayOf = (id: string) =>
    providers.data?.find((p) => p.id === id)?.display ?? id;

  /** A connection's owner team name, or `null` when it is shared. */
  const teamName = (id: string | null): string | null =>
    id === null
      ? null
      : (teams.data?.find((team) => team.id === id)?.name ?? id.slice(0, 8));

  const setTeamFilter = (team: string) => {
    const next = new URLSearchParams(params);
    if (team) next.set("team_id", team);
    else next.delete("team_id");
    setParams(next, { replace: true });
  };

  /** An account that cannot route until it names its own resource (a legacy Azure row). */
  const needsOverride = (r: Connection) =>
    providers.data?.find((p) => p.id === r.provider)?.requires_base_url_override === true &&
    !r.base_url_override;

  const invalidate = () => {
    void client.invalidateQueries({ queryKey: qk.connections });
    void client.invalidateQueries({ queryKey: qk.models });
    void client.invalidateQueries({ queryKey: qk.modelProviders });
    void client.invalidateQueries({ queryKey: qk.system });
  };

  const remove = useMutation({
    mutationFn: (id: string) => api.del<{ deleted: boolean }>(`/api/connections/${id}`),
    onSuccess: invalidate,
    onError: (e) => setNotice({ tone: "neg", text: e instanceof Error ? e.message : String(e) }),
  });

  const toggle = useMutation({
    mutationFn: ({ id, active }: { id: string; active: boolean }) =>
      api.put<Connection>(`/api/connections/${id}`, { is_active: active }),
    onSuccess: invalidate,
    onError: (e) => setNotice({ tone: "neg", text: e instanceof Error ? e.message : String(e) }),
  });

  const openAdd = () => {
    setAdding(true);
    setStep("kind");
    setKind(null);
    setKeyProvider(null);
    setCustomProtocol(null);
    setNotice(null);
  };

  const chooseKind = (k: "subscription" | "api_key") => {
    setKind(k);
    setStep("provider");
  };

  const startKey = (provider: string) => {
    setKeyProvider(provider);
    setCustomProtocol(null);
    setStep(provider === "opencode" ? "opencode" : "key");
  };

  const startCustom = (protocol: "openai" | "anthropic") => {
    setAdding(true);
    setKind("api_key");
    setKeyProvider(protocol);
    setCustomProtocol(protocol);
    setStep("key");
    setNotice(null);
  };

  // Sidebar shortcuts land here with `?custom=openai|anthropic`. A reader cannot add a
  // connection, so the deep link is dropped rather than opening a form that would 403.
  useEffect(() => {
    const custom = params.get("custom");
    if (custom !== "openai" && custom !== "anthropic") return;
    if (!isAdmin) {
      const next = new URLSearchParams(params);
      next.delete("custom");
      setParams(next, { replace: true });
      return;
    }
    startCustom(custom);
    const next = new URLSearchParams(params);
    next.delete("custom");
    setParams(next, { replace: true });
    // `startCustom` is stable for this effect's purpose: it only sets local state.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [params, setParams, isAdmin]);

  const startSignIn = (provider: OAuthProvider) => {
    // The popup must be opened during the click to survive popup blockers.
    const win = window.open("", "_blank", "width=560,height=720");
    setAuthWindow(win);
    setSignIn(provider);
    setAdding(false);
  };

  const keyProviders = (providers.data ?? []).filter((p) => !oauthIds.has(p.id));
  const subscriptionProviders = (oauth.data ?? []).map((p) => ({
    ...p,
    display: providers.data?.find((x) => x.id === p.id)?.display ?? p.display,
  }));

  const quotaColumns: Array<Column<AccountQuotaRow>> = [
    {
      key: "account",
      title: "account",
      render: (r) => (
        <span className="flex min-w-0 items-center gap-2">
          <Dot tone={r.exhausted ? "neg" : r.is_active ? "pos" : "muted"} />
          <ProviderIcon provider={r.provider} size={17} />
          <span className="font-medium">{displayOf(r.provider)}</span>
          <span className="truncate text-muted">{r.name ?? <span className="text-faint">unnamed</span>}</span>
          {r.exhausted ? <Badge tone="neg">over quota</Badge> : null}
        </span>
      ),
    },
    {
      key: "window",
      title: "window",
      render: (r) => <span className="text-muted">{WINDOW_LABEL[r.window]}</span>,
    },
    { key: "usage", title: "usage", render: (r) => <QuotaUsage row={r} /> },
    {
      key: "quota",
      title: "",
      align: "right",
      render: (r) => (
        <Button onClick={() => setQuotaFor(r)} title="requests, tokens and spend this account may use per window">
          quota
        </Button>
      ),
    },
  ];

  const columns: Array<Column<Connection>> = [
    {
      key: "provider",
      title: "provider",
      render: (r) => (
        <span className="flex items-center gap-2">
          <Dot tone={!r.is_active ? "muted" : r.backoff_level > 0 ? "warn" : "pos"} />
          <ProviderIcon provider={r.provider} size={17} />
          <span className="font-medium">{displayOf(r.provider)}</span>
        </span>
      ),
    },
    {
      key: "owner",
      title: "owner",
      width: "9rem",
      render: (r) => {
        const name = teamName(r.team_id);
        return name === null ? (
          <Badge
            tone="neutral"
            title="shared with the instance; a team key cannot use a shared account"
          >
            shared
          </Badge>
        ) : (
          <Badge tone="accent" title={`owned by team ${name}: only that team's keys route here`}>
            {name}
          </Badge>
        );
      },
    },
    {
      key: "account",
      title: "account",
      render: (r) => (
        <div className="min-w-0">
          <span className="text-muted">
            {r.name ?? r.email ?? <span className="text-faint">unnamed</span>}
          </span>
          {r.base_url_override ? (
            <div
              className="truncate font-mono text-caption text-faint"
              title={r.base_url_override}
            >
              {hostOf(r.base_url_override)}
            </div>
          ) : null}
        </div>
      ),
    },
    {
      key: "auth",
      title: "auth",
      render: (r) => (
        <span className="flex gap-1">
          <Badge tone={r.auth_type === "oauth" ? "accent" : "neutral"}>{r.auth_type}</Badge>
          {r.has_access_token ? <Badge tone="neutral">access</Badge> : null}
          {r.has_refresh_token ? <Badge tone="neutral">refresh</Badge> : null}
          {r.has_api_key ? <Badge tone="neutral">key</Badge> : null}
        </span>
      ),
    },
    { key: "priority", title: "priority", align: "right", render: (r) => r.priority },
    {
      key: "weight",
      title: "weight",
      align: "right",
      render: (r) => (
        <span
          className={r.weight === 100 ? "text-faint" : "text-fg"}
          title="traffic share inside this priority group"
        >
          {r.weight}
        </span>
      ),
    },
    {
      key: "state",
      title: "state",
      render: (r) => (
        <span className="flex flex-wrap gap-1">
          {needsOverride(r) ? (
            <Badge tone="warn" title="cannot route until this account names its resource base URL">
              base url
            </Badge>
          ) : null}
          {r.backoff_level > 0 ? (
            <Badge tone="warn">backoff {r.backoff_level}</Badge>
          ) : r.is_active ? (
            <Badge tone="pos">active</Badge>
          ) : (
            <Badge tone="neutral">disabled</Badge>
          )}
        </span>
      ),
    },
    // Mutations are admin-only at the API gate, so a reader gets no controls at all.
    ...(isAdmin
      ? [
        {
          key: "actions",
          title: "",
          align: "right",
          render: (r) => (
            <span className="flex justify-end gap-1.5">
              <Button
                onClick={() => setTeamFor(r)}
                title="move this account between a team and the shared pool"
              >
                team
              </Button>
              <Button
                onClick={() => setBaseUrlFor(r)}
                title="edit the base URL this account dispatches to"
              >
                base url
              </Button>
              <Button onClick={() => setRouting(r)} title="edit routing priority and weight">
                edit
              </Button>
              <TestButton id={r.id} />
              <Button
                onClick={() => toggle.mutate({ id: r.id, active: !r.is_active })}
                title={r.is_active ? "stop routing through this account" : "route through this account again"}
              >
                {r.is_active ? "disable" : "enable"}
              </Button>
              <Button variant="danger" onClick={() => setRemoving(r)}>
                remove
              </Button>
            </span>
          ),
        } as Column<Connection>,
      ]
      : []),
  ];

  return (
    <>
      <PageHeader
        title="Providers"
        subtitle="Accounts this gateway can route through. Each one is an upstream the router may pick."
        actions={
          <>
            {(teams.data ?? []).length > 0 ? (
              <div className="w-[180px]">
                <Select
                  ariaLabel="filter accounts by owner team"
                  value={teamFilter ?? ""}
                  onChange={setTeamFilter}
                >
                  <option value="">{isAdmin ? "all accounts" : "my teams"}</option>
                  {(teams.data ?? []).map((team) => (
                    <option key={team.id} value={team.id}>
                      {team.name}
                    </option>
                  ))}
                </Select>
              </div>
            ) : null}
            {isAdmin ? (
              <Button variant="primary" onClick={openAdd}>
                add provider
              </Button>
            ) : null}
          </>
        }
      />

      {!isAdmin ? (
        <p className="mb-3 text-small text-muted">
          Read-only: these are the provider accounts your team may route through. An
          administrator adds, edits and assigns them.
        </p>
      ) : null}
      <p className="mb-4 text-small text-muted">
        A team key routes only to its own team's accounts. A team key with no team-owned
        account fails with “no eligible upstream” until an administrator assigns one, and
        shared accounts serve instance-wide keys only.
      </p>

      <ConfirmDialog
        open={removing !== null}
        title="Remove provider account"
        message={<>Remove <span className="font-medium">{removing?.name ?? removing?.provider}</span>? Requests routed only here fall back or fail, and its stored credential is deleted.</>}
        confirmLabel="Remove account"
        busy={remove.isPending}
        onConfirm={() => {
          if (removing) remove.mutate(removing.id);
          setRemoving(null);
        }}
        onClose={() => setRemoving(null)}
      />

      {notice ? (
        <div
          className={cx(
            "mb-3 flex items-start gap-2 rounded-lg border px-3 py-2 text-body",
            notice.tone === "pos"
              ? "border-[var(--color-pos)]/35 bg-[var(--color-pos-tint)] text-[var(--color-pos)]"
              : "border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] text-[var(--color-neg-ink)]",
          )}
        >
          <span className="min-w-0 flex-1 break-words">{notice.text}</span>
          <button onClick={() => setNotice(null)} className="text-caption underline underline-offset-2">
            dismiss
          </button>
        </div>
      ) : null}

      {connections.isError ? (
        <div className="mb-4">
          {apiCode(connections.error) === "team_forbidden" ? (
            <div
              role="alert"
              className="flex items-start gap-2 rounded-lg border border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] px-3 py-2 text-body text-[var(--color-warn-ink)]"
            >
              <span className="min-w-0 flex-1">
                That team's provider accounts are not visible to this account — assignments
                are only readable by members of the owning team.
              </span>
              <button
                type="button"
                onClick={() => setTeamFilter("")}
                className="shrink-0 text-caption underline underline-offset-2"
              >
                clear filter
              </button>
            </div>
          ) : (
            <ErrorNote error={connections.error} onRetry={() => void connections.refetch()} />
          )}
        </div>
      ) : null}

      {!connections.isLoading && accounts.length === 0 ? (
        <EmptyProviders onAdd={isAdmin ? openAdd : null} />
      ) : (
        <Card>
          <CardHeader
            title="Connected"
            action={
              <span className="text-caption text-faint">
                {connectedIds.size} provider{connectedIds.size === 1 ? "" : "s"} · {accounts.length} account
                {accounts.length === 1 ? "" : "s"}
              </span>
            }
          />
          <DataTable
            columns={columns}
            rows={connections.data}
            rowKey={(r) => r.id}
            loading={connections.isLoading}
            empty="No accounts yet."
          />
        </Card>
      )}

      {isAdmin && !connections.isLoading && accounts.length > 0 ? (
        <div className="mt-4">
          <Card>
            <CardHeader
              title="Subscription limits"
              action={
                <span className="text-caption text-faint">
                  {subscriptions.isFetching ? "refreshing…" : "from your provider plan · refreshes every five minutes"}
                </span>
              }
            />
            {subscriptions.isError ? (
              <div className="px-5 pb-4">
                <ErrorNote error={subscriptions.error} onRetry={() => void subscriptions.refetch()} />
              </div>
            ) : subscriptions.isLoading ? (
              <p className="px-5 pb-4 text-small text-faint">reading plan limits…</p>
            ) : (subscriptions.data ?? []).length === 0 ? (
              <p className="px-5 pb-4 text-small text-muted">
                No subscription limits to show. Claude and Codex sign-ins report their plan windows;
                API-key accounts have none.
              </p>
            ) : (
              <div>
                {(subscriptions.data ?? []).map((account) => (
                  <SubscriptionRow key={account.connection_id} account={account} />
                ))}
              </div>
            )}
          </Card>
        </div>
      ) : null}

      {isAdmin && !connections.isLoading && accounts.length > 0 ? (
        <div className="mt-4">
          <Card>
            <CardHeader
              title="Operator limits"
              action={
                <span className="text-caption text-faint">
                  optional caps you set · an exhausted account is skipped by the router
                </span>
              }
            />
            <DataTable
              columns={quotaColumns}
              rows={quotas.data}
              rowKey={(r) => r.connection_id}
              loading={quotas.isLoading}
              empty="No accounts yet."
            />
          </Card>
        </div>
      ) : null}

      <AddProviderFlow
        open={adding}
        step={step}
        kind={kind}
        onClose={() => setAdding(false)}
        onBack={() => {
          setStep("kind");
          setKind(null);
        }}
        onChooseKind={chooseKind}
        onChooseOAuth={startSignIn}
        onChooseKey={startKey}
        onChooseCustom={startCustom}
        onOpenCodeContinue={() => setStep("key")}
        selectedKeyProvider={keyProvider}
        customProtocol={customProtocol}
        subscriptionProviders={subscriptionProviders}
        keyProviders={keyProviders}
        providers={providers.data ?? []}
        onConnected={(label, warning) => {
          setAdding(false);
          invalidate();
          setNotice({
            tone: warning ? "neg" : "pos",
            text: warning ? `${label} connected, with a warning: ${warning}` : `${label} connected`,
          });
        }}
      />

      {routing ? (
        <RoutingModal key={routing.id} connection={routing} onClose={() => setRouting(null)} />
      ) : null}

      {baseUrlFor ? (
        <BaseUrlModal
          key={baseUrlFor.id}
          connection={baseUrlFor}
          spec={providers.data?.find((p) => p.id === baseUrlFor.provider)}
          onClose={() => setBaseUrlFor(null)}
        />
      ) : null}

      {teamFor ? (
        <TeamModal
          key={teamFor.id}
          connection={teamFor}
          onClose={() => setTeamFor(null)}
        />
      ) : null}

      {quotaFor ? (
        <QuotaModal key={quotaFor.connection_id} row={quotaFor} onClose={() => setQuotaFor(null)} />
      ) : null}

      {signIn ? (
        <OAuthDialog
          provider={signIn}
          authWindow={authWindow}
          onClose={() => {
            setSignIn(null);
            setAuthWindow(null);
          }}
          onConnected={(label, warning) => {
            setSignIn(null);
            setAuthWindow(null);
            invalidate();
            setNotice({
              tone: warning ? "neg" : "pos",
              text: warning ? `${label} connected, with a warning: ${warning}` : `${label} connected`,
            });
          }}
        />
      ) : null}
    </>
  );
}

/** Whole number inside `min..=max`; the server clamps, but a silent clamp would hide typos. */
/** The host an override points at, for the account cell. */
function hostOf(url: string): string {
  return url
    .replace(/^[a-z][a-z0-9+.-]*:\/\//i, "")
    .replace(/[/?#].*$/, "")
    .replace(/^[^@]*@/, "")
    .replace(/:\d+$/, "");
}

const PLACEHOLDER_LABELS = /^(resource|your-resource|placeholder)$/i;

/**
 * Why this base URL cannot be saved, or `null`.
 *
 * Mirrors the server, so the form never offers a value the API will refuse: a provider
 * whose manifest host is a placeholder (Azure) needs the deployment's own resource URL,
 * and `RESOURCE` copied from the manifest is refused for every provider because the host
 * cannot resolve.
 */
function baseUrlProblem(value: string, required: boolean): string | null {
  const trimmed = value.trim();
  if (trimmed === "") {
    return required
      ? "This provider needs the resource endpoint of your own deployment."
      : null;
  }
  if (/<\s*resource\s*>/i.test(trimmed)) {
    return "Replace <resource> with your own resource name.";
  }
  const label = hostOf(trimmed).split(".")[0] ?? "";
  if (PLACEHOLDER_LABELS.test(label)) {
    return "The manifest host is a placeholder — replace RESOURCE with your own resource name.";
  }
  return null;
}

function intProblem(value: string, min: number, max: number): string | null {
  const text = value.trim();
  if (text === "") return "required";
  if (!/^-?\d+$/.test(text)) return "whole number only";
  const n = Number(text);
  if (!Number.isSafeInteger(n) || n < min || n > max) return `must be ${min}…${max}`;
  return null;
}

/**
 * Routing fields for one account. The strategy itself is env-only, so this modal cannot tell
 * whether weight is in play; the hint says when it is.
 */
function RoutingModal({ connection, onClose }: { connection: Connection; onClose: () => void }) {
  const update = useUpdateConnection();
  const [priority, setPriority] = useState(String(connection.priority));
  const [weight, setWeight] = useState(String(connection.weight));
  const [failure, setFailure] = useState<unknown>(null);

  const priorityProblem = intProblem(priority, -100, 100);
  const weightProblem = intProblem(weight, 1, 1000);
  const invalid = priorityProblem !== null || weightProblem !== null;
  const busy = update.isPending;

  const save = () => {
    if (invalid || busy) return;
    setFailure(null);
    update.mutate(
      { id: connection.id, patch: { priority: Number(priority.trim()), weight: Number(weight.trim()) } },
      {
        // A failed PATCH keeps the modal open with the edits and the error; the list is stale
        // until the next successful save, so `qk.connections` invalidation only runs on success.
        onSuccess: onClose,
        onError: setFailure,
      },
    );
  };

  return (
    <Modal
      title="Routing"
      open
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            cancel
          </Button>
          <Button variant="primary" disabled={invalid || busy} onClick={save}>
            {busy ? "saving…" : "save"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <p className="text-small leading-relaxed text-muted">
          Priority groups accounts (lower first); weight only splits traffic inside one priority
          group when <span className="font-mono text-caption text-fg">RROUTER_STRATEGY=weighted</span>.
        </p>

        <div className="grid grid-cols-2 gap-3">
          <div className="flex min-w-0 flex-col gap-1">
            <Field label="priority" hint="−100…100; lower groups are tried first">
              <Input
                type="number"
                value={priority}
                onChange={(v) => {
                  setPriority(v);
                  setFailure(null);
                }}
                ariaLabel="routing priority"
                disabled={busy}
              />
            </Field>
            {priorityProblem ? (
              <span role="alert" className="text-caption text-neg">
                {priorityProblem}
              </span>
            ) : null}
          </div>
          <div className="flex min-w-0 flex-col gap-1">
            <Field label="weight" hint="1…1000; share of traffic in this group">
              <Input
                type="number"
                value={weight}
                onChange={(v) => {
                  setWeight(v);
                  setFailure(null);
                }}
                ariaLabel="routing weight"
                disabled={busy}
              />
            </Field>
            {weightProblem ? (
              <span role="alert" className="text-caption text-neg">
                {weightProblem}
              </span>
            ) : null}
          </div>
        </div>

        <div className="flex items-center justify-between gap-3 text-caption text-faint">
          <span>{connection.name ?? connection.email ?? connection.provider}</span>
          <span className="font-mono">{connection.id}</span>
        </div>

        {failure ? <ErrorNote error={failure} /> : null}
      </div>
    </Modal>
  );
}

/**
 * One account's base URL.
 *
 * The override is an origin: the gateway takes it and keeps the provider's endpoint path,
 * so an Azure tenant URL is `https://<resource>.openai.azure.com` with no path of its own.
 * A saved URL can be replaced here but not removed — the credential merge has no "unset",
 * and an empty base URL would send traffic to the origin root — so the hint says so
 * instead of offering a control that would do nothing.
 */
function BaseUrlModal({
  connection,
  spec,
  onClose,
}: {
  connection: Connection;
  spec?: ModelProvider;
  onClose: () => void;
}) {
  const client = useQueryClient();
  const [value, setValue] = useState(connection.base_url_override ?? "");
  const [failure, setFailure] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const required = spec?.requires_base_url_override === true;
  const example = spec?.base_url_example ?? "https://<resource>.openai.azure.com";
  const problem = baseUrlProblem(value, required);
  const unchanged = (connection.base_url_override ?? "") === value.trim();

  const save = async () => {
    if (problem !== null || unchanged || busy) return;
    setBusy(true);
    setFailure(null);
    try {
      await api.patch<Connection>(`/api/connections/${connection.id}`, {
        credentials: { base_url_override: value.trim() },
      });
      void client.invalidateQueries({ queryKey: qk.connections });
      void client.invalidateQueries({ queryKey: qk.models });
      void client.invalidateQueries({ queryKey: qk.modelProviders });
      onClose();
    } catch (error) {
      setFailure(error);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title="Base URL"
      open
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            cancel
          </Button>
          <Button
            variant="primary"
            disabled={problem !== null || unchanged || busy}
            onClick={() => void save()}
          >
            {busy ? "saving…" : "save"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <p className="text-small leading-relaxed text-muted">
          {required
            ? `${spec?.display ?? connection.provider} names your deployment, so this account must point at its own resource. Only the origin is used; the gateway keeps the provider's API path.`
            : "Optional: send this account's traffic to a regional, self-hosted or proxy endpoint. Only the origin is used; the gateway keeps the provider's API path."}
        </p>
        <Field
          label={required ? "resource base URL (required)" : "base URL"}
          hint={required ? `for example ${example}` : "an origin; any path here is replaced"}
        >
          <Input
            value={value}
            onChange={setValue}
            placeholder={example}
            ariaLabel="base url override"
          />
        </Field>
        {problem ? (
          <span role="alert" className="text-caption text-neg">
            {problem}
          </span>
        ) : null}
        {connection.base_url_override ? (
          <p className="text-caption text-faint">
            A saved base URL can be replaced but not removed here: it is where this account
            routes.
          </p>
        ) : null}
        {failure ? <ErrorNote error={failure} /> : null}
      </div>
    </Modal>
  );
}

/**
 * Move one account between a team and the shared pool.
 *
 * The two choices are not equivalent for routing: a team-owned account is used only by
 * that team's keys, while a shared account is used only by instance-wide keys. The hint
 * says so, because "shared" reads as "available to everyone" and it is not.
 */
function TeamModal({
  connection,
  onClose,
}: {
  connection: Connection;
  onClose: () => void;
}) {
  const client = useQueryClient();
  const teams = useTeams();
  const [team, setTeam] = useState(connection.team_id ?? "");
  const [failure, setFailure] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const unchanged = (connection.team_id ?? "") === team;

  const save = async () => {
    if (unchanged || busy) return;
    setBusy(true);
    setFailure(null);
    try {
      await api.patch<Connection>(`/api/connections/${connection.id}`, {
        // Explicit null hands the account back to the shared pool.
        team_id: team === "" ? null : team,
      });
      void client.invalidateQueries({ queryKey: qk.connections });
      void client.invalidateQueries({ queryKey: qk.modelProviders });
      void client.invalidateQueries({ queryKey: qk.models });
      onClose();
    } catch (error) {
      setFailure(error);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title="Owner team"
      open
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            cancel
          </Button>
          <Button variant="primary" disabled={unchanged || busy} onClick={() => void save()}>
            {busy ? "saving…" : "save"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <Field
          label="owner"
          hint="a team account serves only that team's keys; a shared account serves instance-wide keys and is invisible to team accounts"
        >
          <Select value={team} onChange={setTeam} ariaLabel="connection owner team">
            <option value="">Shared / instance-wide</option>
            {(teams.data ?? []).map((row) => (
              <option key={row.id} value={row.id}>
                {row.name}
              </option>
            ))}
          </Select>
        </Field>
        <p className="text-caption text-faint">
          Moving an account takes effect on the next request: a team key with no team-owned
          account of this provider fails with “no eligible upstream” until one is assigned.
        </p>
        {failure ? <ErrorNote error={failure} /> : null}
      </div>
    </Modal>
  );
}

function EmptyProviders({ onAdd }: { onAdd: (() => void) | null }) {
  return (
    <Card className="px-6 py-14 text-center">
      <img
        src={`${import.meta.env.BASE_URL}logo-mark.png`}
        alt=""
        width={56}
        height={56}
        className="mx-auto h-14 w-14 opacity-90"
      />
      <Heading level={2} className="mt-4 text-heading font-medium">Connect your first provider</Heading>
      <p className="mx-auto mt-1.5 max-w-[420px] text-body text-muted">
        RRouter has nothing to route to yet. Sign in to a subscription account, or paste an API key — either
        takes about a minute.
      </p>
      {onAdd ? (
        <div className="mt-5">
          <Button variant="primary" onClick={onAdd}>
            add your first provider
          </Button>
        </div>
      ) : (
        <p className="mt-4 text-caption text-faint">
          An administrator adds the first provider account.
        </p>
      )}
    </Card>
  );
}

function AddProviderFlow({
  open,
  step,
  kind,
  onClose,
  onBack,
  onChooseKind,
  onChooseOAuth,
  onChooseKey,
  onChooseCustom,
  onOpenCodeContinue,
  selectedKeyProvider,
  customProtocol,
  subscriptionProviders,
  keyProviders,
  providers,
  onConnected,
}: {
  open: boolean;
  step: AddStep;
  kind: "subscription" | "api_key" | null;
  onClose: () => void;
  onBack: () => void;
  onChooseKind: (kind: "subscription" | "api_key") => void;
  onChooseOAuth: (provider: OAuthProvider) => void;
  onChooseKey: (provider: string) => void;
  onChooseCustom: (protocol: "openai" | "anthropic") => void;
  onOpenCodeContinue: () => void;
  selectedKeyProvider: string | null;
  customProtocol: "openai" | "anthropic" | null;
  subscriptionProviders: Array<OAuthProvider & { display: string }>;
  keyProviders: ModelProvider[];
  /** Every provider row, for the connection scope of a subscription sign-in. */
  providers: ModelProvider[];
  onConnected: (label: string, warning: string | null) => void;
}) {
  return (
    <Modal
      title={
        step === "kind"
          ? "Add a provider"
          : step === "opencode"
            ? "Add OpenCode"
            : kind === "subscription"
              ? "Sign in with a subscription"
              : customProtocol === "openai"
                ? "Custom OpenAI-compatible endpoint"
                : customProtocol === "anthropic"
                  ? "Custom Anthropic endpoint"
                  : "Add an API key"
      }
      open={open}
      onClose={onClose}
      footer={
        step === "kind" ? null : (
          <Button variant="quiet" onClick={onBack}>
            back
          </Button>
        )
      }
    >
      {step === "kind" ? (
        <div className="grid gap-2 sm:grid-cols-2">
          <Choice
            title="Subscription"
            body="Use a Claude, ChatGPT, or GitHub Copilot plan you already pay for. RRouter keeps the session token fresh."
            onClick={() => onChooseKind("subscription")}
          />
          <Choice
            title="API key"
            body="Paste a key from any provider. Stored encrypted; the key is never returned by the API again."
            onClick={() => onChooseKind("api_key")}
          />
        </div>
      ) : null}

      {step === "provider" && kind === "subscription" ? (
        <ProviderList
          empty="No subscription sign-in is wired for this build."
          items={subscriptionProviders.map((p) => ({
            id: p.id,
            title: p.display,
            meta: p.flow === "device_code" ? "device code" : "browser sign-in",
            scope: providers.find((row) => row.id === p.id)?.scope ?? "none",
            onClick: () => onChooseOAuth(p),
          }))}
        />
      ) : null}

      {step === "provider" && kind === "api_key" ? (
        <Stack gap="lg">
          <div>
            <SectionLabel className="mb-2">custom endpoint</SectionLabel>
            <Grid cols={2} gap="sm">
              {([
                { protocol: "openai", title: "Custom OpenAI", body: "Any OpenAI-compatible chat API" },
                { protocol: "anthropic", title: "Custom Anthropic", body: "Any Anthropic Messages API" },
              ] as const).map((item) => {
                const brand = providerBrand(item.protocol);
                return (
                  <button
                    key={item.protocol}
                    type="button"
                    onClick={() => onChooseCustom(item.protocol)}
                    className="flex items-center gap-3 rounded-[var(--radius-card)] border px-3 py-2.5 text-left transition-colors hover:brightness-[0.98]"
                    style={{
                      borderColor: `${brand.color}66`,
                      backgroundColor: `${brand.color}0f`,
                    }}
                  >
                    <ProviderIcon provider={item.protocol} size={22} />
                    <span className="min-w-0">
                      <span className="block truncate text-body font-medium">{item.title}</span>
                      <span className="block truncate text-caption text-muted">{item.body}</span>
                    </span>
                  </button>
                );
              })}
            </Grid>
          </div>
          <div>
            <SectionLabel className="mb-2">registered providers</SectionLabel>
            <ProviderList
              empty="No API-key provider is registered."
              items={keyProviders.map((p) => ({
                id: p.id,
                title: p.display,
                meta: p.listed ? "lists its models" : "add models manually",
                scope: p.scope,
                onClick: () => onChooseKey(p.id),
              }))}
            />
          </div>
          <div className="rounded-lg border border-dashed border-line-strong bg-surface-2 px-3 py-3">
            <span className="block text-small font-medium">Missing your favorite provider?</span>
            <span className="mt-0.5 block text-caption leading-relaxed text-muted">
              Add it yourself. API-provider requests become a reviewed JSON manifest and a
              pull request; OAuth providers are reviewed with their sign-in flow.
            </span>
            <span className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-small">
              <a
                className="font-medium text-[var(--color-accent-text)] hover:underline"
                href="https://github.com/centopw/RRouter/issues/new?template=provider-api.yml"
                target="_blank"
                rel="noreferrer"
              >
                Add an API provider →
              </a>
              <a
                className="text-muted hover:text-fg hover:underline"
                href="https://github.com/centopw/RRouter/issues/new?template=provider-oauth.yml"
                target="_blank"
                rel="noreferrer"
              >
                Add an OAuth provider →
              </a>
            </span>
          </div>
        </Stack>
      ) : null}

      {step === "opencode" ? <OpenCodeGuide onContinue={onOpenCodeContinue} /> : null}

      {step === "key" && kind === "api_key" ? (
        <ApiKeyForm
          providers={keyProviders}
          fixedProvider={selectedKeyProvider}
          customProtocol={customProtocol}
          onConnected={onConnected}
        />
      ) : null}
    </Modal>
  );
}

function Choice({ title, body, onClick }: { title: string; body: string; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className="rounded-[var(--radius-card)] border border-line-strong bg-surface-2 p-4 text-left transition-colors hover:border-[var(--color-line-strong)] hover:bg-[var(--color-selected)]"
    >
      <span className="block text-lead font-medium">{title}</span>
      <span className="mt-1 block text-small leading-relaxed text-muted">{body}</span>
    </button>
  );
}

function ProviderList({
  items,
  empty,
}: {
  items: Array<{
    id: string;
    title: string;
    meta: string;
    /** `global` = shared account, `team` = a team-owned account, `none` = nothing. */
    scope: ModelProvider["scope"];
    onClick: () => void;
  }>;
  empty: string;
}) {
  if (items.length === 0) return <p className="text-body text-faint">{empty}</p>;
  return (
    <ul className="flex flex-col gap-1.5">
      {items.map((item) => (
        <li key={item.id}>
          <button
            onClick={item.onClick}
            className="flex w-full items-center gap-3 rounded-lg border border-line bg-[var(--color-surface-2)] px-3 py-2.5 text-left hover:border-line-strong hover:bg-surface-2"
          >
            <ProviderIcon provider={item.id} size={20} />
            <span className="min-w-0 flex-1">
              <span className="block truncate text-body">{item.title}</span>
              <span className="block text-caption text-faint">{item.meta}</span>
            </span>
            {item.scope === "global" ? (
              <Badge tone="pos" title="a shared instance account: serves non-team keys only">
                connected · shared
              </Badge>
            ) : item.scope === "team" ? (
              <Badge tone="accent" title="a team-owned account: serves that team's keys">
                connected · team
              </Badge>
            ) : null}
            <span className="text-caption text-faint">→</span>
          </button>
        </li>
      ))}
    </ul>
  );
}

function OpenCodeGuide({ onContinue }: { onContinue: () => void }) {
  return (
    <div className="flex flex-col gap-3">
      <p className="text-small leading-relaxed text-muted">
        OpenCode exposes its models through an OpenAI-compatible endpoint. Pick the plan you
        use — all three reuse one OpenCode API key, created in the Console.
      </p>
      <div className="grid gap-2">
        {[
          {
            title: "Free",
            meta: "limited-time free models",
            body: "Free endpoints are available inside the OpenCode client only; from another client use a Go or pay-as-you-go key. You can still add the key here and route paid/Go models.",
          },
          {
            title: "Pay as you go",
            meta: "Console credits",
            body: "Buy credits in the OpenCode Console and create an API key. Billing is per request, with no monthly subscription.",
          },
          {
            title: "Go / Go Plus / Go Teams",
            meta: "$10 / $40 / $20 per member monthly",
            body: "Subscription access to the Go model list with 5-hour, weekly and monthly allowances. Upgrade or manage members in the Console.",
          },
        ].map((plan) => (
          <div key={plan.title} className="rounded-lg border border-line bg-surface-2 px-3 py-2.5">
            <div className="flex flex-wrap items-baseline gap-2">
              <span className="text-body font-medium">{plan.title}</span>
              <span className="text-caption text-faint">{plan.meta}</span>
            </div>
            <p className="mt-0.5 text-caption leading-relaxed text-muted">{plan.body}</p>
          </div>
        ))}
      </div>
      <div className="flex flex-wrap items-center gap-3 text-small">
        <a
          className="font-medium text-[var(--color-accent-text)] hover:underline"
          href="https://opencode.ai/console"
          target="_blank"
          rel="noreferrer"
        >
          Create an OpenCode API key →
        </a>
        <a
          className="text-muted hover:text-fg hover:underline"
          href="https://opencode.ai/docs/go"
          target="_blank"
          rel="noreferrer"
        >
          Go plans and limits
        </a>
      </div>
      <p className="text-caption leading-relaxed text-faint">
        The OpenCode key authorises RRouter upstream. Clients calling RRouter still need a
        gateway key from API Keys unless the server runs with RROUTER_REQUIRE_API_KEY=false.
      </p>
      <div className="flex justify-end">
        <Button variant="primary" onClick={onContinue}>
          continue with API key →
        </Button>
      </div>
    </div>
  );
}

function ApiKeyForm({
  providers,
  fixedProvider,
  customProtocol,
  onConnected,
}: {
  providers: ModelProvider[];
  fixedProvider: string | null;
  customProtocol: "openai" | "anthropic" | null;
  onConnected: (label: string, warning: string | null) => void;
}) {
  const client = useQueryClient();
  const teams = useTeams();
  const [provider, setProvider] = useState(fixedProvider ?? providers[0]?.id ?? "");
  const [name, setName] = useState("");
  const [team, setTeam] = useState("");
  const [key, setKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [error, setError] = useState<unknown>(null);

  useEffect(() => {
    if (fixedProvider) setProvider(fixedProvider);
  }, [fixedProvider]);

  const selected = providers.find((p) => p.id === (fixedProvider ?? provider));
  const fixedDisplay = selected?.display;
  // A provider whose manifest host is a placeholder cannot route without a real URL, so
  // the field stops being optional and the button stays disabled until it is filled in.
  const requiresBaseUrl = selected?.requires_base_url_override === true;
  const example = selected?.base_url_example ?? "https://<resource>.openai.azure.com";
  const baseProblem = baseUrlProblem(baseUrl, requiresBaseUrl);
  const needsBaseUrl = customProtocol !== null && baseUrl.trim() === "";

  const save = useMutation({
    mutationFn: () =>
      api.post<Connection>("/api/connections", {
        provider,
        auth_type: "api_key",
        name: name || null,
        // `null` is the shared pool: instance-wide keys route here, team keys do not.
        team_id: team === "" ? null : team,
        credentials: {
          api_key: key,
          ...(baseUrl.trim() ? { base_url_override: baseUrl.trim() } : {}),
        },
      }),
    onSuccess: (conn) => {
      void client.invalidateQueries({ queryKey: qk.connections });
      void client.invalidateQueries({ queryKey: qk.models });
      void client.invalidateQueries({ queryKey: qk.modelProviders });
      setKey("");
      onConnected(conn.name ?? conn.provider, null);
    },
    onError: setError,
  });
  const ready =
    provider !== "" && key !== "" && !needsBaseUrl && baseProblem === null && !save.isPending;

  return (
    <form
      className="flex flex-col gap-3"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <Field
        label="provider"
        hint={
          customProtocol
            ? `Talks the ${customProtocol === "openai" ? "OpenAI chat" : "Anthropic messages"} protocol; models are added on the Models page.`
            : fixedProvider === "opencode"
              ? "OpenCode Console key — Free, pay-as-you-go and Go keys all work."
              : undefined
        }
      >
        {fixedProvider ? (
          <div className="flex h-9 items-center gap-2 rounded-lg border border-line bg-surface-2 px-3 text-body text-muted">
            <ProviderIcon provider={fixedProvider} size={16} />
            {fixedDisplay ?? fixedProvider}
          </div>
        ) : (
          <Select value={provider} onChange={setProvider} ariaLabel="api key provider">
            {providers.map((p) => (
              <option key={p.id} value={p.id}>
                {p.display}
              </option>
            ))}
          </Select>
        )}
      </Field>
      <Field label="label" hint="how this account appears in the table">
        <Input value={name} onChange={setName} placeholder="production-key" ariaLabel="account label" />
      </Field>
      <Field
        label="owner"
        hint="a team account serves only that team's keys; shared serves instance-wide keys"
      >
        <Select value={team} onChange={setTeam} ariaLabel="connection owner team">
          <option value="">Shared / instance-wide</option>
          {(teams.data ?? []).map((row) => (
            <option key={row.id} value={row.id}>
              {row.name}
            </option>
          ))}
        </Select>
      </Field>
      <Field label="api key" hint="stored encrypted; never returned by the API">
        <Input value={key} onChange={setKey} type="password" placeholder="sk-…" ariaLabel="api key" />
      </Field>
      <Field
        label={requiresBaseUrl ? "resource base URL (required)" : "base url"}
        hint={
          requiresBaseUrl
            ? `Your deployment's endpoint, for example ${example}. The gateway keeps the provider's API path.`
            : customProtocol
              ? "required; the endpoint root that serves this protocol"
              : "optional, for a self-hosted or proxy endpoint"
        }
      >
        <Input
          value={baseUrl}
          onChange={setBaseUrl}
          placeholder={
            requiresBaseUrl
              ? example
              : customProtocol === "anthropic"
                ? "https://api.example.com/v1"
                : "https://api.example.com/v1"
          }
          ariaLabel="base url override"
        />
      </Field>
      {baseProblem ? (
        <span role="alert" className="text-caption text-neg">
          {baseProblem}
        </span>
      ) : null}
      {error ? <ErrorNote error={error} /> : null}
      <div className="flex justify-end">
        <Button type="submit" variant="primary" disabled={!ready}>
          save provider
        </Button>
      </div>
    </form>
  );
}

function TestButton({ id }: { id: string }) {
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  return (
    <Button
      disabled={busy}
      onClick={async () => {
        setBusy(true);
        try {
          const res = await api.post<{ ok: boolean; status: number; latency_ms: number; error?: string }>(
            `/api/connections/${id}/test`,
          );
          if (res.ok) {
            toast.success(`Connection works · ${res.status} · ${res.latency_ms}ms`);
          } else {
            toast.error(`Connection failed · ${res.status}`, res.error ?? undefined);
          }
        } catch (e) {
          toast.error("Connection test failed", e instanceof Error ? e.message : String(e));
        } finally {
          setBusy(false);
        }
      }}
    >
      {busy ? "testing…" : "test"}
    </Button>
  );
}

/** Wire spelling to the phrase the row shows; the server stores `daily` and friends. */
const WINDOW_LABEL: Record<AccountQuotaWindow, string> = {
  daily: "daily",
  weekly: "weekly",
  monthly: "monthly",
};

const QUOTA_WINDOWS = [
  { id: "daily" as const, label: "daily" },
  { id: "weekly" as const, label: "weekly" },
  { id: "monthly" as const, label: "monthly" },
];

/** Green under 70%, amber to 89%, red at 90%+. Text carries the number too. */
function usageTone(percent: number): { fill: string; text: string } {
  if (percent >= 90) return { fill: "bg-[var(--color-neg-fill)]", text: "text-[var(--color-neg)]" };
  if (percent >= 70) return { fill: "bg-[var(--color-warn)]", text: "text-[var(--color-warn)]" };
  return { fill: "bg-[var(--color-pos)]", text: "text-[var(--color-pos)]" };
}

/** "1h 35m" until the window rolls over; empty when the provider gave no timestamp. */
function until(ts: number | null): string {
  if (!ts) return "";
  const secs = Math.max(0, ts - Math.floor(Date.now() / 1000));
  if (secs < 3600) return `${Math.max(1, Math.round(secs / 60))}m`;
  if (secs < 86_400) return `${Math.floor(secs / 3600)}h ${Math.round((secs % 3600) / 60)}m`;
  return `${Math.floor(secs / 86_400)}d ${Math.round((secs % 86_400) / 3600)}h`;
}

function WindowBar({ window }: { window: SubscriptionWindow }) {
  const percent = Math.round(window.used_percent);
  const tone = usageTone(percent);
  const short =
    window.id === "five_hour"
      ? "5h"
      : window.id === "weekly"
        ? "wk"
        : window.id === "premium-interactions"
          ? "req"
          : window.id === "completions"
            ? "code"
            : window.label.toLowerCase();
  return (
    <div className="flex items-center gap-2 text-caption">
      <span className="w-11 shrink-0 text-faint" title={window.label}>
        {short}
      </span>
      <span className="h-1.5 min-w-[48px] flex-1 overflow-hidden rounded-full bg-rule" aria-hidden>
        <span className={cx("block h-full rounded-full", tone.fill)} style={{ width: `${percent}%` }} />
      </span>
      <span className={cx("w-9 shrink-0 text-right font-medium tnum", tone.text)}>{percent}%</span>
      {window.resets_at ? (
        <span className="w-14 shrink-0 text-right text-faint" title={`resets ${when(window.resets_at)}`}>
          {until(window.resets_at)}
        </span>
      ) : null}
    </div>
  );
}

function SubscriptionRow({ account }: { account: AccountSubscription }) {
  const exhausted = account.windows.find((window) => window.used_percent >= 100);
  return (
    <div className="border-b border-rule px-5 py-3 last:border-0">
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
        <span className="text-body font-medium text-fg">
          {account.provider === "claude"
            ? "Claude"
            : account.provider === "codex"
              ? "Codex"
              : account.provider === "github"
                ? "GitHub Copilot"
                : account.provider}
        </span>
        <span className="min-w-0 truncate text-small text-muted">{account.name ?? ""}</span>
        {account.plan ? <Badge tone="neutral">{account.plan}</Badge> : null}
        {exhausted ? (
          <Badge tone="neg" title={exhausted.resets_at ? `resets ${when(exhausted.resets_at)}` : undefined}>
            quota exhausted
          </Badge>
        ) : null}
        {account.error ? (
          <span
            className="max-w-full truncate text-caption text-faint"
            title={account.error}
          >
            {account.error.length > 110 ? `${account.error.slice(0, 110)}…` : account.error}
          </span>
        ) : (
          <span className="ml-auto flex items-center gap-1.5 text-caption text-faint">
            <Dot tone="pos" />
            live
          </span>
        )}
      </div>
      {account.windows.length > 0 ? (
        <div className="mt-2 grid gap-1.5 min-[700px]:grid-cols-2 min-[1100px]:grid-cols-3">
          {account.windows.map((window) => (
            <WindowBar key={window.id} window={window} />
          ))}
        </div>
      ) : null}
    </div>
  );
}

/**
 * What the account spent inside its window, against each limit it sets. A quota with
 * no limit still shows the spend: "unlimited" is a permission, not an absence of data.
 */
function QuotaUsage({ row }: { row: AccountQuotaRow }) {
  const parts: string[] = [];
  if (row.max_requests > 0) {
    parts.push(`requests ${compact(row.requests)}/${compact(row.max_requests)}`);
  }
  if (row.max_tokens > 0) {
    parts.push(`tokens ${compact(row.tokens)}/${compact(row.max_tokens)}`);
  }
  if (row.max_cost_micros > 0) {
    parts.push(`${usd(row.cost_micros)}/${usd(row.max_cost_micros)}`);
  }
  return (
    <span className="text-muted">
      {parts.length > 0
        ? parts.join(" · ")
        : `${usd(row.cost_micros)} · ${compact(row.tokens)} tokens · ${compact(row.requests)} requests`}
      {row.exhausted && row.resets_at ? (
        <span className="text-neg"> · resets {when(row.resets_at)}</span>
      ) : null}
    </span>
  );
}

/**
 * Limits for one account. Every field is optional: an empty field or 0 means unlimited,
 * and save always sends the whole form so clearing a field clears the limit.
 */
function QuotaModal({ row, onClose }: { row: AccountQuotaRow; onClose: () => void }) {
  const update = useUpdateAccountQuota();
  const clear = useClearAccountQuota();
  const [window, setWindow] = useState<AccountQuotaWindow>(row.window);
  const [requests, setRequests] = useState(row.max_requests > 0 ? String(row.max_requests) : "");
  const [tokens, setTokens] = useState(row.max_tokens > 0 ? String(row.max_tokens) : "");
  const [cost, setCost] = useState(usdInput(row.max_cost_micros));
  const [failure, setFailure] = useState<unknown>(null);
  const [confirmClear, setConfirmClear] = useState(false);

  const countProblem = (value: string): string | null => {
    const text = value.trim();
    if (text === "") return null;
    return /^\d+$/.test(text) && Number.isSafeInteger(Number(text)) ? null : "whole number only";
  };
  const costParsed = parseUsd(cost);
  const requestsProblem = countProblem(requests);
  const tokensProblem = countProblem(tokens);
  const costProblem = "problem" in costParsed ? costParsed.problem : null;
  const invalid = requestsProblem !== null || tokensProblem !== null || costProblem !== null;
  const busy = update.isPending || clear.isPending;
  const configured =
    row.max_requests > 0 || row.max_tokens > 0 || row.max_cost_micros > 0 || row.window !== "daily";

  const save = () => {
    if (invalid || busy) return;
    setFailure(null);
    update.mutate(
      {
        id: row.connection_id,
        patch: {
          window,
          max_requests: requests.trim() === "" ? 0 : Number(requests),
          max_tokens: tokens.trim() === "" ? 0 : Number(tokens),
          max_cost_micros: "micros" in costParsed ? costParsed.micros : 0,
        },
      },
      { onSuccess: onClose, onError: setFailure },
    );
  };

  return (
    <Modal
      title="Account quota"
      open
      width="lg"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            cancel
          </Button>
          {configured ? (
            <Button variant="danger" disabled={busy} onClick={() => setConfirmClear(true)}>
              clear
            </Button>
          ) : null}
          <Button variant="primary" disabled={invalid || busy} onClick={save}>
            {busy ? "saving…" : "save"}
          </Button>
        </>
      }
    >
      <ConfirmDialog
        open={confirmClear}
        title="Clear account quota"
        message={<>The operator limits for <span className="font-medium">{row.name ?? row.provider}</span> are removed. Its subscription limits, when the provider exposes them, are unaffected.</>}
        confirmLabel="Clear quota"
        busy={clear.isPending}
        onConfirm={() => {
          setFailure(null);
          clear.mutate(row.connection_id, { onSuccess: onClose, onError: setFailure });
          setConfirmClear(false);
        }}
        onClose={() => setConfirmClear(false)}
      />

      <div className="flex flex-col gap-4">
        <p className="text-small leading-relaxed text-muted">
          Measured from the start of the current UTC window. The router skips an account
          once a limit is reached and falls back to the next one.
        </p>

        <Field label="window">
          <Segmented
            options={QUOTA_WINDOWS}
            value={window}
            onChange={(v) => {
              if (!busy) setWindow(v);
            }}
            ariaLabel="quota window"
          />
        </Field>

        <div className="grid grid-cols-2 gap-3">
          <div className="flex min-w-0 flex-col gap-1">
            <Field label="requests" hint="empty = unlimited">
              <Input
                type="number"
                value={requests}
                onChange={(v) => {
                  setRequests(v);
                  setFailure(null);
                }}
                ariaLabel="quota requests"
                disabled={busy}
              />
            </Field>
            {requestsProblem ? (
              <span role="alert" className="text-caption text-neg">
                {requestsProblem}
              </span>
            ) : null}
          </div>
          <div className="flex min-w-0 flex-col gap-1">
            <Field label="tokens" hint="empty = unlimited">
              <Input
                type="number"
                value={tokens}
                onChange={(v) => {
                  setTokens(v);
                  setFailure(null);
                }}
                ariaLabel="quota tokens"
                disabled={busy}
              />
            </Field>
            {tokensProblem ? (
              <span role="alert" className="text-caption text-neg">
                {tokensProblem}
              </span>
            ) : null}
          </div>
        </div>

        <div className="flex min-w-0 flex-col gap-1">
          <Field label="spend (USD)" hint="empty = unlimited">
            <Input
              value={cost}
              onChange={(v) => {
                setCost(v);
                setFailure(null);
              }}
              ariaLabel="quota spend"
              disabled={busy}
            />
          </Field>
          {costProblem ? (
            <span role="alert" className="text-caption text-neg">
              {costProblem}
            </span>
          ) : null}
        </div>

        <div className="flex items-center justify-between gap-3 text-caption text-faint">
          <span>{row.name ?? row.provider}</span>
          <span className="font-mono">{row.connection_id}</span>
        </div>

        {failure ? <ErrorNote error={failure} /> : null}
      </div>
    </Modal>
  );
}
