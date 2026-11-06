import { Link } from "react-router-dom";
import { Badge, Card, CardHeader, Dot, ErrorNote, PageHeader } from "../components/ui";
import { useConnections, useHealth, useSystem } from "../lib/queries";
import { uptime } from "../lib/format";

export default function Health() {
  const health = useHealth();
  const system = useSystem();
  const connections = useConnections();

  const counts = system.data?.counts;
  const active = connections.data?.filter((c) => c.is_active).length ?? 0;
  const degraded = connections.data?.filter((c) => c.backoff_level > 0) ?? [];

  return (
    <>
      <PageHeader
        title="Health"
        subtitle="Process, storage, and provider state as the gateway itself reports it."
        actions={
          <>
            <Link
              to="/setup"
              className="inline-flex h-8 items-center rounded-lg border border-line bg-surface-2 px-3 text-[13px] text-fg hover:border-line-strong"
            >
              Run setup again
            </Link>
            <Badge tone={health.isError ? "neg" : "pos"}>
              <Dot tone={health.isError ? "neg" : "pos"} />
              {health.isError ? "unreachable" : "serving"}
            </Badge>
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
          <div className="text-[12px] text-muted">Version</div>
          <div className="mt-1 text-[22px] font-semibold tnum">v{system.data?.version ?? "—"}</div>
          <div className="mt-2 text-[11px] text-faint">
            uptime {system.data ? uptime(system.data.uptime_secs) : "—"}
          </div>
        </Card>
        <Card className="px-5 py-4">
          <div className="text-[12px] text-muted">Connections</div>
          <div className="mt-1 text-[22px] font-semibold tnum">
            {active}
            <span className="text-[13px] font-normal text-faint"> / {counts?.connections ?? 0} active</span>
          </div>
          <div className="mt-2 text-[11px] text-faint">
            {degraded.length === 0 ? "no provider backoff" : `${degraded.length} in backoff`}
          </div>
        </Card>
        <Card className="px-5 py-4">
          <div className="text-[12px] text-muted">API keys</div>
          <div className="mt-1 text-[22px] font-semibold tnum">{counts?.api_keys ?? 0}</div>
          <div className="mt-2 text-[11px] text-faint">
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
              <p className="text-[13px] text-faint">No provider is connected yet.</p>
            )}
          </div>
        </Card>
        <Card>
          <CardHeader title="Configuration" />
          <dl className="grid grid-cols-2 gap-x-4 gap-y-2 px-5 pb-4 text-[13px]">
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
        <CardHeader title="Connection state" action={<span className="text-[11px] text-faint">live from /api/connections</span>} />
        <div className="px-5 pb-4">
          {(connections.data ?? []).length === 0 ? (
            <p className="text-[13px] text-faint">No connections. Add one under Providers.</p>
          ) : (
            <ul className="divide-y divide-[var(--color-rule)]">
              {(connections.data ?? []).map((c) => (
                <li key={c.id} className="flex items-center gap-3 py-2.5 text-[13px]">
                  <Dot tone={!c.is_active ? "muted" : c.backoff_level > 0 ? "warn" : "pos"} />
                  <span className="min-w-0 flex-1 truncate">{c.name ?? c.provider}</span>
                  <span className="text-[11px] text-faint">{c.provider}</span>
                  {c.backoff_level > 0 ? <Badge tone="warn">backoff {c.backoff_level}</Badge> : null}
                  <Badge tone={c.is_active ? "pos" : "neutral"}>{c.is_active ? "active" : "off"}</Badge>
                </li>
              ))}
            </ul>
          )}
        </div>
      </Card>
    </>
  );
}
