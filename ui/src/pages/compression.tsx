import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { api } from "../lib/api";
import {
  AlertBanner,
  Card,
  CardHeader,
  ErrorNote,
  PageHeader,
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
  const [error, setError] = useState<unknown>(null);

  const stored = settings.data?.compression;
  const rtk = stored?.rtk ?? true;
  const caveman = stored?.caveman ?? false;
  const level: CompressionLevel = stored?.caveman_level ?? "full";
  // One save at a time: the controls all write the same settings object.
  const busy = saving !== null || settings.isPending;

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
