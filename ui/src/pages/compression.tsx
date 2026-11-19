import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { api } from "../lib/api";
import { compact, pct } from "../lib/format";
import { useCompressionStats } from "../lib/queries";
import {
  AlertBanner,
  Card,
  CardHeader,
  ErrorNote,
  PageHeader,
  Segmented,
  Select,
  Toggle,
} from "../components/ui";
import {
  GATEWAY_SETTINGS_KEY,
  useGatewaySettings,
  type Brand,
  type CompressionLevel,
  type CompressionSettings,
} from "./settings/gateway-settings";

/** Windows the effect readout offers; the id is both the control value and the label. */
const EFFECT_RANGES = [
  { id: "24", label: "24h", hours: 24 },
  { id: "168", label: "7d", hours: 168 },
  { id: "720", label: "30d", hours: 720 },
] as const;
type EffectRange = (typeof EFFECT_RANGES)[number]["id"];

const LEVELS: ReadonlyArray<{ id: CompressionLevel; label: string; hint: string }> = [
  { id: "lite", label: "Lite", hint: "grammar intact, filler removed" },
  { id: "full", label: "Full", hint: "fragments, articles dropped" },
  { id: "ultra", label: "Ultra", hint: "telegraphic" },
];

/** How the gateway trims what reaches and leaves the model; each method saves on change. */
export default function Compression() {
  const settings = useGatewaySettings();
  const client = useQueryClient();
  const [saving, setSaving] = useState<null | "rtk" | "caveman" | "level">(null);
  const [range, setRange] = useState<EffectRange>("168");
  const [error, setError] = useState<unknown>(null);

  const stored = settings.data?.compression;
  const rtk = stored?.rtk ?? true;
  const caveman = stored?.caveman ?? false;
  const level: CompressionLevel = stored?.caveman_level ?? "full";
  // One save at a time: the controls all write the same settings object.
  const busy = saving !== null || settings.isPending;

  // The effect readout is its own query: changing the window never refetches the stored
  // switch state, and the card reads the same endpoint the Activity line does.
  const effectWindow = EFFECT_RANGES.find((option) => option.id === range) ?? EFFECT_RANGES[1];
  const stats = useCompressionStats(effectWindow.hours);
  const sum = stats.data;
  // Percent removed is bytes-in-relative, so it is only defined once RTK removed bytes.
  const savedPercent =
    sum && sum.rtk_before > 0 ? pct((sum.rtk_saved * 100) / sum.rtk_before) : "—";

  /** PUT a partial object; the server deep-merges it into the stored settings. */
  const save = async (
    control: "rtk" | "caveman" | "level",
    patch: CompressionSettings,
  ) => {
    setSaving(control);
    setError(null);
    try {
      const saved = await api.put<Brand>("/api/settings", { compression: patch });
      client.setQueryData<Brand>(GATEWAY_SETTINGS_KEY, (old) => ({
        ...(old ?? {}),
        ...(saved ?? {}),
        compression: { ...(old?.compression ?? {}), ...(saved?.compression ?? patch) },
      }));
      await client.invalidateQueries({ queryKey: GATEWAY_SETTINGS_KEY });
    } catch (e) {
      // The controls read the stored value, so a failed save keeps the previous one.
      setError(e);
    } finally {
      setSaving(null);
    }
  };

  return (
    <>
      <PageHeader
        title="Compression"
        subtitle="The gateway trims what reaches and leaves the model. Each method runs per request, and its effect shows on that request's log."
      />

      {settings.isError ? (
        <div className="mb-4">
          <ErrorNote error={settings.error} onRetry={() => void settings.refetch()} />
        </div>
      ) : null}
      {error ? (
        <AlertBanner
          tone="neg"
          title="Could not save compression settings"
          onDismiss={() => setError(null)}
        >
          {error instanceof Error ? error.message : String(error)}
        </AlertBanner>
      ) : null}

      <div
        className="mb-4 flex flex-wrap items-center gap-2 rounded-[var(--radius-card)] border border-line bg-surface-2 px-4 py-2.5 font-mono text-caption text-muted"
        aria-label="Request pipeline order"
      >
        <span>guardrails</span>
        <span aria-hidden className="text-faint">
          →
        </span>
        <span className={rtk ? "text-fg" : undefined}>RTK</span>
        <span aria-hidden className="text-faint">
          →
        </span>
        <span className={caveman ? "text-fg" : undefined}>Caveman</span>
        <span aria-hidden className="text-faint">
          →
        </span>
        <span>provider</span>
      </div>

      <Card className="mb-4">
        <CardHeader
          title="Effect"
          action={
            <Segmented
              options={EFFECT_RANGES.map((option) => ({ id: option.id, label: option.label }))}
              value={range}
              onChange={(id) => setRange(id)}
              ariaLabel="effect time range"
            />
          }
        />
        <div className="px-5 pb-5">
          {/* The row reserves its height so switching windows never moves the cards below. */}
          <dl className="grid min-h-[2.75rem] grid-cols-2 gap-x-6 gap-y-3 sm:grid-cols-3 lg:grid-cols-5">
            <div>
              <dt className="text-caption text-faint">Requests compressed</dt>
              <dd className="num mt-0.5 text-small text-fg">{compact(sum?.requests)}</dd>
            </div>
            <div>
              <dt className="text-caption text-faint">Tool-result bytes</dt>
              <dd className="num mt-0.5 text-small text-fg">
                {sum ? `${compact(sum.rtk_before)} → ${compact(sum.rtk_after)}` : "—"}
              </dd>
            </div>
            <div>
              <dt className="text-caption text-faint">Removed</dt>
              <dd className="num mt-0.5 text-small text-fg">{savedPercent}</dd>
            </div>
            <div>
              <dt className="text-caption text-faint">Est. tokens saved</dt>
              <dd className="num mt-0.5 text-small text-fg">
                {compact(sum?.rtk_saved_tokens_estimate)}
              </dd>
            </div>
            <div>
              <dt className="text-caption text-faint">Caveman requests</dt>
              <dd className="num mt-0.5 text-small text-fg">{compact(sum?.caveman_requests)}</dd>
            </div>
          </dl>
          <p className="mt-3 text-caption text-faint">
            {stats.isError
              ? "The effect readout could not be loaded; the switches below are unaffected."
              : "Bytes and tokens here count tool-result text only, never the whole prompt. Tokens are estimated at ~4 bytes per token, not a tokenizer run. Caveman adds a small instruction — its saving is on the reply, so it is counted here, not measured."}
          </p>
        </div>
      </Card>

      <Card>
        <CardHeader
          title="RTK"
          action={<span className="text-caption text-faint">input · tool results</span>}
        />
        <div className="flex flex-wrap items-start justify-between gap-4 px-5 pb-5">
          <div className="max-w-[62ch] space-y-3">
            <p className="text-small leading-relaxed text-muted">
              Compresses noisy tool results — git diffs, grep and find matches, build and test
              logs — before they reach the provider. Structure is kept and repetition dropped;
              error results are never touched.
            </p>
            <p className="text-small leading-relaxed text-muted">
              A client can bypass it for one request with{" "}
              <code className="rounded-md border border-line bg-surface-2 px-1.5 py-0.5 font-mono text-caption text-fg">
                x-rrouter-rtk: off
              </code>
              .
            </p>
          </div>
          <Toggle
            checked={rtk}
            label="RTK compression"
            disabled={busy}
            onChange={(next) => void save("rtk", { rtk: next })}
          />
        </div>
      </Card>

      <Card className="mt-4">
        <CardHeader
          title="Caveman"
          action={<span className="text-caption text-faint">output · model replies</span>}
        />
        <div className="flex flex-col gap-4 px-5 pb-5">
          <div className="flex flex-wrap items-start justify-between gap-4">
            <p className="max-w-[62ch] text-small leading-relaxed text-muted">
              Adds a terse-output style instruction to the system prompt, so replies come back
              shorter. It never edits code, paths, commands or error text — only the prose around
              them.
            </p>
            <Toggle
              checked={caveman}
              label="Caveman compression"
              disabled={busy}
              onChange={(next) => void save("caveman", { caveman: next })}
            />
          </div>

          <div className="grid gap-4 border-t border-line pt-4 md:grid-cols-[12rem_1fr] md:items-start">
            <div>
              <p className="text-small font-medium text-fg">Level</p>
              <p className="mt-0.5 text-caption text-faint">
                {caveman ? "applies to the next reply" : "turn Caveman on to choose"}
              </p>
            </div>
            <div className="flex flex-col gap-3">
              <Select
                ariaLabel="Caveman level"
                value={level}
                onChange={(next) => void save("level", { caveman_level: next as CompressionLevel })}
                disabled={!caveman || busy}
                className="w-[12rem]"
              >
                {LEVELS.map((option) => (
                  <option key={option.id} value={option.id}>
                    {option.label}
                  </option>
                ))}
              </Select>
              <dl className="grid gap-1 text-caption">
                {LEVELS.map((option) => (
                  <div key={option.id} className="flex gap-2">
                    <dt
                      className={
                        option.id === level ? "w-12 shrink-0 text-fg" : "w-12 shrink-0 text-faint"
                      }
                    >
                      {option.label}
                    </dt>
                    <dd className={option.id === level ? "text-muted" : "text-faint"}>
                      {option.hint}
                    </dd>
                  </div>
                ))}
              </dl>
            </div>
          </div>
        </div>
      </Card>
    </>
  );
}
