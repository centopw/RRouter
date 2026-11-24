/**
 * "Requests that never reached a provider": requests RRouter rejected before trying any
 * account (unknown model, rejected key, no healthy account, …). They are not a provider, so
 * they get their own calm section under the Limits Board: one total, then one row per
 * reason with what it means and where to fix it.
 */

import { useState } from "react";
import { Link } from "react-router-dom";
import { CaretRight } from "@phosphor-icons/react";
import { Card, Dot, cx } from "../../components/ui";
import { ago, compact } from "../../lib/format";
import type { UnroutedReasonView, UnroutedSummary } from "./unrouted-model";

const MAX_MODELS = 6;

function ReasonRow({ reason }: { reason: UnroutedReasonView }) {
  const share = Math.round(reason.share);
  return (
    <li className="grid grid-cols-[minmax(0,1fr)_7rem] items-start gap-x-4 py-2.5">
      <div className="flex min-w-0 flex-col gap-0.5">
        <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
          <span className="inline-flex items-center gap-1.5 text-small font-medium text-fg">
            <Dot tone="warn" />
            {reason.info.label}
          </span>
          {reason.info.fix ? (
            <Link
              to={reason.info.fix.href}
              className="text-caption text-accent-text underline-offset-2 hover:underline focus-visible:underline"
            >
              {reason.info.fix.label}
            </Link>
          ) : null}
        </div>
        <p className="text-caption text-muted">
          {reason.info.explanation}
          {reason.sampleModel ? (
            <>
              {" "}
              Latest: <span className="font-mono text-ink-soft">{reason.sampleModel}</span>
              {reason.lastSeenAt !== null ? (
                <span className="text-faint"> · {ago(reason.lastSeenAt)}</span>
              ) : null}
            </>
          ) : reason.lastSeenAt !== null ? (
            <span className="text-faint"> · latest {ago(reason.lastSeenAt)}</span>
          ) : null}
        </p>
        {reason.sampleError ? (
          <p className="line-clamp-1 break-all font-mono text-caption text-faint">
            {reason.sampleError}
          </p>
        ) : null}
      </div>
      <div className="flex flex-col items-end gap-1 pt-0.5">
        <span className="text-small font-medium text-fg tnum">
          {compact(reason.requests)}
          <span className="font-normal text-faint"> · {share}%</span>
        </span>
        <span
          role="img"
          aria-label={`${share}% of rejected requests`}
          className="block h-1 w-full overflow-hidden rounded-full bg-rule"
        >
          <span
            className="block h-full rounded-full bg-muted"
            style={{ width: `${Math.min(100, Math.max(0, reason.share))}%` }}
          />
        </span>
      </div>
    </li>
  );
}

export function UnroutedSection({
  summary,
  rangeLabel,
}: {
  summary: UnroutedSummary;
  rangeLabel: string;
}) {
  if (summary.requests === 0) return null;
  const { requests, lastSeenAt, reasons, models, reasonSource, sampled } = summary;

  return (
    <section id="usage-unrouted" aria-labelledby="usage-unrouted-heading" className="mb-5 scroll-mt-4">
      <div className="mb-2.5 flex flex-wrap items-baseline justify-between gap-2">
        <h2 id="usage-unrouted-heading" className="text-small font-semibold text-fg">
          Requests that never reached a provider
        </h2>
        <span className="text-caption text-faint">
          Rejected by RRouter before any account was tried · last {rangeLabel}
        </span>
      </div>

      <Card className="px-4 py-3">
        <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1">
          <p className="text-small">
            <span className="font-semibold text-fg tnum">{compact(requests)}</span>{" "}
            <span className="text-muted">{requests === 1 ? "request" : "requests"} rejected</span>
            {lastSeenAt !== null ? (
              <span className="text-faint"> · latest {ago(lastSeenAt)}</span>
            ) : null}
          </p>
          <Link
            to="/activity"
            className="text-caption text-accent-text underline-offset-2 hover:underline focus-visible:underline"
          >
            Open Activity
          </Link>
        </div>

        {reasons.length > 0 ? (
          <ul role="list" className="mt-2 flex flex-col divide-y divide-rule border-t border-rule">
            {reasons.map((reason) => (
              <ReasonRow key={reason.id} reason={reason} />
            ))}
          </ul>
        ) : (
          <p className="mt-2 text-caption text-muted">
            This gateway version does not record why. The usual causes are a model name nothing
            serves, a missing or rejected API key, or no healthy account for the model.
          </p>
        )}

        {models.length > 0 &&
        !(
          reasons.length === 1 &&
          models.length === 1 &&
          reasons[0]!.sampleModel === models[0]!.model
        ) ? (
          <div className="mt-2 flex flex-wrap items-baseline gap-x-3 gap-y-1 border-t border-rule pt-2 text-caption">
            <span className="text-faint">Models asked for</span>
            {models.slice(0, MAX_MODELS).map((model) => (
              <span key={model.model} className="inline-flex items-baseline gap-1">
                <span className="font-mono text-ink-soft">{model.model}</span>
                <span className="text-faint tnum">{compact(model.requests)}</span>
              </span>
            ))}
            {models.length > MAX_MODELS ? (
              <span className="text-faint">+{models.length - MAX_MODELS} more</span>
            ) : null}
          </div>
        ) : null}

        {reasonSource === "recent" ? (
          <p className="mt-2 text-caption text-faint">
            Reasons read from the latest {compact(sampled)} of these requests.
          </p>
        ) : null}
      </Card>
    </section>
  );
}

/**
 * Secondary collapsible disclosure for pre-dispatch failures so they stay visible and
 * explainable without intruding on the tracked-quota Providers table.
 */
export function UnroutedDisclosure({
  summary,
  rangeLabel,
}: {
  summary: UnroutedSummary;
  rangeLabel: string;
}) {
  const [open, setOpen] = useState(false);
  if (summary.requests === 0) return null;
  const { requests, lastSeenAt, reasons, models, reasonSource, sampled } = summary;
  const topReason = reasons[0];

  return (
    <section id="usage-unrouted" aria-label="Requests that never reached a provider" className="scroll-mt-4">
      <Card as="div" className="overflow-hidden">
        <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((prev) => !prev)}
        className="flex w-full flex-wrap items-center justify-between gap-2 px-4 py-2.5 text-left transition-colors hover:bg-[var(--color-row-hover)] focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
      >
        <span className="flex min-w-0 items-center gap-2 text-small">
          <CaretRight
            size={12}
            aria-hidden
            className={cx("shrink-0 text-faint transition-transform", open && "rotate-90")}
          />
          <Dot tone="warn" />
          <span className="font-medium text-fg">Requests that never reached a provider</span>
          <span className="text-caption text-muted tnum">
            · {compact(requests)} rejected
            {topReason ? ` (${topReason.info.label})` : ""}
          </span>
        </span>
        <span className="text-caption text-faint tnum">
          {lastSeenAt !== null ? `latest ${ago(lastSeenAt)} · ` : ""}
          last {rangeLabel}
        </span>
      </button>

      {open ? (
        <div className="border-t border-rule px-4 py-3">
          <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1">
            <p className="text-caption text-muted">
              Rejected by RRouter before any provider account was tried.
            </p>
            <Link
              to="/activity"
              className="text-caption text-accent-text underline-offset-2 hover:underline focus-visible:underline"
            >
              Open Activity
            </Link>
          </div>

          {reasons.length > 0 ? (
            <ul role="list" className="mt-2 flex flex-col divide-y divide-rule border-t border-rule">
              {reasons.map((reason) => (
                <ReasonRow key={reason.id} reason={reason} />
              ))}
            </ul>
          ) : (
            <p className="mt-2 text-caption text-muted">
              This gateway version does not record why. The usual causes are a model name nothing
              serves, a missing or rejected API key, or no healthy account for the model.
            </p>
          )}

          {models.length > 0 &&
          !(
            reasons.length === 1 &&
            models.length === 1 &&
            reasons[0]!.sampleModel === models[0]!.model
          ) ? (
            <div className="mt-2 flex flex-wrap items-baseline gap-x-3 gap-y-1 border-t border-rule pt-2 text-caption">
              <span className="text-faint">Models asked for</span>
              {models.slice(0, MAX_MODELS).map((model) => (
                <span key={model.model} className="inline-flex items-baseline gap-1">
                  <span className="font-mono text-ink-soft">{model.model}</span>
                  <span className="text-faint tnum">{compact(model.requests)}</span>
                </span>
              ))}
              {models.length > MAX_MODELS ? (
                <span className="text-faint">+{models.length - MAX_MODELS} more</span>
              ) : null}
            </div>
          ) : null}

          {reasonSource === "recent" ? (
            <p className="mt-2 text-caption text-faint">
              Reasons read from the latest {compact(sampled)} of these requests.
            </p>
          ) : null}
        </div>
      ) : null}
      </Card>
    </section>
  );
}
