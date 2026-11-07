import { useCallback, useEffect, useState } from "react";
import { Badge, Button, Card, CardHeader, ConfirmDialog, ErrorNote } from "./ui";
import { useToast } from "./toast";
import { api } from "../lib/api";
import { when } from "../lib/format";

type Deployment = {
  kind: string;
  label: string;
  automatic: boolean;
  can_download: boolean;
  command: string | null;
  detail: string;
};

type UpdateStatus = {
  current_version: string;
  latest_version: string | null;
  release_url: string;
  notes: string | null;
  published_at: string | null;
  checked_at: number | null;
  phase: string;
  error: string | null;
  downloaded: boolean;
  deployment: Deployment;
  options: { auto_check: boolean; auto_download: boolean };
};

type ActionResult = { accepted?: boolean; restarting?: boolean; message?: string; warning?: string };

/**
 * Update controls. Checking and downloading are harmless; "restart to update" is only
 * offered where this process can actually replace itself. Container and Helm installs get
 * the supervisor command instead of a button that cannot work.
 */
export function UpdateCard() {
  const toast = useToast();
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);
  const [confirming, setConfirming] = useState(false);

  const load = useCallback(async () => {
    const next = await api.get<UpdateStatus>("/api/update");
    setStatus(next);
  }, []);

  useEffect(() => {
    void load().catch(setFailure);
    const timer = window.setInterval(() => void load().catch(() => {}), 60_000);
    return () => window.clearInterval(timer);
  }, [load]);

  const run = async (path: string, label: string) => {
    setBusy(path);
    setFailure(null);
    try {
      const result = await api.post<ActionResult>(path);
      if (result?.message) toast.success(label, result.message);
      await load();
    } catch (e) {
      setFailure(e);
    } finally {
      setBusy(null);
    }
  };

  const saveOptions = async (patch: { auto_check?: boolean; auto_download?: boolean }) => {
    setFailure(null);
    try {
      const next = await api.put<UpdateStatus>("/api/update/settings", patch);
      setStatus(next);
    } catch (e) {
      setFailure(e);
    }
  };

  const copy = async (command: string) => {
    try {
      await navigator.clipboard.writeText(command);
      toast.success("Copied", command);
    } catch (e) {
      toast.error("Could not copy", e instanceof Error ? e.message : String(e));
    }
  };

  if (!status) {
    return (
      <Card className="mt-5">
        <CardHeader title="Software updates" />
        <div className="px-5 pb-5 text-[12.5px] text-faint">{failure ? <ErrorNote error={failure} /> : "loading…"}</div>
      </Card>
    );
  }

  const newer = status.latest_version !== null && status.latest_version !== status.current_version;
  const working = busy !== null || status.phase === "checking" || status.phase === "downloading";
  const ready =
    newer && status.deployment.automatic && (status.downloaded || status.deployment.kind === "brew");

  return (
    <>
      <ConfirmDialog
        open={confirming}
        title="Restart with the new version?"
        message={
          <>
            {status.deployment.kind === "brew"
              ? "Homebrew upgrades the formula. Restart the RRouter service afterwards to run the new binary; configuration and credentials stay in the data directory."
              : "RRouter will replace its binary and restart. In-flight streams are interrupted; configuration and credentials stay in the data directory."}
          </>
        }
        confirmLabel={status.deployment.kind === "brew" ? "Run brew upgrade" : "Restart and update"}
        busy={busy === "/api/update/apply"}
        onConfirm={() => {
          setConfirming(false);
          void run("/api/update/apply", "Update started");
        }}
        onClose={() => setConfirming(false)}
      />

      <Card className="mt-5">
        <CardHeader
          title="Software updates"
          action={<Badge tone="neutral">v{status.current_version}</Badge>}
        />
        <div className="flex flex-col gap-4 px-5 pb-5">
          <div className="flex flex-wrap items-center gap-x-6 gap-y-2 text-[12.5px]">
            <span className="text-muted">
              latest <span className="font-mono text-fg">{status.latest_version ? `v${status.latest_version}` : "—"}</span>
            </span>
            <span className="text-muted">
              checked <span className="text-fg">{status.checked_at ? when(status.checked_at) : "never"}</span>
            </span>
            {newer ? (
              <Badge tone="accent">update available</Badge>
            ) : status.latest_version ? (
              <Badge tone="pos">up to date</Badge>
            ) : null}
            {status.downloaded ? <Badge tone="pos">downloaded</Badge> : null}
          </div>

          <div className="rounded-lg border border-line bg-surface-2 px-3 py-2.5">
            <div className="flex flex-wrap items-baseline gap-2">
              <span className="text-[12.5px] font-medium">Deployment</span>
              <Badge tone="neutral">{status.deployment.label}</Badge>
              <span className="text-[11.5px] text-faint">
                {status.deployment.automatic ? "this process can finish the upgrade" : "supervisor owns the upgrade"}
              </span>
            </div>
            <p className="mt-1 text-[11.5px] leading-relaxed text-muted">{status.deployment.detail}</p>
            {status.deployment.command ? (
              <div className="mt-2 flex items-center gap-2">
                <code className="min-w-0 flex-1 truncate rounded border border-line bg-surface px-2 py-1 font-mono text-[11.5px] text-fg">
                  {status.deployment.command}
                </code>
                <Button variant="quiet" onClick={() => void copy(status.deployment.command ?? "")}>
                  copy
                </Button>
              </div>
            ) : null}
          </div>

          <div className="grid gap-3 min-[700px]:grid-cols-2">
            <label className="flex items-start gap-3 rounded-lg border border-line px-3 py-2.5">
              <input
                type="checkbox"
                checked={status.options.auto_check}
                onChange={(e) => void saveOptions({ auto_check: e.target.checked })}
                className="mt-0.5 h-4 w-4 accent-[var(--color-accent)]"
              />
              <span>
                <span className="block text-[12.5px] font-medium">Check daily</span>
                <span className="block text-[11.5px] text-faint">Ask GitHub once a day for a newer release.</span>
              </span>
            </label>
            <label className="flex items-start gap-3 rounded-lg border border-line px-3 py-2.5">
              <input
                type="checkbox"
                checked={status.options.auto_download}
                disabled={!status.deployment.can_download}
                onChange={(e) => void saveOptions({ auto_download: e.target.checked })}
                className="mt-0.5 h-4 w-4 accent-[var(--color-accent)]"
              />
              <span>
                <span className="block text-[12.5px] font-medium">Download automatically</span>
                <span className="block text-[11.5px] text-faint">
                  {status.deployment.can_download
                    ? "Verify and stage the archive; nothing restarts by itself."
                    : `${status.deployment.label} replaces the whole install, so there is no binary to stage.`}
                </span>
              </span>
            </label>
          </div>

          {status.notes ? (
            <details className="rounded-lg border border-line px-3 py-2 text-[12px]">
              <summary className="cursor-pointer text-muted">Release notes</summary>
              <pre className="mt-2 max-h-56 overflow-auto whitespace-pre-wrap font-sans text-[11.5px] leading-relaxed text-muted">
                {status.notes}
              </pre>
            </details>
          ) : null}

          {status.error ? <ErrorNote error={status.error} /> : null}
          {failure ? <ErrorNote error={failure} /> : null}

          <div className="flex flex-wrap items-center gap-2">
            <Button variant="quiet" onClick={() => void run("/api/update/check", "Update check finished")} disabled={working}>
              {working && status.phase === "checking" ? "checking…" : "check now"}
            </Button>
            {newer && !status.downloaded ? (
              <Button
                variant="primary"
                onClick={() => void run("/api/update/download", "Update downloaded")}
                disabled={working || !status.deployment.can_download}
              >
                {working && status.phase === "downloading" ? "downloading…" : "download"}
              </Button>
            ) : null}
            {ready ? (
              <Button variant="primary" onClick={() => setConfirming(true)}>
                {status.deployment.kind === "brew" ? "run brew upgrade" : "restart to update"}
              </Button>
            ) : null}
            <a
              className="ml-auto text-[12px] text-[var(--color-accent-text)] hover:underline"
              href={status.release_url}
              target="_blank"
              rel="noreferrer"
            >
              release page →
            </a>
          </div>
          <p className="text-[11px] leading-relaxed text-faint">
            Downloads are verified against the release SHA-256. The staged binary lives under
            the data directory; restarting is the only step that changes the running process.
          </p>
        </div>
      </Card>
    </>
  );
}
