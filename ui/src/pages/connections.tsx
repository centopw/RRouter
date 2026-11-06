import { useEffect, useMemo, useState } from "react";
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
  Select,
} from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { OAuthDialog } from "../components/oauth-dialog";
import { ms } from "../lib/format";
import { qk, useConnections, useModelProviders, useOAuthProviders } from "../lib/queries";
import {
  api,
  ApiError,
  type Connection,
  type ModelProvider,
  type OAuthProvider,
} from "../lib/api";

/** `/api/connections/:id` takes PATCH, and api.ts only exposes get/post/put/del. */
async function patchConnection(id: string, body: unknown): Promise<Connection> {
  const res = await fetch(`/api/connections/${id}`, {
    method: "PATCH",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const text = await res.text();
  let parsed: unknown = null;
  try {
    parsed = text ? JSON.parse(text) : null;
  } catch {
    parsed = text;
  }
  if (!res.ok) {
    const envelope = parsed as { error?: { message?: unknown } } | null;
    const message =
      typeof envelope?.error?.message === "string"
        ? envelope.error.message
        : `${res.status} ${res.statusText}`;
    throw new ApiError(res.status, message, parsed);
  }
  return parsed as Connection;
}

type ConnectionTest = { ok: boolean; status: number; latency_ms: number; error?: string };

type TestState =
  | { pending: true }
  | { pending: false; result?: ConnectionTest; error?: string };

type SignIn = { provider: OAuthProvider; authWindow: Window | null };

export default function Connections() {
  const connections = useConnections();
  const providers = useModelProviders();
  const oauth = useOAuthProviders();
  const client = useQueryClient();
  const [addFor, setAddFor] = useState<{ provider: string } | null>(null);
  const [signIn, setSignIn] = useState<SignIn | null>(null);
  const [tests, setTests] = useState<Record<string, TestState>>({});
  const [toast, setToast] = useState<string | null>(null);

  useEffect(() => {
    if (!toast) return;
    const timer = setTimeout(() => setToast(null), 6_000);
    return () => clearTimeout(timer);
  }, [toast]);

  const oauthById = useMemo(
    () => new Map((oauth.data ?? []).map((p) => [p.id, p])),
    [oauth.data],
  );

  const invalidate = () => {
    void client.invalidateQueries({ queryKey: qk.connections });
    void client.invalidateQueries({ queryKey: qk.modelProviders });
    void client.invalidateQueries({ queryKey: qk.system });
  };

  const test = useMutation({
    mutationFn: (id: string) => api.post<ConnectionTest>(`/api/connections/${id}/test`),
    onMutate: (id) => {
      setTests((prev) => ({ ...prev, [id]: { pending: true } }));
    },
    onSuccess: (result, id) => {
      setTests((prev) => ({ ...prev, [id]: { pending: false, result } }));
    },
    onError: (error, id) => {
      setTests((prev) => ({
        ...prev,
        [id]: { pending: false, error: error instanceof Error ? error.message : String(error) },
      }));
    },
  });

  const toggle = useMutation({
    mutationFn: (vars: { id: string; active: boolean }) =>
      patchConnection(vars.id, { is_active: vars.active }),
    onSuccess: invalidate,
  });

  const remove = useMutation({
    mutationFn: (id: string) => api.del<{ deleted: boolean }>(`/api/connections/${id}`),
    onSuccess: (_result, id) => {
      setTests((prev) => {
        const next = { ...prev };
        delete next[id];
        return next;
      });
      invalidate();
    },
  });

  const openSignIn = (provider: OAuthProvider) => {
    // Opened inside the click so the browser keeps it as user-initiated; the dialog
    // navigates it once the authorize URL exists.
    const authWindow = window.open("", "rrouter-oauth", "width=520,height=720");
    setSignIn({ provider, authWindow });
  };

  const columns: Array<Column<Connection>> = [
    {
      key: "provider",
      title: "provider",
      render: (r) => <span className="font-mono text-[12px]">{r.provider}</span>,
    },
    {
      key: "name",
      title: "name",
      render: (r) => (
        <div className="min-w-0">
          <div className="truncate">
            {r.name ?? r.email ?? <span className="text-faint">unnamed</span>}
          </div>
          {r.name && r.email && r.email !== r.name ? (
            <div className="truncate text-[11px] text-faint">{r.email}</div>
          ) : null}
        </div>
      ),
    },
    {
      key: "auth",
      title: "auth",
      render: (r) => <Badge tone="neutral">{r.auth_type}</Badge>,
    },
    {
      key: "tokens",
      title: "tokens",
      render: (r) => {
        const tokens = [
          r.has_access_token ? "access" : null,
          r.has_refresh_token ? "refresh" : null,
          r.has_api_key ? "key" : null,
        ].filter((t): t is string => t !== null);
        return tokens.length ? (
          <span className="flex flex-wrap gap-1">
            {tokens.map((t) => (
              <Badge key={t}>{t}</Badge>
            ))}
          </span>
        ) : (
          <span className="text-faint">—</span>
        );
      },
    },
    {
      key: "priority",
      title: "priority",
      align: "right",
      width: "88px",
      render: (r) => <span className="text-muted">{r.priority}</span>,
    },
    {
      key: "state",
      title: "state",
      width: "120px",
      render: (r) =>
        !r.is_active ? (
          <span className="flex items-center gap-1.5 text-[12px] text-faint">
            <Dot tone="muted" /> off
          </span>
        ) : r.backoff_level > 0 ? (
          <span className="flex items-center gap-1.5 text-[12px] text-warn" title={r.last_error ?? undefined}>
            <Dot tone="warn" /> backoff {r.backoff_level}
          </span>
        ) : (
          <span className="flex items-center gap-1.5 text-[12px] text-pos">
            <Dot tone="pos" /> active
          </span>
        ),
    },
    {
      key: "actions",
      title: "",
      align: "right",
      render: (r) => (
        <div className="flex flex-col items-end gap-1">
          <div className="flex items-center gap-1.5">
            <Button
              onClick={() => test.mutate(r.id)}
              disabled={tests[r.id]?.pending === true}
              title="send one real token request through this connection"
            >
              {tests[r.id]?.pending === true ? "testing…" : "test"}
            </Button>
            <Button
              variant="quiet"
              onClick={() => toggle.mutate({ id: r.id, active: !r.is_active })}
              disabled={toggle.isPending}
            >
              {r.is_active ? "disable" : "enable"}
            </Button>
            <Button
              variant="danger"
              onClick={() => {
                if (
                  confirm(
                    `Delete the ${r.provider} connection${r.name ? ` for ${r.name}` : ""}? Its stored credentials are removed.`,
                  )
                ) {
                  remove.mutate(r.id);
                }
              }}
            >
              delete
            </Button>
          </div>
          <TestLine state={tests[r.id]} />
        </div>
      ),
    },
  ];

  const tiles = providers.data ?? [];

  return (
    <>
      <PageHeader
        title="Connections"
        subtitle="Accounts this gateway can route through."
        actions={
          <Button
            variant="primary"
            disabled={!tiles.length}
            onClick={() => setAddFor({ provider: tiles[0]?.id ?? "" })}
          >
            add connection
          </Button>
        }
      />

      {connections.isError ? (
        <div className="mb-4">
          <ErrorNote error={connections.error} onRetry={() => void connections.refetch()} />
        </div>
      ) : null}
      {providers.isError ? (
        <div className="mb-4">
          <ErrorNote error={providers.error} onRetry={() => void providers.refetch()} />
        </div>
      ) : null}
      {oauth.isError ? (
        <div className="mb-4">
          <ErrorNote error={oauth.error} onRetry={() => void oauth.refetch()} />
        </div>
      ) : null}

      <Card className="mb-3">
        <CardHeader
          title="Providers"
          action={
            <span className="text-[11px] text-faint">
              sign in where OAuth exists, otherwise add a key
            </span>
          }
        />
        {providers.data === undefined ? (
          <div className="px-5 pb-6 text-[13px] text-faint">loading…</div>
        ) : tiles.length === 0 ? (
          <div className="px-5 pb-6 text-[13px] text-faint">No providers reported by the gateway.</div>
        ) : (
          <div className="grid gap-2 px-5 pb-5 sm:grid-cols-2 xl:grid-cols-3">
            {tiles.map((p) => {
              const oauthProvider = oauthById.get(p.id);
              return (
                <div
                  key={p.id}
                  className="flex flex-col justify-between gap-3 rounded-lg border border-line-strong bg-surface-2 px-3 py-2.5"
                >
                  <div className="flex items-start justify-between gap-2">
                    <span className="min-w-0 truncate text-[13px] font-medium text-fg" title={p.id}>
                      {p.display}
                    </span>
                    {p.has_connection ? (
                      <span className="flex shrink-0 items-center gap-1.5 text-[11px] text-pos">
                        <Dot tone="pos" /> connected
                      </span>
                    ) : (
                      <span className="shrink-0 text-[11px] text-faint">no credentials</span>
                    )}
                  </div>
                  <div className="flex items-center justify-between gap-2">
                    <span className="truncate font-mono text-[11px] text-faint">{p.id}</span>
                    {oauthProvider ? (
                      <Button onClick={() => openSignIn(oauthProvider)}>sign in</Button>
                    ) : (
                      <Button onClick={() => setAddFor({ provider: p.id })}>add key</Button>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </Card>

      <Card>
        <CardHeader
          title="Accounts"
          action={
            <span className="text-[11px] text-faint">
              {connections.data ? `${connections.data.length} connection${connections.data.length === 1 ? "" : "s"}` : ""}
            </span>
          }
        />
        <DataTable
          columns={columns}
          rows={connections.data}
          rowKey={(r) => r.id}
          loading={connections.isLoading}
          empty="No accounts yet. Pick a provider above and sign in or add a key."
        />
        {remove.isError ? (
          <div className="px-5 pb-3">
            <ErrorNote error={remove.error} />
          </div>
        ) : null}
        {toggle.isError ? (
          <div className="px-5 pb-3">
            <ErrorNote error={toggle.error} />
          </div>
        ) : null}
      </Card>

      {addFor ? (
        <AddConnection
          providers={tiles}
          initialProvider={addFor.provider}
          oauthIds={new Set(oauthById.keys())}
          onClose={() => setAddFor(null)}
          onSaved={(label) => {
            setAddFor(null);
            setToast(`Added ${label}.`);
          }}
        />
      ) : null}

      {signIn ? (
        <OAuthDialog
          provider={signIn.provider}
          authWindow={signIn.authWindow}
          onClose={() => setSignIn(null)}
          onConnected={(label, warning) => setToast(warning ?? `Signed in to ${label}.`)}
        />
      ) : null}

      {toast ? (
        <div
          role="status"
          className="fixed bottom-5 right-5 z-40 flex max-w-[440px] items-start gap-2 rounded-lg border border-[#3a4d1e] bg-[var(--color-accent-soft)] px-3 py-2 text-[13px] text-accent shadow-2xl"
        >
          <Dot tone="pos" />
          <span className="min-w-0 break-words">{toast}</span>
        </div>
      ) : null}
    </>
  );
}

function TestLine({ state }: { state: TestState | undefined }) {
  if (!state) return null;
  if (state.pending) {
    return <span className="text-[11px] text-faint">testing…</span>;
  }
  if (state.error) {
    return (
      <span className="max-w-[320px] truncate text-[11px] text-neg" title={state.error}>
        {state.error}
      </span>
    );
  }
  const result = state.result;
  if (!result) return null;
  if (!result.ok) {
    return (
      <span className="max-w-[320px] truncate text-[11px] text-neg" title={result.error}>
        {result.status} · {result.error ?? "upstream error"}
      </span>
    );
  }
  return (
    <span className="text-[11px] tnum text-pos">
      ok · {result.status} · {ms(result.latency_ms)}
    </span>
  );
}

function AddConnection({
  providers,
  initialProvider,
  oauthIds,
  onClose,
  onSaved,
}: {
  providers: ModelProvider[];
  initialProvider: string;
  oauthIds: Set<string>;
  onClose: () => void;
  onSaved: (label: string) => void;
}) {
  const client = useQueryClient();
  const [provider, setProvider] = useState(initialProvider);
  const [name, setName] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");

  const save = useMutation({
    mutationFn: () =>
      api.post<Connection>("/api/connections", {
        provider,
        auth_type: "api_key",
        name: name.trim() || undefined,
        credentials: {
          api_key: apiKey.trim(),
          ...(baseUrl.trim() ? { base_url_override: baseUrl.trim() } : {}),
        },
      }),
    onSuccess: (connection) => {
      void client.invalidateQueries({ queryKey: qk.connections });
      void client.invalidateQueries({ queryKey: qk.modelProviders });
      void client.invalidateQueries({ queryKey: qk.system });
      onSaved(connection.name ?? connection.provider);
    },
  });

  const display = providers.find((p) => p.id === provider)?.display ?? provider;

  return (
    <Modal title="Add connection" open onClose={onClose} footer={
      <>
        <Button variant="quiet" onClick={onClose}>
          cancel
        </Button>
        <Button
          variant="primary"
          type="submit"
          disabled={!provider || !apiKey.trim() || save.isPending}
          onClick={() => save.mutate()}
        >
          {save.isPending ? "saving…" : "add connection"}
        </Button>
      </>
    }>
      <form
        className="space-y-3"
        onSubmit={(e) => {
          e.preventDefault();
          save.mutate();
        }}
      >
        <Field label="provider" hint={oauthIds.has(provider) ? "also supports sign in from the Providers grid" : undefined}>
          <Select value={provider} onChange={setProvider} ariaLabel="connection provider">
            {providers.map((p) => (
              <option key={p.id} value={p.id}>
                {p.display} ({p.id})
              </option>
            ))}
          </Select>
        </Field>
        <Field label="name" hint="shown in the accounts table; optional">
          <Input value={name} onChange={setName} placeholder={display} ariaLabel="connection name" />
        </Field>
        <Field label="api key" hint="stored encrypted; never shown again">
          <Input
            value={apiKey}
            onChange={setApiKey}
            type="password"
            placeholder="sk-…"
            ariaLabel="api key"
          />
        </Field>
        <Field label="base url override" hint="optional; for a proxy or self-hosted endpoint">
          <Input
            value={baseUrl}
            onChange={setBaseUrl}
            placeholder="https://api.example.com/v1"
            ariaLabel="base url override"
          />
        </Field>
        {save.isError ? <ErrorNote error={save.error} /> : null}
      </form>
    </Modal>
  );
}
