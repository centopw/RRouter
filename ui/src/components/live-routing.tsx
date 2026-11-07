import { useMemo } from "react";
import { Card, cx } from "./ui";
import { ago } from "../lib/format";
import type { UsageEvent } from "../lib/api";

const LIVE_WINDOW_SECS = 15 * 60;
const ACTIVE_WINDOW_SECS = 90;

const NAMES: Record<string, string> = {
  anthropic: "Anthropic",
  claude: "Claude",
  codex: "Codex",
  openai: "OpenAI",
  gemini: "Gemini",
  deepseek: "DeepSeek",
  groq: "Groq",
  openrouter: "OpenRouter",
  xai: "xAI",
  mistral: "Mistral",
  together: "Together",
  cerebras: "Cerebras",
  ollama: "Ollama",
  github: "GitHub",
};

type Node = {
  provider: string;
  requests: number;
  errors: number;
  lastTs: number;
};

/** Nodes sit on an ellipse around the gateway; two providers get the horizontal axis,
 *  three form a triangle, and larger sets walk the ring so cards never cover the centre. */
function position(index: number, count: number): { x: number; y: number } {
  if (count === 1) return { x: 78, y: 50 };
  if (count === 2) return { x: index === 0 ? 18 : 82, y: 50 };
  const angle = -Math.PI / 2 + (index * 2 * Math.PI) / count;
  return { x: 50 + Math.cos(angle) * 37, y: 50 + Math.sin(angle) * 34 };
}

/**
 * The routing map: one gateway node and one node per provider seen in the window.
 * Edges animate while a provider served a request in the last minute; this is a live
 * readout, not a topology editor.
 */
export function LiveRoutingCard({
  events,
  providers,
  loading,
}: {
  events: UsageEvent[];
  /** Providers with an active connection, so a quiet account still appears. */
  providers: string[];
  loading?: boolean;
}) {
  const now = Math.floor(Date.now() / 1000);
  const nodes = useMemo(() => {
    const recent = events.filter((e) => now - e.ts <= LIVE_WINDOW_SECS);
    const byProvider = new Map<string, Node>();
    for (const provider of providers) {
      byProvider.set(provider, { provider, requests: 0, errors: 0, lastTs: 0 });
    }
    for (const event of recent) {
      const node = byProvider.get(event.provider) ?? {
        provider: event.provider,
        requests: 0,
        errors: 0,
        lastTs: 0,
      };
      node.requests += 1;
      if (event.status >= 400 || event.error !== null) node.errors += 1;
      node.lastTs = Math.max(node.lastTs, event.ts);
      byProvider.set(event.provider, node);
    }
    return [...byProvider.values()]
      .filter((node) => node.requests > 0 || providers.includes(node.provider))
      .sort((a, b) => a.provider.localeCompare(b.provider))
      .slice(0, 6);
  }, [events, providers, now]);

  const requests = nodes.reduce((n, node) => n + node.requests, 0);
  const errors = nodes.reduce((n, node) => n + node.errors, 0);
  const latest = nodes.reduce((n, node) => Math.max(n, node.lastTs), 0);
  const active = latest > 0 && now - latest <= ACTIVE_WINDOW_SECS;

  return (
    <Card>
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-line px-6 py-4">
        <div>
          <div className="flex items-center gap-2">
            <h2 className="text-[15px] font-semibold">Live routing</h2>
            <span
              className={cx(
                "flex items-center gap-1.5 rounded-full px-2 py-0.5 text-[11px] font-medium",
                active
                  ? "bg-[var(--color-pos-tint)] text-[var(--color-pos)]"
                  : "bg-surface-2 text-faint",
              )}
            >
              <span
                aria-hidden
                className={cx(
                  "h-1.5 w-1.5 rounded-full",
                  active ? "live-dot bg-[var(--color-pos)]" : "bg-[var(--color-faint)]",
                )}
              />
              {active ? "routing" : "idle"}
            </span>
          </div>
          <p className="mt-0.5 text-[12.5px] text-muted">
            Providers serving traffic in the last 15 minutes. The map refreshes every 5 seconds.
          </p>
        </div>
        <div className="flex items-center gap-4 text-[12px] text-muted">
          <span>
            <span className="font-mono text-fg tnum">{requests}</span> requests
          </span>
          <span className={errors > 0 ? "text-neg" : "text-faint"}>
            <span className="font-mono tnum">{errors}</span> errors
          </span>
          <span className="text-faint">{latest > 0 ? `last ${ago(latest)}` : "no traffic yet"}</span>
        </div>
      </div>

      {loading && events.length === 0 ? (
        <div aria-hidden className="h-[300px] animate-pulse bg-[var(--color-surface-2)]/40" />
      ) : nodes.length === 0 ? (
        <div className="px-6 py-12 text-center text-[13px] text-faint">
          No provider traffic yet. The map fills in as requests arrive.
        </div>
      ) : (
        <div className="relative h-[300px] overflow-hidden max-[700px]:h-[250px]">
          <div aria-hidden className="route-grid absolute inset-0" />
          <svg
            aria-hidden
            viewBox="0 0 100 100"
            preserveAspectRatio="none"
            className="absolute inset-0 h-full w-full overflow-visible"
          >
            {nodes.map((node, i) => {
              const p = position(i, nodes.length);
              const flowing = node.lastTs > 0 && now - node.lastTs <= ACTIVE_WINDOW_SECS;
              return (
                <g key={node.provider}>
                  <line
                    x1="50"
                    y1="50"
                    x2={p.x}
                    y2={p.y}
                    stroke={node.errors > 0 ? "var(--color-neg-line)" : "var(--color-line-strong)"}
                    strokeWidth="1"
                    vectorEffect="non-scaling-stroke"
                  />
                  {flowing ? (
                    <line
                      x1="50"
                      y1="50"
                      x2={p.x}
                      y2={p.y}
                      stroke={node.errors > 0 ? "var(--color-neg)" : "var(--color-accent)"}
                      strokeWidth="1.5"
                      vectorEffect="non-scaling-stroke"
                      className="route-edge"
                    />
                  ) : null}
                </g>
              );
            })}
          </svg>

          <div className="absolute left-1/2 top-1/2 z-10 -translate-x-1/2 -translate-y-1/2">
            <div className="flex w-[150px] flex-col items-center rounded-xl border border-[var(--color-accent)]/45 bg-surface px-3 py-2.5 text-center shadow-[0_4px_14px_rgba(16,18,20,0.08)]">
              <img
                src={`${import.meta.env.BASE_URL}logo-mark-light.svg`}
                alt=""
                className="h-8 w-8 object-contain"
              />
              <span className="mt-1 text-[13px] font-semibold text-fg">RRouter</span>
              <span className="text-[11px] text-faint tnum">{requests} in window</span>
            </div>
          </div>

          {nodes.map((node, i) => {
            const p = position(i, nodes.length);
            const flowing = node.lastTs > 0 && now - node.lastTs <= ACTIVE_WINDOW_SECS;
            const tone = `var(--color-series-${(i % 6) + 1})`;
            return (
              <div
                key={node.provider}
                className="absolute z-10 -translate-x-1/2 -translate-y-1/2"
                style={{ left: `${p.x}%`, top: `${p.y}%` }}
              >
                <div
                  className={cx(
                    "flex w-[150px] items-center gap-2 rounded-xl border bg-surface px-3 py-2 shadow-[0_2px_8px_rgba(16,18,20,0.06)] transition-colors",
                    flowing
                      ? "border-[var(--color-line-strong)]"
                      : "border-line",
                  )}
                >
                  <span
                    aria-hidden
                    className="h-2 w-2 shrink-0 rounded-full"
                    style={{ backgroundColor: tone }}
                  />
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-[12.5px] font-medium text-fg">
                      {NAMES[node.provider] ?? node.provider}
                    </div>
                    <div className="truncate text-[10.5px] text-faint tnum">
                      {node.requests} req
                      {node.errors > 0 ? ` · ${node.errors} err` : ""}
                      {node.lastTs > 0 ? ` · ${ago(node.lastTs)}` : " · idle"}
                    </div>
                  </div>
                  <span
                    aria-label={flowing ? "served recently" : "idle"}
                    className={cx(
                      "h-1.5 w-1.5 shrink-0 rounded-full",
                      flowing ? "live-dot bg-[var(--color-pos)]" : "bg-[var(--color-line-strong)]",
                    )}
                  />
                </div>
              </div>
            );
          })}
        </div>
      )}
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t border-line px-6 py-2.5 text-[11px] text-faint">
        <span>animated edge = provider served a request in the last 90 seconds</span>
        <span className="ml-auto">5s refresh</span>
      </div>
    </Card>
  );
}
