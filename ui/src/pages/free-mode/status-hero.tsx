import { ArrowsClockwise } from "@phosphor-icons/react";
import { Button, Card, CopyIconButton, Dot, Toggle, cx } from "../../components/ui";
import { ago, when } from "../../lib/format";
import { statusHeadline, type FreeModeState } from "./model";

export function StatusHero({
  state,
  togglingPool,
  probingPool,
  refreshing,
  onTogglePool,
  onRefresh,
}: {
  state: FreeModeState;
  togglingPool: boolean;
  probingPool: boolean;
  refreshing: boolean;
  onTogglePool: (enabled: boolean) => void;
  onRefresh: () => void;
}) {
  const isProbing = probingPool || (refreshing && state.enabled);
  const headline = statusHeadline(state, isProbing);
  const failedCatalogs = state.catalogs.filter((catalog) => catalog.error !== null);
  const checkedAt =
    state.last_checked_at !== null && state.last_checked_at > 0 ? state.last_checked_at : null;

  return (
    <Card className="p-5">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
        <div className="flex items-start gap-3.5 min-w-0">
          <div className="flex items-center gap-2 pt-0.5 shrink-0">
            <Toggle
              checked={state.enabled}
              onChange={onTogglePool}
              label="Enable free models"
              disabled={togglingPool}
              size="md"
            />
            <span
              className={cx(
                "w-6 text-caption font-semibold tnum",
                state.enabled ? "text-fg" : "text-faint",
              )}
            >
              {state.enabled ? "ON" : "OFF"}
            </span>
          </div>

          <div className="min-w-0 flex-1">
            <div className="flex items-start gap-2">
              <Dot tone={headline.tone} className="mt-1.5 shrink-0" />
              <p role="status" className="text-body font-semibold text-fg tnum">
                {headline.title}
              </p>
            </div>
            <p className="mt-1 text-small leading-relaxed text-muted">{headline.detail}</p>

            <div className="mt-3 flex flex-wrap items-center gap-x-3 gap-y-2">
              <div className="inline-flex items-center gap-1.5">
                <span className="text-caption text-faint">Model name</span>
                <code className="rounded-[var(--radius-control)] border border-line bg-surface-2 px-2 py-0.5 font-mono text-small text-fg select-all">
                  {state.ref}
                </code>
                <CopyIconButton value={state.ref} label={`Copy ${state.ref} model name`} />
              </div>

              {checkedAt !== null ? (
                <span className="text-caption text-faint" title={when(checkedAt)}>
                  Last check {ago(checkedAt)}
                </span>
              ) : null}
            </div>
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-2 sm:pl-2">
          <Button
            variant="secondary"
            size="sm"
            loading={refreshing}
            disabled={togglingPool || refreshing}
            onClick={onRefresh}
          >
            {!refreshing ? <ArrowsClockwise size={14} aria-hidden /> : null}
            {refreshing ? (state.enabled ? "Checking…" : "Refreshing…") : "Refresh"}
          </Button>
        </div>
      </div>

      {failedCatalogs.length > 0 ? (
        <div
          role="alert"
          className="mt-4 rounded-[var(--radius-surface)] border border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] px-3 py-2 text-caption text-[var(--color-warn-ink)]"
        >
          {failedCatalogs.map((catalog) => (
            <p key={catalog.provider} className="leading-relaxed">
              Could not refresh the <span className="font-mono">{catalog.provider}</span> catalog:{" "}
              {catalog.error}
            </p>
          ))}
        </div>
      ) : null}
    </Card>
  );
}
