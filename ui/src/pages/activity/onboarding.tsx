import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import { CopyButton } from "../../components/ui";
import { curlFor } from "./request-log";

/** Zero-traffic gateway: show how to send the first request instead of four dashes. */

export function Onboarding({ baseUrl }: { baseUrl: string }) {
  const curl = curlFor(baseUrl, "/v1/chat/completions", "claude/sonnet-4-5");
  return (
    <section className="rounded-[var(--radius-card)] border border-line bg-surface px-8 py-10">
      <div className="mx-auto max-w-[560px] text-center">
        <div className="mx-auto flex h-10 w-10 items-center justify-center rounded-full border border-line bg-surface-2">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="var(--color-accent)" strokeWidth="1.6" aria-hidden>
            <path d="M3 12h4l2-6 3 12 2-6h5" />
          </svg>
        </div>
        <h2 className="mt-4 text-[18px] font-semibold">No requests yet</h2>
        <p className="mt-1 text-[13px] text-muted">
          This gateway is up and waiting. Three steps and the first call lands here.
        </p>

        <ol className="mt-6 space-y-3 text-left">
          <Step n={1} title="Add a provider" to="/providers" cta="Open Providers">
            Sign in with a Claude, ChatGPT or Copilot subscription, or paste an API key.
          </Step>
          <Step n={2} title="Create an API key" to="/keys" cta="Open API Keys">
            Clients authenticate with <span className="font-mono text-[12px] text-muted">sk-rr-…</span>.
          </Step>
          <Step n={3} title="Send a test request" done>
            Any OpenAI- or Anthropic-shaped client works.
          </Step>
        </ol>

        <div className="mt-5 rounded-lg border border-line bg-[var(--color-bg)] p-3 text-left">
          <div className="flex items-center justify-between gap-2 pb-2">
            <span className="text-[11px] uppercase tracking-[0.08em] text-faint">cURL</span>
            <CopyButton value={curl} />
          </div>
          <pre className="overflow-x-auto whitespace-pre-wrap break-all font-mono text-[11.5px] leading-relaxed text-muted">
            {curl}
          </pre>
        </div>

        <p className="mt-4 text-[12px] text-faint">
          Already sending requests? Traffic shows up here within a few seconds.
        </p>
      </div>
    </section>
  );
}

function Step({
  n,
  title,
  to,
  cta,
  done,
  children,
}: {
  n: number;
  title: string;
  to?: string;
  cta?: string;
  done?: boolean;
  children: ReactNode;
}) {
  return (
    <li className="flex items-start gap-3 rounded-lg border border-[var(--color-rule)] px-4 py-3">
      <span className="mt-0.5 flex h-6 w-6 shrink-0 items-center justify-center rounded-full border border-line text-[11px] text-muted tnum">
        {done ? "✓" : n}
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-[13px] font-medium">{title}</span>
          {to && cta ? (
            <Link to={to} className="text-[12px] text-accent hover:underline">
              {cta} →
            </Link>
          ) : null}
        </div>
        <p className="mt-0.5 text-[12.5px] text-muted">{children}</p>
      </div>
    </li>
  );
}
