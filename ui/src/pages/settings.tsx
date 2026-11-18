import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { api, type FetchResult, type SyncResult } from "../lib/api";
import { qk, useConnections, useSystem } from "../lib/queries";
import { Badge, Button, Card, CardHeader, ErrorNote, Field, Input, PageHeader, Select } from "../components/ui";
import { UpdateCard } from "../components/update-card";
import { Toggle } from "../components/atoms";
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
  const connections = useConnections();
  const [connectionId, setConnectionId] = useState("");
  const [metadataBusy, setMetadataBusy] = useState<null | "sync" | "fetch">(null);
  const [metadataNote, setMetadataNote] = useState<string | null>(null);
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

  const [isDarkTheme, setIsDarkTheme] = useState(
    () =>
      typeof document !== "undefined" &&
      (document.documentElement.dataset.theme === "dark" ||
        document.documentElement.classList.contains("dark")),
  );

  useEffect(() => {
    const root = document.documentElement;
    const sync = () =>
      setIsDarkTheme(root.dataset.theme === "dark" || root.classList.contains("dark"));
    sync();
    const observer = new MutationObserver(sync);
    observer.observe(root, { attributes: true, attributeFilter: ["data-theme", "class"] });
    return () => observer.disconnect();
  }, []);

  const color = draft.brand_color?.trim() ?? "";
  const colorOk = /^#[0-9a-f]{3,8}$/i.test(color);
  const parsedRgb = colorOk ? parseHexColor(color) : null;
  const pickerHex = parsedRgb ? toHex6(parsedRgb) : "#1c2b42";
  const contrastWhite = parsedRgb ? contrastRatio(parsedRgb, [255, 255, 255]) : null;
  const contrastInk = parsedRgb ? contrastRatio(parsedRgb, [13, 16, 23]) : null;
  const activeContrast = isDarkTheme ? contrastInk : contrastWhite;
  const lowContrast = activeContrast !== null && activeContrast < 4.5;
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
            className="inline-flex h-8 items-center rounded-full border border-line bg-surface px-3 text-body text-fg transition-colors hover:border-line-strong hover:bg-surface-3"
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
          action={<span className="text-caption text-faint">applies to the dashboard</span>}
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
                value={pickerHex}
                onChange={(e) => set("brand_color")(e.target.value)}
                className="h-8 w-10 shrink-0 cursor-pointer rounded-[var(--radius-input)] border border-line bg-surface-2"
              />
              <Input
                value={draft.brand_color ?? ""}
                onChange={set("brand_color")}
                placeholder="#1c2b42"
                ariaLabel="accent colour hex"
              />
            </div>
            {color !== "" && !parsedRgb ? (
              <p role="alert" className="mt-1.5 text-caption text-neg">
                Enter a valid CSS hex colour (e.g. #1c2b42).
              </p>
            ) : null}
            {parsedRgb && contrastWhite !== null && contrastInk !== null ? (
              <div className="mt-2 space-y-1.5">
                <div className="flex flex-wrap items-center gap-2 text-caption">
                  <span
                    className="inline-flex h-6 items-center rounded-full px-2.5 text-caption font-semibold"
                    style={{
                      backgroundColor: pickerHex,
                      color: isDarkTheme ? "#0d1017" : "#ffffff",
                    }}
                  >
                    Preview
                  </span>
                  <span className="font-mono text-muted tnum">
                    vs #fff: {contrastWhite.toFixed(2)}:1
                  </span>
                  <span className="text-faint">·</span>
                  <span className="font-mono text-muted tnum">
                    vs #0d1017: {contrastInk.toFixed(2)}:1
                  </span>
                  <Badge tone={lowContrast ? "warn" : "pos"}>
                    <span className="tnum">
                      {lowContrast ? "Low contrast (< 4.5:1)" : "AA pass"}
                    </span>
                  </Badge>
                </div>
                {lowContrast && activeContrast !== null ? (
                  <p
                    role="status"
                    className="rounded-[var(--radius-input)] border border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] px-2.5 py-1.5 text-caption text-[var(--color-warn-ink)] tnum"
                  >
                    WCAG AA warning: contrast is {activeContrast.toFixed(2)}:1 against{" "}
                    {isDarkTheme ? "dark ink (#0d1017)" : "white (#ffffff)"} — aim for at least
                    4.5:1 so primary buttons and badges stay legible.
                  </p>
                ) : null}
              </div>
            ) : null}
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
          {dirty ? <span className="text-small text-faint">Unsaved changes</span> : null}
          {error ? (
            <span className="text-small text-neg">
              {error instanceof Error ? error.message : String(error)}
            </span>
          ) : null}
        </div>
      </Card>

      <Card className="mt-5">
        <CardHeader
          title="Model metadata"
          action={<span className="text-caption text-faint">registry-only · never changes routing</span>}
        />
        <div className="flex flex-col gap-4 px-5 pb-5">
          <div className="grid gap-4 min-[800px]:grid-cols-2">
            <div>
              <p className="text-small leading-relaxed text-muted">
                <span className="font-medium text-fg">sync metadata</span> downloads models.dev and
                updates context windows, prices and capabilities for models this gateway already
                lists. It never adds or removes a model, so it cannot change routing.
              </p>
              <Button
                className="mt-3"
                disabled={metadataBusy !== null}
                onClick={async () => {
                  setMetadataBusy("sync");
                  setMetadataNote(null);
                  try {
                    const result = await api.post<SyncResult>("/api/models/sync");
                    setMetadataNote(`synced ${result.models} models from ${result.providers} providers`);
                    await client.invalidateQueries({ queryKey: qk.models });
                  } catch (e) {
                    setError(e);
                  } finally {
                    setMetadataBusy(null);
                  }
                }}
              >
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
                  disabled={
                    metadataBusy !== null ||
                    (connections.data ?? []).length === 0
                  }
                  onClick={async () => {
                    const target = connectionId || connections.data?.[0]?.id;
                    if (!target) return;
                    setMetadataBusy("fetch");
                    setMetadataNote(null);
                    try {
                      const result = await api.post<FetchResult>("/api/models/fetch", {
                        connection_id: target,
                      });
                      setMetadataNote(
                        `${result.added} new of ${result.count} listed by ${result.provider}`,
                      );
                      await client.invalidateQueries({ queryKey: qk.models });
                    } catch (e) {
                      setError(e);
                    } finally {
                      setMetadataBusy(null);
                    }
                  }}
                >
                  {metadataBusy === "fetch" ? "fetching…" : "fetch"}
                </Button>
              </div>
            </div>
          </div>
          {metadataNote ? <p className="text-small text-[var(--color-pos)]">{metadataNote}</p> : null}
        </div>
      </Card>

      <Card className="mt-5">
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
            checked={draft.rtk ?? true}
            label="Token Saver"
            disabled={savingRtk}
            onChange={async () => {
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
          />
        </div>
      </Card>

      <UpdateCard />

      <Card className="mt-5">
        <CardHeader title="Runtime" action={<span className="text-caption text-faint">read-only</span>} />
        <dl className="grid gap-x-8 gap-y-0 px-5 pb-4 text-body md:grid-cols-2">
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
        <p className="border-t border-line px-5 py-3 text-small text-faint">
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
      <dd className="min-w-0 truncate text-right font-mono text-small text-muted tnum">{v}</dd>
    </div>
  );
}

type Rgb = [number, number, number];

function parseHexColor(input: string): Rgb | null {
  const raw = input.trim().replace(/^#/, "");
  if (!/^[0-9a-f]+$/i.test(raw)) return null;
  if (raw.length === 3 || raw.length === 4) {
    const r = Number.parseInt(raw[0]! + raw[0]!, 16);
    const g = Number.parseInt(raw[1]! + raw[1]!, 16);
    const b = Number.parseInt(raw[2]! + raw[2]!, 16);
    return [r, g, b];
  }
  if (raw.length === 6 || raw.length === 8) {
    const r = Number.parseInt(raw.slice(0, 2), 16);
    const g = Number.parseInt(raw.slice(2, 4), 16);
    const b = Number.parseInt(raw.slice(4, 6), 16);
    return [r, g, b];
  }
  return null;
}

function toHex6([r, g, b]: Rgb): string {
  const part = (n: number) => Math.max(0, Math.min(255, n)).toString(16).padStart(2, "0");
  return `#${part(r)}${part(g)}${part(b)}`;
}

function channelLuminance(c8: number): number {
  const srgb = c8 / 255;
  return srgb <= 0.03928 ? srgb / 12.92 : ((srgb + 0.055) / 1.055) ** 2.4;
}

function relativeLuminance([r, g, b]: Rgb): number {
  return (
    0.2126 * channelLuminance(r) +
    0.7152 * channelLuminance(g) +
    0.0722 * channelLuminance(b)
  );
}

function contrastRatio(a: Rgb, b: Rgb): number {
  const l1 = relativeLuminance(a);
  const l2 = relativeLuminance(b);
  return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
}
