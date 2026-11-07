import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { api } from "../lib/api";
import { useSystem } from "../lib/queries";
import { Button, Card, CardHeader, ErrorNote, Field, Input, PageHeader, cx } from "../components/ui";
import { uptime } from "../lib/format";

/** White-label brand fields stored through `/api/settings` (flat object). */
type Brand = {
  brand_name?: string;
  brand_color?: string;
  logo_url?: string;
  footer_text?: string;
  /** Token Saver: compress noisy tool outputs before they reach the provider. */
  rtk?: boolean;
};

const BRAND_KEYS = ["brand_name", "brand_color", "logo_url", "footer_text"] as const;

export default function Settings() {
  const system = useSystem();
  const client = useQueryClient();
  const settings = useQuery({
    queryKey: ["gateway-settings"],
    queryFn: () => api.get<Brand>("/api/settings"),
    staleTime: 60_000,
  });

  const [draft, setDraft] = useState<Brand>({});
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [savingRtk, setSavingRtk] = useState(false);
  const [error, setError] = useState<unknown>(null);

  useEffect(() => {
    if (!settings.data || dirty) return;
    setDraft(settings.data);
  }, [settings.data, dirty]);

  const set = (key: (typeof BRAND_KEYS)[number]) => (value: string) => {
    setDraft((d) => ({ ...d, [key]: value }));
    setDirty(true);
    setError(null);
  };

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      await api.put<Brand>("/api/settings", draft);
      setDirty(false);
      await client.invalidateQueries({ queryKey: ["gateway-settings"] });
    } catch (e) {
      setError(e);
    } finally {
      setSaving(false);
    }
  };

  const color = draft.brand_color?.trim() ?? "";
  const colorOk = /^#[0-9a-f]{3,8}$/i.test(color);
  const counts = system.data?.counts;
  const version = system.data?.version ?? "—";

  return (
    <>
      <PageHeader
        title="Settings"
        subtitle="Branding for this deployment and the runtime facts the gateway reports about itself."
        actions={
          <Link
            to="/setup"
            className="inline-flex h-8 items-center rounded-lg border border-line bg-surface-2 px-3 text-[13px] hover:border-line-strong"
          >
            Run setup again
          </Link>
        }
      />

      {settings.isError ? (
        <div className="mb-4">
          <ErrorNote error={settings.error} onRetry={() => void settings.refetch()} />
        </div>
      ) : null}

      <Card>
        <CardHeader
          title="Branding"
          action={<span className="text-[11.5px] text-faint">applies to the dashboard</span>}
        />
        <div className="grid max-w-[720px] gap-4 px-5 pb-5 md:grid-cols-2">
          <Field label="Name" hint="Shown in the sidebar, the phone header and the tab title.">
            <Input
              value={draft.brand_name ?? ""}
              onChange={set("brand_name")}
              placeholder="RRouter"
              ariaLabel="brand name"
            />
          </Field>

          <Field label="Accent colour" hint="Any CSS hex colour; used for highlights and focus.">
            <div className="flex items-center gap-2">
              <input
                type="color"
                aria-label="accent colour picker"
                value={colorOk ? color : ""}
                onChange={(e) => set("brand_color")(e.target.value)}
                className="h-8 w-10 shrink-0 cursor-pointer rounded-lg border border-line bg-surface-2"
              />
              <Input
                value={draft.brand_color ?? ""}
                onChange={set("brand_color")}
                placeholder="#1c2b42"
                ariaLabel="accent colour hex"
              />
            </div>
          </Field>

          <Field label="Logo URL" hint="http(s) or data:image URL. Empty keeps the default mark.">
            <Input
              value={draft.logo_url ?? ""}
              onChange={set("logo_url")}
              placeholder="https://example.com/logo.svg"
              ariaLabel="logo url"
            />
          </Field>

          <Field label="Footer note" hint="Replaces “MIT” under the version in the sidebar.">
            <Input
              value={draft.footer_text ?? ""}
              onChange={set("footer_text")}
              placeholder="Internal gateway"
              ariaLabel="footer note"
            />
          </Field>
        </div>
        <div className="flex flex-wrap items-center gap-3 border-t border-line px-5 py-3">
          <Button variant="primary" onClick={() => void save()} disabled={saving || !dirty}>
            {saving ? "Saving…" : "Save branding"}
          </Button>
          {dirty ? <span className="text-[12px] text-faint">Unsaved changes</span> : null}
          {error ? (
            <span className="text-[12.5px] text-neg">
              {error instanceof Error ? error.message : String(error)}
            </span>
          ) : null}
        </div>
      </Card>

      <Card className="mt-5">
        <CardHeader
          title="Token Saver"
          action={
            <span className="text-[11.5px] text-faint">
              RTK filters · error results are never touched
            </span>
          }
        />
        <div className="flex flex-wrap items-center justify-between gap-4 px-5 pb-5">
          <p className="max-w-[62ch] text-[12.5px] leading-relaxed text-muted">
            Compresses noisy tool results (git diffs, grep matches, build logs) before they reach
            the provider: repetition is collapsed and long dumps keep their head and tail with a
            marker. A client can bypass it per request with{" "}
            <code className="font-mono text-[11px] text-fg">x-rrouter-rtk: off</code>.
          </p>
          <button
            type="button"
            role="switch"
            aria-checked={draft.rtk ?? true}
            aria-label="Token Saver"
            disabled={savingRtk}
            onClick={async () => {
              const next = !(draft.rtk ?? true);
              setSavingRtk(true);
              setError(null);
              try {
                const saved = await api.put<Brand>("/api/settings", { ...draft, rtk: next });
                setDraft(saved);
                setDirty(false);
                await client.invalidateQueries({ queryKey: ["gateway-settings"] });
              } catch (e) {
                setError(e);
              } finally {
                setSavingRtk(false);
              }
            }}
            className={cx(
              "relative h-6 w-11 shrink-0 rounded-full border transition-colors disabled:opacity-50",
              (draft.rtk ?? true)
                ? "border-accent bg-accent"
                : "border-line-strong bg-surface-3",
            )}
          >
            <span
              aria-hidden
              className={cx(
                "absolute top-1 h-4 w-4 rounded-full transition-[left] duration-150",
                (draft.rtk ?? true) ? "left-[22px] bg-white" : "left-[3px] bg-[var(--color-muted)]",
              )}
            />
          </button>
        </div>
      </Card>

      <Card className="mt-5">
        <CardHeader title="Runtime" action={<span className="text-[11.5px] text-faint">read-only</span>} />
        <dl className="grid gap-x-8 gap-y-0 px-5 pb-4 text-[13px] md:grid-cols-2">
          <Row k="Version" v={`v${version}`} />
          <Row k="Uptime" v={system.data ? uptime(system.data.uptime_secs) : "—"} />
          <Row k="API keys required" v={system.data?.require_api_key ? "yes" : "no"} />
          <Row k="Remote admin" v={system.data?.allow_remote_admin ? "allowed" : "loopback only"} />
          <Row k="Master key" v={system.data?.master_key_source ?? "—"} />
          <Row k="Static assets" v={system.data?.static_assets ? "served from disk" : "embedded"} />
          <Row k="Connections" v={counts ? `${counts.active_connections} active of ${counts.connections}` : "—"} />
          <Row k="API keys" v={counts ? String(counts.api_keys) : "—"} />
          <Row k="Public models" v={counts ? String(counts.disabled_models) + " disabled" : "—"} />
          <Row k="Aliases / combos" v={counts ? `${counts.aliases} / ${counts.combos}` : "—"} />
        </dl>
        <p className="border-t border-line px-5 py-3 text-[12px] text-faint">
          Bind address, timeouts and other process settings are environment variables; see
          docs/DEPLOY.md. This page never shows credential material.
        </p>
      </Card>
    </>
  );
}

function Row({ k, v }: { k: string; v: string }) {
  return (
    <div className="flex items-center justify-between gap-4 border-b border-[var(--color-rule)] py-2.5 last:border-0">
      <dt className="text-faint">{k}</dt>
      <dd className="min-w-0 truncate text-right font-mono text-[12.5px] text-muted tnum">{v}</dd>
    </div>
  );
}
