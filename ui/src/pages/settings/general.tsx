import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  ErrorNote,
  Field,
  Input,
  PageHeader,
} from "../../components/ui";
import { GATEWAY_SETTINGS_KEY, useGatewaySettings, type Brand } from "./gateway-settings";

const BRAND_KEYS = ["brand_name", "brand_color", "logo_url", "footer_text"] as const;

/** Deployment identity: the name, mark and accent colour this gateway is presented with. */
export default function GeneralSettings() {
  const client = useQueryClient();
  const settings = useGatewaySettings();

  const [draft, setDraft] = useState<Brand>({});
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
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
      await client.invalidateQueries({ queryKey: GATEWAY_SETTINGS_KEY });
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
  const previewInk: Rgb = isDarkTheme ? [13, 16, 23] : [255, 255, 255];
  const previewInkHex = toHex6(previewInk);
  const contrastWhite = parsedRgb ? contrastRatio(parsedRgb, [255, 255, 255]) : null;
  const contrastInk = parsedRgb ? contrastRatio(parsedRgb, [13, 16, 23]) : null;
  const activeContrast = parsedRgb ? contrastRatio(parsedRgb, previewInk) : null;
  const lowContrast = activeContrast !== null && activeContrast < 4.5;

  return (
    <>
      <PageHeader
        title="General"
        subtitle="How this deployment is named and marked, for the dashboard only."
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
                      color: previewInkHex,
                    }}
                  >
                    Preview
                  </span>
                  <span className="font-mono text-muted tnum">
                    vs white: {contrastWhite.toFixed(2)}:1
                  </span>
                  <span className="text-faint">·</span>
                  <span className="font-mono text-muted tnum">
                    vs ink: {contrastInk.toFixed(2)}:1
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
                    className="text-caption text-[var(--color-warn-ink)] tnum"
                  >
                    WCAG AA warning: contrast is {activeContrast.toFixed(2)}:1 against{" "}
                    {isDarkTheme ? "dark ink" : "white"} ({previewInkHex}) — aim for at least
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

          <Field label="Footer note" hint="Shown as one line at the bottom of the sidebar.">
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
    </>
  );
}

type Rgb = [number, number, number];

function parseHexColor(input: string): Rgb | null {
  const hex = input.replace(/^#/, "");
  const full =
    hex.length === 3 || hex.length === 4
      ? hex
          .slice(0, 3)
          .split("")
          .map((c) => c + c)
          .join("")
      : hex.slice(0, 6);
  if (!/^[0-9a-f]{6}$/i.test(full)) return null;
  return [
    parseInt(full.slice(0, 2), 16),
    parseInt(full.slice(2, 4), 16),
    parseInt(full.slice(4, 6), 16),
  ];
}

function toHex6([r, g, b]: Rgb): string {
  return `#${[r, g, b].map((c) => c.toString(16).padStart(2, "0")).join("")}`;
}

function channelLuminance(c8: number): number {
  const c = c8 / 255;
  return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}

function relativeLuminance([r, g, b]: Rgb): number {
  return (
    0.2126 * channelLuminance(r) + 0.7152 * channelLuminance(g) + 0.0722 * channelLuminance(b)
  );
}

function contrastRatio(a: Rgb, b: Rgb): number {
  const la = relativeLuminance(a);
  const lb = relativeLuminance(b);
  const [light, dark] = la >= lb ? [la, lb] : [lb, la];
  return (light + 0.05) / (dark + 0.05);
}
