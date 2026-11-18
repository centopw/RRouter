import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { api, type FetchResult, type SyncResult } from "../../lib/api";
import { qk, useConnections, useSystem } from "../../lib/queries";
import {
  AlertBanner,
  Button,
  Card,
  CardHeader,
  ErrorNote,
  Input,
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
  const [savingCapture, setSavingCapture] = useState(false);
  const [savingRetention, setSavingRetention] = useState(false);
  const [sweepBusy, setSweepBusy] = useState(false);
  const [sweepNote, setSweepNote] = useState<string | null>(null);
  const [draft, setDraft] = useState<RetentionDraft>(emptyDraft);
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

  // The settings query is typed for the white-label fields; capture rides in the same
  // flat blob, so read it through a narrow view instead of widening the shared type.
  const captureOn = (settings.data as (Brand & { capture_bodies?: boolean }) | undefined)?.capture_bodies ?? false;

  const toggleCapture = async (next: boolean) => {
    setSavingCapture(true);
    setError(null);
    try {
      const saved = await api.put<Brand>("/api/settings", {
        ...(settings.data ?? {}),
        capture_bodies: next,
      });
      client.setQueryData(GATEWAY_SETTINGS_KEY, saved);
      await client.invalidateQueries({ queryKey: GATEWAY_SETTINGS_KEY });
    } catch (e) {
      setError(e);
    } finally {
      setSavingCapture(false);
    }
  };

  const stored = settings.data as (Brand & HistorySettings) | undefined;
  // Seed the draft once the blob arrives; a later save writes the same shape back.
  const [seeded, setSeeded] = useState(false);
  if (!seeded && stored) {
    setSeeded(true);
    setDraft(draftFrom(stored));
  }

  const saveRetention = async () => {
    const patch = patchFrom(draft);
    setSavingRetention(true);
    setError(null);
    try {
      const saved = await api.put<Brand>("/api/settings", { ...(settings.data ?? {}), ...patch });
      client.setQueryData(GATEWAY_SETTINGS_KEY, saved);
      await client.invalidateQueries({ queryKey: GATEWAY_SETTINGS_KEY });
    } catch (e) {
      setError(e);
    } finally {
      setSavingRetention(false);
    }
  };

  const sweepNow = async () => {
    setSweepBusy(true);
    setSweepNote(null);
    setError(null);
    try {
      const report = await api.post<Record<string, number>>("/api/settings/retention/sweep");
      const dropped = Object.entries(report)
        .filter(([, n]) => n > 0)
        .map(([name, n]) => `${name} ${n}`)
        .join(" · ");
      setSweepNote(dropped ? `removed ${dropped}` : "nothing was past its cutoff");
      await client.invalidateQueries({ queryKey: GATEWAY_SETTINGS_KEY });
    } catch (e) {
      setError(e);
    } finally {
      setSweepBusy(false);
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

      <Card className="mt-4">
        <CardHeader
          title="Request capture"
          action={
            <span className="text-caption text-faint">
              {captureOn ? "on · writing bodies to the local database" : "off by default"}
            </span>
          }
        />
        <div className="flex flex-col gap-4 px-5 pb-5">
          <AlertBanner tone="warn" title="This stores prompt and response content on this machine">
            Turning capture on writes each request body and the response the client received —
            including streamed SSE frames — into the gateway's local SQLite database. The values of{" "}
            <code className="font-mono text-caption">authorization</code>,{" "}
            <code className="font-mono text-caption">x-api-key</code>,{" "}
            <code className="font-mono text-caption">api-key</code> and{" "}
            <code className="font-mono text-caption">cookie</code> are redacted before writing, but
            the conversation itself is kept in cleartext. Bodies are capped at 64 KiB each and
            pruned with usage retention.
          </AlertBanner>
          <div className="flex flex-wrap items-center justify-between gap-4">
            <p className="max-w-[62ch] text-small leading-relaxed text-muted">
              Needed only to debug a specific request in the inspector, for example to see the
              exact prompt a provider received. Leave it off otherwise.
            </p>
            <Toggle
              checked={captureOn}
              label="Capture request and response bodies"
              disabled={savingCapture}
              onChange={(next) => void toggleCapture(next)}
            />
          </div>
        </div>
      </Card>

      <Card className="mt-4">
        <CardHeader
          title="Stored history"
          action={
            <span className="text-caption text-faint">
              swept daily · 0 keeps until deleted by hand
            </span>
          }
        />
        <div className="flex flex-col gap-4 px-5 pb-5">
          <p className="max-w-[70ch] text-small leading-relaxed text-muted">
            Every store the gateway keeps has its own lifetime. A blank box follows the usage
            window; a switch turns the store off immediately (nothing new is written) without
            touching rows already there.
          </p>
          <div className="overflow-x-auto">
            <table className="w-full border-collapse text-body">
              <thead>
                <tr className="border-b border-line text-caption text-faint">
                  <th className="py-2 pr-4 text-left font-medium">store</th>
                  <th className="py-2 pr-4 text-left font-medium">record</th>
                  <th className="py-2 text-left font-medium">keep (days)</th>
                </tr>
              </thead>
              <tbody>
                <RetentionRow
                  what="Usage, cost and quota history"
                  detail="The accounting ledger: budgets, quotas and the Activity page read it, so it cannot be switched off."
                  value={draft.usage_retention_days}
                  onChange={(v) => setDraft({ ...draft, usage_retention_days: v })}
                />
                <RetentionRow
                  what="Attempt traces"
                  detail="One row per dispatch attempt: provider, model, status and latency. Backs the request inspector."
                  toggle={{
                    on: draft.record_attempts,
                    label: "record attempts",
                    onChange: (on) => setDraft({ ...draft, record_attempts: on }),
                  }}
                  value={draft.attempts_retention_days}
                  onChange={(v) => setDraft({ ...draft, attempts_retention_days: v })}
                />
                <RetentionRow
                  what="Captured bodies"
                  detail="Prompt and response bodies, written only while Request capture is on."
                  value={draft.capture_retention_days}
                  onChange={(v) => setDraft({ ...draft, capture_retention_days: v })}
                />
                <RetentionRow
                  what="Audit trail"
                  detail="Admin and identity events, and the webhook fan-out they feed."
                  toggle={{
                    on: draft.record_audit,
                    label: "record audit events",
                    onChange: (on) => setDraft({ ...draft, record_audit: on }),
                  }}
                  value={draft.audit_retention_days}
                  onChange={(v) => setDraft({ ...draft, audit_retention_days: v })}
                />
                <RetentionRow
                  what="System logs"
                  detail="The in-memory console buffer behind the System logs page; its switch lives there."
                  value={draft.system_log_retention_days}
                  onChange={(v) => setDraft({ ...draft, system_log_retention_days: v })}
                />
              </tbody>
            </table>
          </div>
          <div className="flex flex-wrap items-center gap-3">
            <Button variant="primary" disabled={savingRetention || !seeded} onClick={() => void saveRetention()}>
              {savingRetention ? "saving…" : "save retention"}
            </Button>
            <Button variant="quiet" disabled={sweepBusy} onClick={() => void sweepNow()}>
              {sweepBusy ? "sweeping…" : "run sweep now"}
            </Button>
            {sweepNote ? <span className="text-small text-muted">{sweepNote}</span> : null}
          </div>
          <p className="text-caption text-faint">
            A stored number is clamped to 3650 days; a value outside that range is refused, not
            silently changed. The next scheduled sweep applies a change within a day, and “run
            sweep now” applies it immediately.
          </p>
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

/** The retention half of the settings blob; the white-label type stays untouched. */
type HistorySettings = {
  record_attempts?: boolean;
  record_audit?: boolean;
  usage_retention_days?: number | null;
  attempts_retention_days?: number | null;
  capture_retention_days?: number | null;
  audit_retention_days?: number | null;
  system_log_retention_days?: number | null;
};

/** Text drafts, because an empty box means "follow the usage window", not zero. */
type RetentionDraft = {
  record_attempts: boolean;
  record_audit: boolean;
  usage_retention_days: string;
  attempts_retention_days: string;
  capture_retention_days: string;
  audit_retention_days: string;
  system_log_retention_days: string;
};

const emptyDraft: RetentionDraft = {
  record_attempts: true,
  record_audit: true,
  usage_retention_days: "0",
  attempts_retention_days: "",
  capture_retention_days: "",
  audit_retention_days: "",
  system_log_retention_days: "",
};

function asText(value: number | null | undefined): string {
  return typeof value === "number" ? String(value) : "";
}

function draftFrom(stored: HistorySettings): RetentionDraft {
  return {
    record_attempts: stored.record_attempts ?? true,
    record_audit: stored.record_audit ?? true,
    usage_retention_days: asText(stored.usage_retention_days) || "0",
    attempts_retention_days: asText(stored.attempts_retention_days),
    capture_retention_days: asText(stored.capture_retention_days),
    audit_retention_days: asText(stored.audit_retention_days),
    system_log_retention_days: asText(stored.system_log_retention_days),
  };
}

/** A blank box becomes `null`: the store follows the usage window. */
function patchFrom(draft: RetentionDraft): HistorySettings {
  const days = (value: string): number | null => {
    const trimmed = value.trim();
    return trimmed === "" ? null : Math.max(0, Math.min(3650, Number(trimmed) || 0));
  };
  return {
    record_attempts: draft.record_attempts,
    record_audit: draft.record_audit,
    usage_retention_days: days(draft.usage_retention_days),
    attempts_retention_days: days(draft.attempts_retention_days),
    capture_retention_days: days(draft.capture_retention_days),
    audit_retention_days: days(draft.audit_retention_days),
    system_log_retention_days: days(draft.system_log_retention_days),
  };
}

function RetentionRow({
  what,
  detail,
  toggle,
  value,
  onChange,
}: {
  what: string;
  detail: string;
  toggle?: { on: boolean; label: string; onChange: (on: boolean) => void };
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <tr className="border-b border-[var(--color-rule)] align-top last:border-0">
      <td className="py-3 pr-4">
        <div className="font-medium text-fg">{what}</div>
        <p className="mt-0.5 max-w-[52ch] text-caption leading-snug text-faint">{detail}</p>
      </td>
      <td className="py-3 pr-4">
        {toggle ? (
          <Toggle checked={toggle.on} label={toggle.label} onChange={toggle.onChange} />
        ) : (
          <span className="text-caption text-faint">always on</span>
        )}
      </td>
      <td className="w-[110px] py-2.5">
        <Input
          type="number"
          size="sm"
          mono
          value={value}
          onChange={onChange}
          placeholder="follow usage"
          ariaLabel={`${what} retention in days`}
        />
      </td>
    </tr>
  );
}
