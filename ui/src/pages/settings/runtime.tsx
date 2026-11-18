import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { api, type FetchResult, type SyncResult } from "../../lib/api";
import { qk, useConnections, useSystem } from "../../lib/queries";
import {
  Button,
  Card,
  CardHeader,
  ErrorNote,
  PageHeader,
  Select,
  Toggle,
} from "../../components/ui";
import { UpdateCard } from "../../components/update-card";
import { uptime } from "../../lib/format";
import { GATEWAY_SETTINGS_KEY, useGatewaySettings, type Brand } from "./gateway-settings";

/** What the gateway reports about itself, plus limited registry and runtime maintenance. */
export default function RuntimeSettings() {
  const system = useSystem();
  const settings = useGatewaySettings();
  const connections = useConnections();
  const client = useQueryClient();

  const [connectionId, setConnectionId] = useState("");
  const [metadataBusy, setMetadataBusy] = useState<null | "sync" | "fetch">(null);
  const [metadataNote, setMetadataNote] = useState<string | null>(null);
  const [savingRtk, setSavingRtk] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const counts = system.data?.counts;
  const version = system.data?.version ?? "—";

  const syncMetadata = async () => {
    setMetadataBusy("sync");
    setMetadataNote(null);
    setError(null);
    try {
      const result = await api.post<SyncResult>("/api/models/sync");
      setMetadataNote(`synced ${result.models} models from ${result.providers} providers`);
      await client.invalidateQueries({ queryKey: qk.models });
    } catch (e) {
      setError(e);
    } finally {
      setMetadataBusy(null);
    }
  };

  const fetchMetadata = async () => {
    const target = connectionId || connections.data?.[0]?.id;
    if (!target) return;
    setMetadataBusy("fetch");
    setMetadataNote(null);
    setError(null);
    try {
      const result = await api.post<FetchResult>("/api/models/fetch", { connection_id: target });
      setMetadataNote(`${result.added} new of ${result.count} listed by ${result.provider}`);
      await client.invalidateQueries({ queryKey: qk.models });
    } catch (e) {
      setError(e);
    } finally {
      setMetadataBusy(null);
    }
  };

  const toggleRtk = async () => {
    const next = !(settings.data?.rtk ?? true);
    setSavingRtk(true);
    setError(null);
    try {
      const saved = await api.put<Brand>("/api/settings", { ...(settings.data ?? {}), rtk: next });
      client.setQueryData(GATEWAY_SETTINGS_KEY, saved);
      await client.invalidateQueries({ queryKey: GATEWAY_SETTINGS_KEY });
    } catch (e) {
      setError(e);
    } finally {
      setSavingRtk(false);
    }
  };

  return (
    <>
      <PageHeader
        title="Runtime"
        subtitle="Read-only facts about this process, plus registry and Token Saver maintenance."
      />

      {system.isError ? (
        <div className="mb-4">
          <ErrorNote error={system.error} onRetry={() => void system.refetch()} />
        </div>
      ) : null}
      {error ? (
        <div className="mb-4">
          <ErrorNote error={error} title="Action failed" />
        </div>
      ) : null}

      <Card>
        <CardHeader title="Facts" action={<span className="text-caption text-faint">read-only</span>} />
        <dl className="grid gap-x-8 gap-y-0 px-5 pb-4 text-body md:grid-cols-2">
          <Row k="Version" v={`v${version}`} />
          <Row k="Uptime" v={system.data ? uptime(system.data.uptime_secs) : "—"} />
          <Row k="API keys required" v={system.data?.require_api_key ? "yes" : "no"} />
          <Row k="Remote admin" v={system.data?.allow_remote_admin ? "allowed" : "loopback only"} />
          <Row k="Master key" v={system.data?.master_key_source ?? "—"} />
          <Row k="Static assets" v={system.data?.static_assets ? "served from disk" : "embedded"} />
          <Row
            k="Connections"
            v={counts ? `${counts.active_connections} active of ${counts.connections}` : "—"}
          />
          <Row k="API keys" v={counts ? String(counts.api_keys) : "—"} />
          <Row k="Public models" v={counts ? String(counts.disabled_models) + " disabled" : "—"} />
          <Row k="Aliases / combos" v={counts ? `${counts.aliases} / ${counts.combos}` : "—"} />
        </dl>
        <p className="border-t border-line px-5 py-3 text-small text-faint">
          Bind address, timeouts and other process settings are environment variables; see
          docs/DEPLOY.md. This page never shows credential material.
        </p>
      </Card>

      <Card className="mt-4">
        <CardHeader title="Model metadata" action={<span className="text-caption text-faint">registry-only · never changes routing</span>} />
        <div className="flex flex-col gap-4 px-5 pb-5">
          <div className="grid gap-4 min-[800px]:grid-cols-2">
            <div>
              <p className="text-small leading-relaxed text-muted">
                <span className="font-medium text-fg">sync metadata</span> downloads models.dev and
                updates context windows, prices and capabilities for models this gateway already
                lists. It never adds or removes a model, so it cannot change routing.
              </p>
              <Button className="mt-3" disabled={metadataBusy !== null} onClick={() => void syncMetadata()}>
                {metadataBusy === "sync" ? "syncing…" : "sync metadata"}
              </Button>
            </div>
            <div>
              <p className="text-small leading-relaxed text-muted">
                <span className="font-medium text-fg">fetch</span> asks one connected account for
                the model list the provider itself serves, and stores those ids as upstream rows.
                Use it after adding a provider or when a new model is missing.
              </p>
              <div className="mt-3 flex items-center gap-2">
                <div className="min-w-0 flex-1">
                  <Select
                    ariaLabel="fetch connection"
                    value={connectionId || (connections.data?.[0]?.id ?? "")}
                    onChange={setConnectionId}
                    disabled={(connections.data ?? []).length === 0}
                  >
                    {(connections.data ?? []).length === 0 ? (
                      <option value="">no connections</option>
                    ) : (
                      (connections.data ?? []).map((c) => (
                        <option key={c.id} value={c.id}>
                          {c.provider} · {c.email ?? c.name ?? c.id.slice(0, 8)}
                        </option>
                      ))
                    )}
                  </Select>
                </div>
                <Button
                  disabled={metadataBusy !== null || (connections.data ?? []).length === 0}
                  onClick={() => void fetchMetadata()}
                >
                  {metadataBusy === "fetch" ? "fetching…" : "fetch"}
                </Button>
              </div>
            </div>
          </div>
          {metadataNote ? <p className="text-small text-[var(--color-pos)]">{metadataNote}</p> : null}
        </div>
      </Card>

      <Card className="mt-4">
        <CardHeader
          title="Token Saver"
          action={
            <span className="text-caption text-faint">
              RTK filters · error results are never touched
            </span>
          }
        />
        <div className="flex flex-wrap items-center justify-between gap-4 px-5 pb-5">
          <p className="max-w-[62ch] text-small leading-relaxed text-muted">
            Compresses noisy tool results (git diffs, grep matches, build logs) before they reach
            the provider: repetition is collapsed and long dumps keep their head and tail with a
            marker. A client can bypass it per request with{" "}
            <code className="font-mono text-caption text-fg">x-rrouter-rtk: off</code>.
          </p>
          <Toggle
            checked={settings.data?.rtk ?? true}
            label="Token Saver"
            disabled={savingRtk}
            onChange={() => void toggleRtk()}
          />
        </div>
      </Card>

      <div className="mt-4">
        <UpdateCard />
      </div>
    </>
  );
}

function Row({ k, v }: { k: string; v: string }) {
  return (
    <div className="flex items-center justify-between gap-4 border-b border-[var(--color-rule)] py-2.5 last:border-0">
      <dt className="text-faint">{k}</dt>
      <dd className="min-w-0 truncate text-right font-mono text-small text-muted tnum">{v}</dd>
    </div>
  );
}
