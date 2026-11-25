import { Link } from "react-router-dom";
import { DataTable, type Column } from "../components/table";
import { Badge, Card, CardHeader, Dot, ErrorNote, PageHeader } from "../components/ui";
import type { Connection } from "../lib/api";
import { useConnections, useHealth, useSystem } from "../lib/queries";
import { uptime } from "../lib/format";

export function Health() {
  const health = useHealth();
  const system = useSystem();
  const connections = useConnections();

  const counts = system.data?.counts;
  const active = connections.data?.filter((c) => c.is_active).length ?? 0;
  const degraded = connections.data?.filter((c) => c.backoff_level > 0) ?? [];
  const subtitle = "Process, storage, and provider state as the gateway itself reports it.";

  const statusBadge = (
    <Badge tone={health.isError ? "neg" : "pos"}>
      <Dot tone={health.isError ? "neg" : "pos"} />
      {health.isError ? "unreachable" : "serving"}
    </Badge>
  );

  const connectionColumns: Array<Column<Connection>> = [
    {
      key: "name",
      title: "Connection",
      render: (connection) => connection.name ?? connection.provider,
      cellTitle: (connection) => connection.name ?? connection.provider,
    },
    {
      key: "provider",
      title: "Provider",
      width: "160px",
      className: "text-muted",
      render: (connection) => connection.provider,
    },
    {
      key: "backoff",
      title: "Backoff",
      width: "120px",
      align: "right",
      render: (connection) =>
        connection.backoff_level > 0 ? (
          <Badge tone="warn">
            <span className="font-mono tnum">{connection.backoff_level}</span>
          </Badge>
        ) : (
          "—"
        ),
      cellTitle: (connection) =>
        connection.backoff_level > 0 ? `backoff ${connection.backoff_level}` : "no backoff",
    },
    {
      key: "state",
      title: "State",
      width: "110px",
      render: (connection) => (
        <span className="inline-flex items-center gap-1.5">
          <Dot
            tone={
              !connection.is_active ? "muted" : connection.backoff_level > 0 ? "warn" : "pos"
            }
          />
          {connection.is_active ? "active" : "off"}
        </span>
      ),
      cellTitle: (connection) => (connection.is_active ? "active" : "off"),
    },
  ];

  const setupLink = (
    <Link
      to="/setup"
      className="inline-flex h-8 items-center rounded-full border border-line bg-surface-2 px-3 text-body text-fg transition-colors hover:border-line-strong hover:bg-surface-3"
    >
      Run setup again
    </Link>
  );

  return (
    <>
      <PageHeader
        title="Health"
        subtitle={subtitle}
        actions={
          <>
            <Link
              to="/system/logs"
              className="inline-flex h-8 items-center rounded-full border border-line bg-surface px-3 text-body text-muted transition-colors hover:border-line-strong hover:bg-surface-3 hover:text-fg"
            >
              System logs →
            </Link>
            {setupLink}
            {statusBadge}
          </>
        }
      />

      {system.isError ? (
        <div className="mb-4">
          <ErrorNote error={system.error} onRetry={() => void system.refetch()} />
        </div>
      ) : null}

      <div className="grid gap-3 md:grid-cols-3">
        <Card className="px-5 py-4">
          <div className="text-small text-muted">Version</div>
          <div className="mt-1 text-subtitle font-semibold tnum">v{system.data?.version ?? "—"}</div>
          <div className="mt-2 text-caption text-faint tnum">
            uptime {system.data ? uptime(system.data.uptime_secs) : "—"}
          </div>
        </Card>
        <Card className="px-5 py-4">
          <div className="text-small text-muted">Connections</div>
          <div className="mt-1 text-subtitle font-semibold tnum">
            {active}
            <span className="text-body font-normal text-faint tnum"> / {counts?.connections ?? 0} active</span>
          </div>
          <div className="mt-2 text-caption text-faint tnum">
            {degraded.length === 0 ? "no provider backoff" : `${degraded.length} in backoff`}
          </div>
        </Card>
        <Card className="px-5 py-4">
          <div className="text-small text-muted">API keys</div>
          <div className="mt-1 text-subtitle font-semibold tnum">{counts?.api_keys ?? 0}</div>
          <div className="mt-2 text-caption text-faint">
            {system.data?.require_api_key ? "enforced" : "not enforced"}
          </div>
        </Card>
      </div>

      <div className="mt-3 grid gap-3 lg:grid-cols-2">
        <Card>
          <CardHeader title="Providers with traffic" />
          <div className="px-5 pb-4">
            {counts?.providers.length ? (
              <div className="flex flex-wrap gap-1.5">
                {counts.providers.map((p) => (
                  <Badge key={p} tone="accent">
                    {p}
                  </Badge>
                ))}
              </div>
            ) : (
              <p className="text-body text-faint">No provider is connected yet.</p>
            )}
          </div>
        </Card>
        <Card>
          <CardHeader title="Configuration" />
          <dl className="grid grid-cols-2 gap-x-4 gap-y-2 px-5 pb-4 text-body">
            <dt className="text-muted">require_api_key</dt>
            <dd className="text-right tnum">{String(system.data?.require_api_key ?? "—")}</dd>
            <dt className="text-muted">allow_remote_admin</dt>
            <dd className="text-right tnum">{String(system.data?.allow_remote_admin ?? "—")}</dd>
            <dt className="text-muted">static_assets</dt>
            <dd className="text-right tnum">{String(system.data?.static_assets ?? "—")}</dd>
            <dt className="text-muted">master_key_source</dt>
            <dd className="text-right">{system.data?.master_key_source ?? "—"}</dd>
            <dt className="text-muted">disabled_models</dt>
            <dd className="text-right tnum">{counts?.disabled_models ?? 0}</dd>
            <dt className="text-muted">aliases / combos</dt>
            <dd className="text-right tnum">
              {counts?.aliases ?? 0} / {counts?.combos ?? 0}
            </dd>
          </dl>
        </Card>
      </div>

      <Card className="mt-3">
        <CardHeader title="Connection state" action={<span className="text-caption text-faint">live from /api/connections</span>} />
        <DataTable<Connection>
          columns={connectionColumns}
          rows={connections.data}
          rowKey={(connection) => connection.id}
          loading={connections.isLoading}
          empty="No connections. Add one under Providers."
          caption="Connection state"
          scrollLabel="connections"
        />
      </Card>
    </>
  );
}

export default Health;
