import { useMemo, useState } from "react";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  Field,
  Input,
  Modal,
  PageHeader,
  cx,
} from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { Checkbox, Heading, Toggle } from "../components/atoms";
import { when } from "../lib/format";
import {
  useCreateWebhook,
  useDeleteWebhook,
  useIsAdmin,
  useUpdateWebhook,
  useWebhookStatus,
  useWebhooks,
} from "../lib/queries";
import type { Webhook, WebhookAttempt, WebhookStatus } from "../lib/api";

/**
 * Webhooks: outbound deliveries for events this gateway records.
 *
 * The registry is admin surface. A row exposes the *name* of the signing secret's
 * environment variable and whether it is set — never a value — and the status screen lists
 * attempts without payloads, because the queue does not carry them to the API.
 */
export default function Webhooks() {
  const isAdmin = useIsAdmin();
  const list = useWebhooks(isAdmin);
  const status = useWebhookStatus(isAdmin);
  const update = useUpdateWebhook();
  const remove = useDeleteWebhook();

  const [editing, setEditing] = useState<Webhook | "new" | null>(null);
  const [deleting, setDeleting] = useState<Webhook | null>(null);
  const [failure, setFailure] = useState<unknown>(null);

  const rows = list.data?.webhooks ?? [];
  const vocabulary = useMemo(() => {
    const types = list.data?.event_types;
    if (Array.isArray(types) && types.length > 0) return types;
    // A server build that predates the array contract still answers with the csv copy.
    return (list.data?.event_types_csv ?? "")
      .split(",")
      .map((entry) => entry.trim())
      .filter(Boolean);
  }, [list.data]);

  // Enabled but unable to deliver: the named environment variable is not set on the
  // gateway host, so every matching event would fail its first attempt.
  const unready = rows.filter((row) => row.enabled && !row.secret_configured);

  if (!isAdmin) {
    return (
      <Card className="px-6 py-10 text-center">
        <Heading level={2} className="text-heading font-medium">Webhooks are admin-only</Heading>
        <p className="mx-auto mt-2 max-w-[420px] text-body text-muted">
          Delivery endpoints and their signing configuration are managed by a gateway
          administrator.
        </p>
      </Card>
    );
  }

  const columns: Array<Column<Webhook>> = [
    {
      key: "url",
      title: "Endpoint",
      render: (row) => (
        <div className="min-w-0">
          <div className="max-w-[420px] truncate font-mono text-small text-fg" title={row.url}>
            {row.url}
          </div>
          <div className="mt-0.5 text-caption text-faint">
            {row.updated_at ? `updated ${when(row.updated_at)}` : `added ${when(row.created_at)}`}
          </div>
        </div>
      ),
    },
    {
      key: "events",
      title: "Events",
      width: "16rem",
      render: (row) =>
        row.event_types.includes("*") ? (
          <Badge tone="accent" title="every event this gateway records">
            all events
          </Badge>
        ) : (
          <span className="flex flex-wrap gap-1">
            {row.event_types.map((type) => (
              <Badge key={type} tone="neutral">
                {type}
              </Badge>
            ))}
          </span>
        ),
    },
    {
      key: "secret",
      title: "Signing",
      width: "14rem",
      render: (row) => (
        <div className="min-w-0">
          <Badge
            tone={!row.has_secret ? "warn" : row.secret_configured ? "pos" : "warn"}
            title={
              !row.has_secret
                ? "no signing secret is configured for this endpoint"
                : row.secret_configured
                  ? "the signing variable resolves in the gateway environment"
                  : "the named variable is not set (or is not UTF-8) in the gateway environment"
            }
          >
            {!row.has_secret ? "unsigned" : row.secret_configured ? "signed" : "secret not set"}
          </Badge>
          <div
            className="mt-0.5 truncate font-mono text-caption text-faint"
            title="environment variable name, never its value"
          >
            {row.secret_env ?? "no secret configured"}
          </div>
          {!row.secret_configured && row.has_secret ? (
            <div className="mt-0.5 text-caption text-warn">
              deliveries will fail until the variable is set
            </div>
          ) : null}
        </div>
      ),
    },
    {
      key: "enabled",
      title: "Enabled",
      width: "6rem",
      render: (row) => (
        <div className="flex flex-col gap-0.5">
          <Toggle
            size="sm"
            checked={row.enabled}
            label={`${row.url} ${row.enabled ? "enabled" : "disabled"}`}
            disabled={update.isPending}
            onChange={(next) =>
              update.mutate({ id: row.id, patch: { enabled: next } }, { onError: setFailure })
            }
          />
          <span className="text-caption text-faint">
            {row.ready ? "ready" : row.enabled ? "cannot deliver" : "not receiving"}
          </span>
        </div>
      ),
    },
    {
      key: "actions",
      title: "Actions",
      srOnlyTitle: true,
      align: "right",
      width: "9rem",
      render: (row) => (
        <span className="flex justify-end gap-1.5">
          <Button variant="quiet" onClick={() => setEditing(row)}>
            edit
          </Button>
          <Button variant="danger" onClick={() => setDeleting(row)}>
            delete
          </Button>
        </span>
      ),
    },
  ];

  return (
    <>
      {editing ? (
        <WebhookModal
          webhook={editing === "new" ? null : editing}
          vocabulary={vocabulary}
          onClose={() => setEditing(null)}
        />
      ) : null}

      <ConfirmDialog
        open={deleting !== null}
        title="Delete webhook endpoint"
        message={
          <>
            Remove <span className="font-mono">{deleting?.url}</span>? It stops receiving
            events immediately. The signing secret lives in the environment, not in RRouter,
            so nothing else is touched.
          </>
        }
        confirmLabel="Delete endpoint"
        busy={remove.isPending}
        error={remove.error ?? failure}
        onConfirm={() => {
          if (deleting) {
            remove.mutate(deleting.id, {
              onError: setFailure,
              onSuccess: () => setDeleting(null),
            });
          }
        }}
        onClose={() => {
          if (!remove.isPending) {
            remove.reset();
            setFailure(null);
            setDeleting(null);
          }
        }}
      />

      <PageHeader
        title="Webhooks"
        subtitle="Deliver this gateway's events to an HTTP endpoint. Deliveries are signed with the secret named on each endpoint."
        actions={
          <>
            <Button
              variant="quiet"
              onClick={() => void status.refetch()}
              disabled={status.isFetching}
            >
              {status.isFetching ? "refreshing…" : "refresh status"}
            </Button>
            <Button
              variant="primary"
              onClick={() => setEditing("new")}
              disabled={(list.data?.max ?? 0) > 0 && rows.length >= (list.data?.max ?? 0)}
            >
              Add endpoint
            </Button>
          </>
        }
      />

      {unready.length > 0 ? (
        <div
          role="alert"
          className="mb-4 rounded-lg border border-warn/40 bg-warn/10 px-4 py-3 text-body text-fg"
        >
          <span className="font-medium">
            {unready.length === 1
              ? "1 enabled endpoint cannot deliver"
              : `${unready.length} enabled endpoints cannot deliver`}
          </span>
          <span className="text-muted">
            {" "}
            — the signing variable is not set in the gateway environment:
          </span>{" "}
          {unready.map((row) => (
            <span key={row.id} className="mr-2 font-mono text-small">
              {row.secret_env ?? "unnamed"}
            </span>
          ))}
          <div className="mt-1 text-caption text-muted">
            Export the variable and restart, or disable the endpoint until it is in place. Every
            matching event fails its first attempt until then.
          </div>
        </div>
      ) : null}
      {failure ? (
        <div className="mb-4">
          <ErrorNote error={failure} />
        </div>
      ) : null}
      {list.isError ? (
        <div className="mb-4">
          <ErrorNote error={list.error} onRetry={() => void list.refetch()} />
        </div>
      ) : null}

      <Card className="mb-6">
        <CardHeader
          title="Delivery queue"
          action={
            <span className="flex items-center gap-2 text-caption text-faint">
              <QueueHealth status={status.data} error={status.isError} />
            </span>
          }
        />
        {status.isError ? (
          <div className="px-6 pb-5">
            <ErrorNote error={status.error} onRetry={() => void status.refetch()} />
          </div>
        ) : (
          <>
            <div className="grid gap-3 px-6 pb-4 sm:grid-cols-5">
              <Figure label="pending" value={status.data?.outbox.pending ?? 0} tone="neutral" />
              <Figure
                label="delivering / retrying"
                value={status.data?.outbox.delivering ?? 0}
                tone="warn"
              />
              <Figure label="delivered" value={status.data?.outbox.delivered ?? 0} tone="pos" />
              <Figure
                label="unmatched"
                value={status.data?.outbox.unmatched ?? 0}
                tone="neutral"
              />
              <Figure label="dead" value={status.data?.outbox.dead ?? 0} tone="neg" />
            </div>
            <DataTable
              columns={attemptColumns}
              rows={status.data?.recent}
              rowKey={(row) => String(row.id)}
              loading={status.isLoading}
              scrollLabel="Recent webhook attempts"
              maxBodyHeight="20rem"
              empty="No deliveries yet. Events appear here once an endpoint is registered and something happens."
            />
            <p className="border-t border-line px-6 py-3 text-caption text-faint">
              Attempts are listed without payloads: the queue does not carry request bodies to
              this screen, and a signing secret is never shown. <span className="text-muted">unmatched</span> is
              terminal: the event was recorded but no enabled endpoint subscribed to its type.
            </p>
          </>
        )}
      </Card>

      <Card>
        <CardHeader
          title="Endpoints"
          action={
            <span className="text-caption text-faint tnum">
              {rows.length}
              {list.data?.max ? ` of ${list.data.max}` : ""} registered
            </span>
          }
        />
        {list.isLoading ? (
          <DataTable columns={columns} rows={undefined} rowKey={(row) => row.id} loading scrollLabel="Webhook endpoints" />
        ) : rows.length === 0 ? (
          <EmptyState>
            No webhook endpoints yet. Add one to receive events such as
            <span className="font-mono"> key.created</span> or
            <span className="font-mono"> connection.degraded</span>.
          </EmptyState>
        ) : (
          <DataTable
            columns={columns}
            rows={rows}
            rowKey={(row) => row.id}
            rowHeaderKey="url"
            scrollLabel="Webhook endpoints"
          />
        )}
      </Card>
    </>
  );
}

const attemptColumns: Array<Column<WebhookAttempt>> = [
  {
    key: "when",
    title: "Queued",
    width: "10rem",
    render: (row) => <span className="whitespace-nowrap text-muted">{when(row.created_at)}</span>,
  },
  {
    key: "type",
    title: "Event",
    render: (row) => <span className="font-mono text-small">{row.type}</span>,
  },
  {
    key: "status",
    title: "Status",
    width: "8rem",
    render: (row) => (
      <Badge
        tone={STATUS_TONE[row.status] ?? "neutral"}
        title={
          row.status === "unmatched"
            ? "terminal: no enabled endpoint subscribed to this event type"
            : undefined
        }
      >
        {row.status}
      </Badge>
    ),
  },
  {
    key: "attempts",
    title: "Attempt",
    align: "right",
    width: "6rem",
    render: (row) => <span className="text-muted tnum">{row.attempts}</span>,
  },
  {
    key: "error",
    title: "Last error",
    render: (row) => (
      <span
        className={cx(
          "block max-w-[360px] truncate font-mono text-caption",
          // `unmatched` records a reason, not a failure: keep it quiet.
          row.status === "unmatched" ? "text-muted" : row.last_error ? "text-neg" : "text-faint",
        )}
        title={row.last_error ?? ""}
      >
        {row.last_error ?? "—"}
      </span>
    ),
  },
  {
    key: "next",
    title: "Next",
    width: "10rem",
    render: (row) =>
      row.status === "unmatched" ? (
        <span className="whitespace-nowrap text-caption text-faint">no retry</span>
      ) : row.delivered_at ? (
        <span className="whitespace-nowrap text-caption text-pos">delivered</span>
      ) : row.next_attempt_at ? (
        <span className="whitespace-nowrap text-caption text-muted">
          {when(row.next_attempt_at)}
        </span>
      ) : (
        <span className="text-faint">—</span>
      ),
  },
];

const STATUS_TONE: Record<string, "pos" | "warn" | "neg" | "accent" | "neutral"> = {
  delivered: "pos",
  dead: "neg",
  pending: "warn",
  delivering: "accent",
  unmatched: "neutral",
};

function QueueHealth({ status, error }: { status?: WebhookStatus; error: boolean }) {
  if (error || !status) return <span>queue unavailable</span>;
  const { pending, delivering, dead } = status.outbox;
  if (dead > 0) {
    return (
      <Badge tone="neg" title="deliveries that exhausted their retries">
        {dead} dead
      </Badge>
    );
  }
  if (pending + delivering > 0) {
    return (
      <span className="flex items-center gap-2">
        <Badge tone="warn">in flight</Badge>
        <span className="tnum">depth {pending + delivering}</span>
      </span>
    );
  }
  return <Badge tone="pos">queue empty</Badge>;
}

function Figure({
  label,
  value,
  tone,
}: {
  label: string;
  value: number;
  tone: "pos" | "warn" | "neg" | "neutral";
}) {
  return (
    <div className="rounded-lg border border-line bg-surface-2 px-3 py-2">
      <div className="text-caption uppercase tracking-[0.08em] text-faint">{label}</div>
      <div
        className={cx(
          "mt-0.5 text-subtitle font-semibold tnum",
          value === 0 && "text-faint",
          value > 0 && tone === "neg" && "text-neg",
          value > 0 && tone === "warn" && "text-warn",
          value > 0 && tone === "pos" && "text-pos",
          value > 0 && tone === "neutral" && "text-fg",
        )}
      >
        {value.toLocaleString()}
      </div>
    </div>
  );
}

/** The signing variable is a name; the value lives in the gateway's environment. */
function secretProblem(value: string, required: boolean): string | null {
  const trimmed = value.trim();
  if (trimmed === "") return required ? "Name the environment variable that holds the signing secret." : null;
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(trimmed)) {
    return "Use an environment variable name: letters, digits and underscores, not starting with a digit.";
  }
  return null;
}

function urlProblem(value: string): string | null {
  const trimmed = value.trim();
  if (trimmed === "") return "An https:// URL is required.";
  if (trimmed.startsWith("https://")) return null;
  if (trimmed.startsWith("http://")) {
    const host = trimmed.slice("http://".length).split(/[/?#]/)[0]?.split(":")[0] ?? "";
    if (["localhost", "127.0.0.1", "[::1]"].includes(host)) return null;
    return "Plain http:// is only accepted for a loopback receiver; use https://.";
  }
  return "The URL must start with https://.";
}

function WebhookModal({
  webhook,
  vocabulary,
  onClose,
}: {
  webhook: Webhook | null;
  vocabulary: string[];
  onClose: () => void;
}) {
  const create = useCreateWebhook();
  const update = useUpdateWebhook();

  const [url, setUrl] = useState(webhook?.url ?? "");
  const [secret, setSecret] = useState("");
  const [enabled, setEnabled] = useState(webhook?.enabled ?? true);
  const [star, setStar] = useState(webhook?.event_types.includes("*") ?? false);
  const [selected, setSelected] = useState<string[]>(
    (webhook?.event_types ?? []).filter((type) => type !== "*"),
  );
  const [problem, setProblem] = useState<string | null>(null);

  const busy = create.isPending || update.isPending;
  const named = vocabulary.filter((type) => type !== "*");
  const urlIssue = url.trim() === "" ? null : urlProblem(url);
  const secretIssue = secretProblem(secret, webhook === null);
  const events = star ? ["*"] : selected;

  const submit = () => {
    const problem =
      urlProblem(url) ??
      secretProblem(secret, webhook === null) ??
      (star || selected.length > 0 ? null : "Choose at least one event type, or all events.");
    if (problem) {
      setProblem(problem);
      return;
    }
    setProblem(null);
    if (webhook === null) {
      create.mutate(
        { url: url.trim(), event_types: events, secret_env: secret.trim() },
        { onSuccess: onClose },
      );
    } else {
      update.mutate(
        {
          id: webhook.id,
          patch: {
            url: url.trim(),
            event_types: events,
            enabled,
            // Blank means "keep the current variable": the API cannot clear it.
            ...(secret.trim() ? { secret_env: secret.trim() } : {}),
          },
        },
        { onSuccess: onClose },
      );
    }
  };

  return (
    <Modal
      title={webhook ? "Edit webhook" : "Add webhook endpoint"}
      width="lg"
      open
      onClose={onClose}
      description="Deliveries are POSTed with an HMAC signature derived from the named environment variable. The dashboard never stores or displays that value."
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={busy}>
            {busy ? "saving…" : webhook ? "save" : "add endpoint"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <Field label="url" hint="https:// is required; http:// only for a loopback receiver">
          <Input
            value={url}
            onChange={setUrl}
            placeholder="https://hooks.example.com/rrouter"
            ariaLabel="webhook url"
            disabled={busy}
          />
        </Field>
        {urlIssue ? (
          <span role="alert" className="text-caption text-neg">
            {urlIssue}
          </span>
        ) : null}

        <Field
          label="signing secret variable"
          hint={
            webhook?.secret_env
              ? `the name of the environment variable; currently ${webhook.secret_env}. Leave blank to keep it.`
              : "the name of the environment variable that holds the signing secret"
          }
        >
          <Input
            value={secret}
            onChange={setSecret}
            placeholder={webhook?.secret_env ?? "RR_WEBHOOK_SECRET"}
            ariaLabel="webhook secret environment variable"
            disabled={busy}
          />
        </Field>
        {secret !== "" && secretIssue ? (
          <span role="alert" className="text-caption text-neg">
            {secretIssue}
          </span>
        ) : null}

        <section className="flex min-w-0 flex-col gap-2">
          <div className="flex items-baseline justify-between gap-3">
            <span className="text-caption font-medium uppercase tracking-[0.08em] text-faint">
              event types
            </span>
            <span className="text-caption text-faint tnum">
              {star ? "all events" : `${selected.length} selected`}
            </span>
          </div>
          <div className="flex items-start gap-2 text-body text-fg">
            <Checkbox
              checked={star}
              disabled={busy}
              ariaLabel="subscribe to all events"
              onChange={(next) => {
                setStar(next);
                if (next) setSelected([]);
              }}
              className="mt-0.5"
            />
            <span>
              all events
              <span className="mt-0.5 block text-caption text-faint">
                subscribes with <span className="font-mono">*</span>, including events added in
                a later release
              </span>
            </span>
          </div>
          <div className="grid gap-x-4 gap-y-1.5 sm:grid-cols-2">
            {named.map((type) => (
              <label
                key={type}
                className={cx(
                  "flex items-center gap-2 rounded px-1 py-0.5 text-small",
                  star || busy ? "cursor-not-allowed opacity-60" : "cursor-pointer hover:bg-surface-3",
                )}
              >
                <Checkbox
                  checked={selected.includes(type)}
                  disabled={star || busy}
                  ariaLabel={`subscribe to ${type}`}
                  onChange={() =>
                    setSelected((current) =>
                      current.includes(type)
                        ? current.filter((entry) => entry !== type)
                        : [...current, type],
                    )
                  }
                />
                <span className="font-mono">{type}</span>
              </label>
            ))}
          </div>
          {vocabulary.length === 0 ? (
            <p className="text-caption text-faint">
              The event vocabulary could not be loaded; reload this page before saving.
            </p>
          ) : null}
        </section>

        {webhook ? (
          <div className="flex items-start gap-2 text-body text-fg">
            <Checkbox
              checked={enabled}
              disabled={busy}
              ariaLabel="endpoint enabled"
              onChange={setEnabled}
              className="mt-0.5"
            />
            <span>
              enabled
              <span className="mt-0.5 block text-caption text-faint">
                a disabled endpoint stays registered but receives nothing
              </span>
            </span>
          </div>
        ) : null}

        {problem ? (
          <span role="alert" className="text-caption text-neg">
            {problem}
          </span>
        ) : null}
        {create.isError ? <ErrorNote error={create.error} /> : null}
        {update.isError ? <ErrorNote error={update.error} /> : null}
      </div>
    </Modal>
  );
}
