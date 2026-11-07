import { useMemo, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
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
  useModelProviders,
  useOAuthProviders,
  useUpdateAccountQuota,
  useUpdateConnection,
} from "../lib/queries";
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

type AddStep = "kind" | "provider" | "key";

export default function Providers() {
  const connections = useConnections();
  const providers = useModelProviders();
  const oauth = useOAuthProviders();
  const client = useQueryClient();

  const [adding, setAdding] = useState(false);
  const [step, setStep] = useState<AddStep>("kind");
  const [kind, setKind] = useState<"subscription" | "api_key" | null>(null);
  const [signIn, setSignIn] = useState<OAuthProvider | null>(null);
  const [authWindow, setAuthWindow] = useState<Window | null>(null);
  const [notice, setNotice] = useState<{ tone: "pos" | "neg"; text: string } | null>(null);
  const [routing, setRouting] = useState<Connection | null>(null);
  const [quotaFor, setQuotaFor] = useState<AccountQuotaRow | null>(null);
  const [removing, setRemoving] = useState<Connection | null>(null);
  const quotas = useAccountQuotas();
  const subscriptions = useAccountSubscriptions();

  const accounts = connections.data ?? [];
  const connectedIds = useMemo(() => new Set(accounts.map((a) => a.provider)), [accounts]);
  const oauthIds = useMemo(() => new Set((oauth.data ?? []).map((p) => p.id)), [oauth.data]);

  const displayOf = (id: string) =>
    providers.data?.find((p) => p.id === id)?.display ?? id;

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
    setNotice(null);
  };

  const chooseKind = (k: "subscription" | "api_key") => {
    setKind(k);
    setStep("provider");
  };

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
          <span className="font-medium">{displayOf(r.provider)}</span>
        </span>
      ),
    },
    {
      key: "account",
      title: "account",
      render: (r) => (
        <span className="text-muted">{r.name ?? r.email ?? <span className="text-faint">unnamed</span>}</span>
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
      render: (r) =>
        r.backoff_level > 0 ? (
          <Badge tone="warn">backoff {r.backoff_level}</Badge>
        ) : r.is_active ? (
          <Badge tone="pos">active</Badge>
        ) : (
          <Badge tone="neutral">disabled</Badge>
        ),
    },
    {
      key: "actions",
      title: "",
      align: "right",
      render: (r) => (
        <span className="flex justify-end gap-1.5">
          <Button onClick={() => setRouting(r)} title="edit routing priority and weight">
            edit
          </Button>
          <TestButton id={r.id} onResult={(text, tone) => setNotice({ tone, text })} />
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
    },
  ];

  return (
    <>
      <PageHeader
        title="Providers"
        subtitle="Accounts this gateway can route through. Each one is an upstream the router may pick."
        actions={
          <Button variant="primary" onClick={openAdd}>
            add provider
          </Button>
        }
      />

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
            "mb-3 flex items-start gap-2 rounded-lg border px-3 py-2 text-[13px]",
            notice.tone === "pos"
              ? "border-[var(--color-pos)]/35 bg-[var(--color-pos-tint)] text-[var(--color-pos)]"
              : "border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] text-[var(--color-neg-ink)]",
          )}
        >
          <span className="min-w-0 flex-1 break-words">{notice.text}</span>
          <button onClick={() => setNotice(null)} className="text-[11px] underline underline-offset-2">
            dismiss
          </button>
        </div>
      ) : null}

      {connections.isError ? (
        <div className="mb-4">
          <ErrorNote error={connections.error} onRetry={() => void connections.refetch()} />
        </div>
      ) : null}

      {!connections.isLoading && accounts.length === 0 ? (
        <EmptyProviders onAdd={openAdd} />
      ) : (
        <Card>
          <CardHeader
            title="Connected"
            action={
              <span className="text-[11px] text-faint">
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

      {!connections.isLoading && accounts.length > 0 ? (
        <div className="mt-4">
          <Card>
            <CardHeader
              title="Subscription limits"
              action={
                <span className="text-[11px] text-faint">
                  {subscriptions.isFetching ? "refreshing…" : "from your provider plan · refreshes every five minutes"}
                </span>
              }
            />
            {subscriptions.isError ? (
              <div className="px-5 pb-4">
                <ErrorNote error={subscriptions.error} onRetry={() => void subscriptions.refetch()} />
              </div>
            ) : subscriptions.isLoading ? (
              <p className="px-5 pb-4 text-[12.5px] text-faint">reading plan limits…</p>
            ) : (subscriptions.data ?? []).length === 0 ? (
              <p className="px-5 pb-4 text-[12.5px] text-muted">
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

      {!connections.isLoading && accounts.length > 0 ? (
        <div className="mt-4">
          <Card>
            <CardHeader
              title="Operator limits"
              action={
                <span className="text-[11px] text-faint">
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
        onChooseKey={() => setStep("key")}
        subscriptionProviders={subscriptionProviders}
        keyProviders={keyProviders}
        connected={connectedIds}
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
        <p className="text-[12px] leading-relaxed text-muted">
          Priority groups accounts (lower first); weight only splits traffic inside one priority
          group when <span className="font-mono text-[11px] text-fg">RROUTER_STRATEGY=weighted</span>.
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
              <span role="alert" className="text-[11px] text-neg">
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
              <span role="alert" className="text-[11px] text-neg">
                {weightProblem}
              </span>
            ) : null}
          </div>
        </div>

        <div className="flex items-center justify-between gap-3 text-[11px] text-faint">
          <span>{connection.name ?? connection.email ?? connection.provider}</span>
          <span className="font-mono">{connection.id}</span>
        </div>

        {failure ? <ErrorNote error={failure} /> : null}
      </div>
    </Modal>
  );
}

function EmptyProviders({ onAdd }: { onAdd: () => void }) {
  return (
    <Card className="px-6 py-14 text-center">
      <img
        src={`${import.meta.env.BASE_URL}logo-mark.png`}
        alt=""
        width={56}
        height={56}
        className="mx-auto h-14 w-14 opacity-90"
      />
      <h2 className="mt-4 text-[17px] font-medium">Connect your first provider</h2>
      <p className="mx-auto mt-1.5 max-w-[420px] text-[13px] text-muted">
        RRouter has nothing to route to yet. Sign in to a subscription account, or paste an API key — either
        takes about a minute.
      </p>
      <div className="mt-5">
        <Button variant="primary" onClick={onAdd}>
          add your first provider
        </Button>
      </div>
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
  subscriptionProviders,
  keyProviders,
  connected,
  onConnected,
}: {
  open: boolean;
  step: AddStep;
  kind: "subscription" | "api_key" | null;
  onClose: () => void;
  onBack: () => void;
  onChooseKind: (kind: "subscription" | "api_key") => void;
  onChooseOAuth: (provider: OAuthProvider) => void;
  onChooseKey: () => void;
  subscriptionProviders: Array<OAuthProvider & { display: string }>;
  keyProviders: ModelProvider[];
  connected: Set<string>;
  onConnected: (label: string, warning: string | null) => void;
}) {
  return (
    <Modal
      title={step === "kind" ? "Add a provider" : kind === "subscription" ? "Sign in with a subscription" : "Add an API key"}
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
            connected: connected.has(p.id),
            onClick: () => onChooseOAuth(p),
          }))}
        />
      ) : null}

      {step === "provider" && kind === "api_key" ? (
        <ProviderList
          empty="No API-key provider is registered."
          items={keyProviders.map((p) => ({
            id: p.id,
            title: p.display,
            meta: p.listed ? "lists its models" : "add models manually",
            connected: connected.has(p.id),
            onClick: onChooseKey,
          }))}
        />
      ) : null}

      {step === "key" && kind === "api_key" ? (
        <ApiKeyForm providers={keyProviders} onConnected={onConnected} />
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
      <span className="block text-[14px] font-medium">{title}</span>
      <span className="mt-1 block text-[12px] leading-relaxed text-muted">{body}</span>
    </button>
  );
}

function ProviderList({
  items,
  empty,
}: {
  items: Array<{ id: string; title: string; meta: string; connected: boolean; onClick: () => void }>;
  empty: string;
}) {
  if (items.length === 0) return <p className="text-[13px] text-faint">{empty}</p>;
  return (
    <ul className="flex flex-col gap-1.5">
      {items.map((item) => (
        <li key={item.id}>
          <button
            onClick={item.onClick}
            className="flex w-full items-center gap-3 rounded-lg border border-line bg-[var(--color-surface-2)] px-3 py-2.5 text-left hover:border-line-strong hover:bg-surface-2"
          >
            <span className="min-w-0 flex-1">
              <span className="block truncate text-[13px]">{item.title}</span>
              <span className="block text-[11px] text-faint">{item.meta}</span>
            </span>
            {item.connected ? <Badge tone="pos">connected</Badge> : null}
            <span className="text-[11px] text-faint">→</span>
          </button>
        </li>
      ))}
    </ul>
  );
}

function ApiKeyForm({
  providers,
  onConnected,
}: {
  providers: ModelProvider[];
  onConnected: (label: string, warning: string | null) => void;
}) {
  const client = useQueryClient();
  const [provider, setProvider] = useState(providers[0]?.id ?? "");
  const [name, setName] = useState("");
  const [key, setKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [error, setError] = useState<unknown>(null);

  const save = useMutation({
    mutationFn: () =>
      api.post<Connection>("/api/connections", {
        provider,
        auth_type: "api_key",
        name: name || null,
        credentials: {
          api_key: key,
          ...(baseUrl ? { base_url_override: baseUrl } : {}),
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

  return (
    <form
      className="flex flex-col gap-3"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <Field label="provider">
        <Select value={provider} onChange={setProvider} ariaLabel="api key provider">
          {providers.map((p) => (
            <option key={p.id} value={p.id}>
              {p.display}
            </option>
          ))}
        </Select>
      </Field>
      <Field label="label" hint="how this account appears in the table">
        <Input value={name} onChange={setName} placeholder="production-key" ariaLabel="account label" />
      </Field>
      <Field label="api key" hint="stored encrypted; never returned by the API">
        <Input value={key} onChange={setKey} type="password" placeholder="sk-…" ariaLabel="api key" />
      </Field>
      <Field label="base url override" hint="optional, for a self-hosted or proxy endpoint">
        <Input value={baseUrl} onChange={setBaseUrl} placeholder="https://…/v1" ariaLabel="base url override" />
      </Field>
      {error ? <ErrorNote error={error} /> : null}
      <div className="flex justify-end">
        <Button type="submit" variant="primary" disabled={!provider || !key || save.isPending}>
          save provider
        </Button>
      </div>
    </form>
  );
}

function TestButton({ id, onResult }: { id: string; onResult: (text: string, tone: "pos" | "neg") => void }) {
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
          onResult(
            res.ok ? `test ok · ${res.status} · ${res.latency_ms}ms` : `test failed · ${res.status} · ${res.error ?? ""}`,
            res.ok ? "pos" : "neg",
          );
        } catch (e) {
          onResult(e instanceof Error ? e.message : String(e), "neg");
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
    <div className="flex items-center gap-2 text-[11.5px]">
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
  return (
    <div className="border-b border-rule px-5 py-3 last:border-0">
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
        <span className="text-[13.5px] font-medium text-fg">
          {account.provider === "claude"
            ? "Claude"
            : account.provider === "codex"
              ? "Codex"
              : account.provider === "github"
                ? "GitHub Copilot"
                : account.provider}
        </span>
        <span className="min-w-0 truncate text-[12px] text-muted">{account.name ?? ""}</span>
        {account.plan ? <Badge tone="neutral">{account.plan}</Badge> : null}
        {account.error ? (
          <span
            className="max-w-full truncate text-[11.5px] text-faint"
            title={account.error}
          >
            {account.error.length > 110 ? `${account.error.slice(0, 110)}…` : account.error}
          </span>
        ) : (
          <span className="ml-auto flex items-center gap-1.5 text-[11px] text-faint">
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
        <p className="text-[12px] leading-relaxed text-muted">
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
              <span role="alert" className="text-[11px] text-neg">
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
              <span role="alert" className="text-[11px] text-neg">
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
            <span role="alert" className="text-[11px] text-neg">
              {costProblem}
            </span>
          ) : null}
        </div>

        <div className="flex items-center justify-between gap-3 text-[11px] text-faint">
          <span>{row.name ?? row.provider}</span>
          <span className="font-mono">{row.connection_id}</span>
        </div>

        {failure ? <ErrorNote error={failure} /> : null}
      </div>
    </Modal>
  );
}
