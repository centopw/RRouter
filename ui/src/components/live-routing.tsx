import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { Link } from "react-router-dom";
import { ArrowCounterClockwise, DotsSixVertical } from "@phosphor-icons/react";
import { Button, Card, PageHeader, cx } from "./ui";
import { BrandMark, type BrandMarkState } from "./brand-mark";
import { ProviderIcon, providerBrand } from "./provider-icon";
import { ago } from "../lib/format";
import { useReducedMotion } from "../lib/media";
import { useConnections, useIsAdmin, useLiveUsage } from "../lib/queries";
import type { UsageEvent } from "../lib/api";

const LIVE_WINDOW_SECS = 15 * 60;
const ACTIVE_WINDOW_SECS = 90;
const POSITION_KEY = "rrouter.live-routing.positions.v1";
const NARROW_POSITION_KEY = `${POSITION_KEY}:narrow`;

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
  opencode: "OpenCode",
  "opencode-go": "OpenCode",
  qoder: "Qoder",
  "google-antigravity": "Antigravity",
  antigravity: "Antigravity",
};

type Point = { x: number; y: number };

type Node = {
  provider: string;
  requests: number;
  errors: number;
  lastTs: number;
  lastError: boolean;
};

const clamp = (value: number, low: number, high: number) =>
  Math.min(high, Math.max(low, value));

/**
 * Initial positions avoid one big ring: small sets orbit the hub, larger sets sit in
 * two rails so a provider card never lands on top of its neighbour. The operator can
 * move every card afterwards; a stored position wins over this layout.
 */
function initialLayout(providers: string[], width: number): Record<string, Point> {
  const result: Record<string, Point> = {};
  const count = providers.length;
  const [first, second] = providers;
  if (count === 1 && first) {
    result[first] = { x: width > 0 && width < 640 ? 68 : 78, y: width > 0 && width < 640 ? 64 : 50 };
    return result;
  }
  if (count === 2 && first && second) {
    const edge = width > 0 && width < 640 ? 26 : 16;
    result[first] = { x: edge, y: 50 };
    result[second] = { x: 100 - edge, y: 50 };
    return result;
  }
  if (width > 0 && width < 640) {
    // Phones put the hub at the top and fan two rails below it; a ring at this width
    // would push cards off the canvas and overlap the centre.
    const split = Math.ceil(count / 2);
    providers.forEach((provider, index) => {
      const left = index < split;
      const sideIndex = left ? index : index - split;
      const sideCount = left ? split : count - split;
      result[provider] = {
        x: left ? 26 : 74,
        y: sideCount === 1 ? 64 : 38 + (sideIndex / (sideCount - 1)) * 52,
      };
    });
    return result;
  }
  if (count <= 6) {
    providers.forEach((provider, index) => {
      const angle = -Math.PI / 2 + (index * 2 * Math.PI) / count;
      result[provider] = {
        x: 50 + Math.cos(angle) * 37,
        y: 50 + Math.sin(angle) * 34,
      };
    });
    return result;
  }
  const split = Math.ceil(count / 2);
  providers.forEach((provider, index) => {
    const left = index < split;
    const sideIndex = left ? index : index - split;
    const sideCount = left ? split : count - split;
    result[provider] = {
      x: left ? 14 : 86,
      y: sideCount === 1 ? 50 : 16 + (sideIndex / (sideCount - 1)) * 68,
    };
  });
  return result;
}

function loadStoredPositions(key: string): Record<string, Point> {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(key) ?? "{}");
    if (!parsed || typeof parsed !== "object") return {};
    const result: Record<string, Point> = {};
    for (const [provider, point] of Object.entries(parsed)) {
      if (
        typeof point === "object" &&
        point !== null &&
        typeof (point as Point).x === "number" &&
        typeof (point as Point).y === "number"
      ) {
        result[provider] = {
          x: clamp((point as Point).x, 4, 96),
          y: clamp((point as Point).y, 6, 94),
        };
      }
    }
    return result;
  } catch {
    return {};
  }
}

function useDocumentTheme(): "light" | "dark" {
  const read = () => {
    const root = document.documentElement;
    return root.dataset.theme === "dark" || root.classList.contains("dark") ? "dark" : "light";
  };
  const [theme, setTheme] = useState<"light" | "dark">(read);
  useEffect(() => {
    const root = document.documentElement;
    const observer = new MutationObserver(() => setTheme(read()));
    observer.observe(root, { attributes: true, attributeFilter: ["data-theme", "class"] });
    return () => observer.disconnect();
  }, []);
  return theme;
}

function usePageVisible(): boolean {
  const [visible, setVisible] = useState(() => document.visibilityState !== "hidden");
  useEffect(() => {
    const update = () => setVisible(document.visibilityState !== "hidden");
    document.addEventListener("visibilitychange", update);
    return () => document.removeEventListener("visibilitychange", update);
  }, []);
  return visible;
}

/**
 * The routing map is a real, persistent canvas: every provider card can be dragged or
 * nudged with the keyboard, and the layout is saved per browser. Edges are drawn from
 * the live event stream, so a quiet provider stays visible without pretending it moved.
 *
 * Mounted directly at `/routing/topology` without props (self-fetching live telemetry)
 * or embedded inside Activity with shared query data.
 */
export function LiveRoutingCard({
  events,
  providers,
  loading,
  standalone,
}: {
  events?: UsageEvent[];
  /** Providers with an active connection, so a quiet account still appears. */
  providers?: string[];
  loading?: boolean;
  /** Expands canvas height and renders a PageHeader when mounted as a full page. */
  standalone?: boolean;
} = {}) {
  if (events === undefined || providers === undefined) {
    return <LiveRoutingStandalone standalone={standalone ?? true} />;
  }
  return (
    <LiveRoutingCanvas
      events={events}
      providers={providers}
      loading={loading}
      standalone={standalone ?? false}
    />
  );
}

function LiveRoutingStandalone({ standalone }: { standalone: boolean }) {
  const isAdmin = useIsAdmin();
  const live = useLiveUsage(200);
  const connections = useConnections(isAdmin);
  const providerIds = useMemo(() => {
    const seen = isAdmin
      ? (connections.data ?? []).filter((c) => c.is_active).map((c) => c.provider)
      : (live.data ?? []).map((event) => event.provider);
    return [...new Set(seen)].sort();
  }, [isAdmin, connections.data, live.data]);

  return (
    <>
      <PageHeader
        title="Topology"
        subtitle="Live upstream routing map across active providers over the last 15 minutes."
        actions={
          isAdmin ? (
            <Link
              to="/providers"
              className="inline-flex h-8 shrink-0 items-center justify-center rounded-full border border-line bg-surface px-3.5 text-body text-muted transition-colors hover:border-line-strong hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
            >
              Manage providers →
            </Link>
          ) : undefined
        }
      />
      <LiveRoutingCanvas
        events={live.data ?? []}
        providers={providerIds}
        loading={live.isLoading || (isAdmin && connections.isLoading)}
        standalone={standalone}
      />
    </>
  );
}

function LiveRoutingCanvas({
  events,
  providers,
  loading,
  standalone,
}: {
  events: UsageEvent[];
  providers: string[];
  loading?: boolean;
  standalone: boolean;
}) {
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  const [stored, setStored] = useState<Record<string, Point>>(() =>
    loadStoredPositions(
      window.matchMedia("(max-width: 700px)").matches ? NARROW_POSITION_KEY : POSITION_KEY,
    ),
  );
  const [dragging, setDragging] = useState<string | null>(null);
  const [announcement, setAnnouncement] = useState("");
  const [canvasSize, setCanvasSize] = useState({ width: 0, height: 0 });
  const canvasRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{
    provider: string;
    pointerId: number;
    dx: number;
    dy: number;
  } | null>(null);
  const reduceMotion = useReducedMotion();
  const pageVisible = usePageVisible();
  const theme = useDocumentTheme();
  const animateLive = !reduceMotion && pageVisible;

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Math.floor(Date.now() / 1000)), 5_000);
    return () => window.clearInterval(timer);
  }, []);

  const nodes = useMemo(() => {
    const byProvider = new Map<string, Node>();
    for (const provider of providers) {
      if (provider) {
        byProvider.set(provider, {
          provider,
          requests: 0,
          errors: 0,
          lastTs: 0,
          lastError: false,
        });
      }
    }
    for (const event of events) {
      if (now - event.ts > LIVE_WINDOW_SECS) continue;
      const node = byProvider.get(event.provider) ?? {
        provider: event.provider,
        requests: 0,
        errors: 0,
        lastTs: 0,
        lastError: false,
      };
      node.requests += 1;
      const failed = event.status >= 400 || event.error !== null;
      if (failed) node.errors += 1;
      if (event.ts >= node.lastTs) {
        node.lastTs = event.ts;
        node.lastError = failed;
      }
      byProvider.set(event.provider, node);
    }
    return [...byProvider.values()]
      .filter((node) => node.requests > 0 || providers.includes(node.provider))
      .sort((a, b) => a.provider.localeCompare(b.provider));
  }, [events, providers, now]);

  const providersKey = useMemo(() => nodes.map((node) => node.provider).join("|"), [nodes]);
  const initial = useMemo(
    () => initialLayout(providersKey ? providersKey.split("|") : [], canvasSize.width),
    [providersKey, canvasSize.width],
  );

  const pointOf = useCallback(
    (provider: string): Point => stored[provider] ?? initial[provider] ?? { x: 50, y: 50 },
    [stored, initial],
  );

  const latestEvent = useMemo(
    () =>
      events.reduce<UsageEvent | null>(
        (latest, event) => (latest === null || event.ts > latest.ts ? event : latest),
        null,
      ),
    [events],
  );
  const liveEvent =
    latestEvent !== null && now - latestEvent.ts <= ACTIVE_WINDOW_SECS ? latestEvent : null;
  const latestProvider = liveEvent?.provider ?? null;
  const latestPoint = latestProvider ? pointOf(latestProvider) : null;
  const latestFailed = liveEvent !== null && (liveEvent.status >= 400 || liveEvent.error !== null);
  const narrow = canvasSize.width > 0 && canvasSize.width < 640;
  const hubPoint = narrow ? { x: 50, y: 18 } : { x: 50, y: 50 };
  const storageKey = narrow ? NARROW_POSITION_KEY : POSITION_KEY;
  useEffect(() => {
    setStored(loadStoredPositions(storageKey));
  }, [storageKey]);

  const [brandState, setBrandState] = useState<BrandMarkState>("idle");
  const eventKey =
    liveEvent === null
      ? "idle"
      : `${liveEvent.ts}:${liveEvent.provider}:${liveEvent.status}:${liveEvent.error ?? ""}`;
  useEffect(() => {
    if (liveEvent === null) {
      setBrandState("idle");
      return;
    }
    setBrandState(latestFailed ? "failover" : "dispatch");
    const timer = window.setTimeout(() => setBrandState("idle"), 1_100);
    return () => window.clearTimeout(timer);
    // eventKey changes for a new event; liveEvent/latestFailed are derived from it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [eventKey]);

  const brandAngle = useMemo(() => {
    if (!latestPoint || canvasSize.width === 0 || canvasSize.height === 0) return 0;
    const dx = (latestPoint.x / 100) * canvasSize.width - (hubPoint.x / 100) * canvasSize.width;
    const dy = (latestPoint.y / 100) * canvasSize.height - (hubPoint.y / 100) * canvasSize.height;
    return (Math.atan2(dy, dx) * 180) / Math.PI;
  }, [latestPoint, canvasSize, hubPoint]);

  const totalRequests = nodes.reduce((sum, node) => sum + node.requests, 0);
  const totalErrors = nodes.reduce((sum, node) => sum + node.errors, 0);
  const activeCount = nodes.filter(
    (node) => node.lastTs > 0 && now - node.lastTs <= ACTIVE_WINDOW_SECS,
  ).length;
  const latestTs = nodes.reduce((latest, node) => Math.max(latest, node.lastTs), 0);
  const active = activeCount > 0;

  useLayoutEffect(() => {
    if (nodes.length === 0) return;
    const canvas = canvasRef.current;
    if (!canvas) return;
    const measure = () => {
      const rect = canvas.getBoundingClientRect();
      setCanvasSize({ width: rect.width, height: rect.height });
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(canvas);
    window.addEventListener("resize", measure);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, [nodes.length, loading, events.length]);

  useEffect(() => {
    const timer = window.setTimeout(() => {
      const keep: Record<string, Point> = {};
      for (const provider of providersKey ? providersKey.split("|") : []) {
        if (stored[provider]) keep[provider] = stored[provider];
      }
      try {
        if (Object.keys(keep).length === 0) localStorage.removeItem(storageKey);
        else localStorage.setItem(storageKey, JSON.stringify(keep));
      } catch {
        // Private mode or a full quota: dragging still works for this visit.
      }
    }, 250);
    return () => window.clearTimeout(timer);
  }, [stored, providersKey, storageKey]);

  const resetLayout = () => {
    setStored({});
    setAnnouncement("Provider layout reset to the automatic arrangement.");
    try {
      localStorage.removeItem(storageKey);
    } catch {
      // Storage is optional; the in-memory state is enough.
    }
  };

  const beginDrag = (event: ReactPointerEvent<HTMLElement>, provider: string) => {
    if (event.button !== 0) return;
    const canvas = canvasRef.current;
    if (!canvas) return;
    const rect = canvas.getBoundingClientRect();
    const point = pointOf(provider);
    dragRef.current = {
      provider,
      pointerId: event.pointerId,
      dx: (point.x / 100) * rect.width - event.clientX,
      dy: (point.y / 100) * rect.height - event.clientY,
    };
    event.currentTarget.setPointerCapture(event.pointerId);
    setDragging(provider);
    event.preventDefault();
  };

  const moveDrag = (event: ReactPointerEvent<HTMLElement>) => {
    const drag = dragRef.current;
    const canvas = canvasRef.current;
    if (!drag || drag.pointerId !== event.pointerId || !canvas) return;
    const rect = canvas.getBoundingClientRect();
    const narrowCanvas = canvasSize.width > 0 && canvasSize.width < 640;
    const halfWidth = Math.min(44, ((narrowCanvas ? 80 : 84) / rect.width) * 100);
    // 63px is the card's rendered height: the usage track added a row below the text.
    const halfHeight = Math.min(36, (32 / rect.height) * 100);
    const x = ((event.clientX + drag.dx) / rect.width) * 100;
    const y = ((event.clientY + drag.dy) / rect.height) * 100;
    setStored((previous) => ({
      ...previous,
      [drag.provider]: {
        x: clamp(x, halfWidth + 1, 99 - halfWidth),
        y: clamp(y, halfHeight + 1, 99 - halfHeight),
      },
    }));
  };

  const endDrag = (event: ReactPointerEvent<HTMLElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    dragRef.current = null;
    setDragging(null);
    try {
      event.currentTarget.releasePointerCapture(event.pointerId);
    } catch {
      // The pointer already left the element; nothing to release.
    }
  };

  const nudge = (event: ReactKeyboardEvent<HTMLElement>, provider: string) => {
    const step = event.shiftKey ? 5 : 1;
    let dx = 0;
    let dy = 0;
    if (event.key === "ArrowLeft") dx = -step;
    else if (event.key === "ArrowRight") dx = step;
    else if (event.key === "ArrowUp") dy = -step;
    else if (event.key === "ArrowDown") dy = step;
    else return;
    event.preventDefault();
    const current = pointOf(provider);
    const marginX = canvasSize.width > 0 && canvasSize.width < 640 ? 22 : 8;
    const next = { x: clamp(current.x + dx, marginX, 100 - marginX), y: clamp(current.y + dy, 8, 92) };
    setStored((previous) => ({ ...previous, [provider]: next }));
    setAnnouncement(
      `${NAMES[provider] ?? provider} moved to ${Math.round(next.x)}% across and ${Math.round(next.y)}% down.`,
    );
  };

  return (
    <Card>
      <div className="flex flex-wrap items-start justify-between gap-3 border-b border-line px-5 py-4">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h2 className="text-lead font-semibold">Live routing</h2>
            <span
              className={cx(
                "flex items-center gap-1.5 rounded-full px-2 py-0.5 text-caption font-medium",
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
              {active ? `${activeCount} ${activeCount === 1 ? "provider" : "providers"} active` : "idle"}
            </span>
          </div>
          <p className="mt-0.5 max-w-[60ch] text-small text-muted">
            Drag a provider to arrange the canvas. The map reads the last 15 minutes and
            refreshes every 5 seconds.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-x-4 gap-y-2 text-small text-muted">
          <span>
            <span className="font-mono text-fg tnum">{totalRequests}</span> requests
          </span>
          <span className={totalErrors > 0 ? "text-neg" : "text-faint"}>
            <span className="font-mono tnum">{totalErrors}</span> errors
          </span>
          <span className="text-faint">{latestTs > 0 ? `last ${ago(latestTs)}` : "no traffic yet"}</span>
          <Button variant="quiet" onClick={resetLayout} className="h-8">
            <ArrowCounterClockwise size={13} aria-hidden />
            Auto arrange
          </Button>
        </div>
      </div>

      {loading && events.length === 0 ? (
        <div
          aria-hidden
          className={cx(
            "animate-pulse bg-[var(--color-surface-2)]/40",
            standalone ? "h-[520px] max-[700px]:h-[480px]" : "h-[380px] max-[700px]:h-[440px]",
          )}
        />
      ) : nodes.length === 0 ? (
        <div className={cx("px-6 text-center text-body text-faint", standalone ? "py-20" : "py-12")}>
          No provider traffic yet. The map fills in as requests arrive.
        </div>
      ) : (
        <div
          ref={canvasRef}
          role="region"
          aria-label="Live routing canvas. Drag provider cards to arrange them."
          aria-describedby="live-routing-help"
          className={cx(
            "route-canvas relative touch-none overflow-hidden",
            standalone ? "h-[520px] max-[700px]:h-[480px]" : "h-[380px] max-[700px]:h-[440px]",
          )}
        >
          <div aria-hidden className="route-grid absolute inset-0" />
          <div
            aria-hidden
            className="pointer-events-none absolute inset-x-0 top-0 h-24 bg-gradient-to-b from-surface/80 to-transparent"
          />
          <svg
            aria-hidden
            viewBox="0 0 100 100"
            preserveAspectRatio="none"
            className="absolute inset-0 h-full w-full overflow-visible"
          >
            {nodes.map((node) => {
              const point = pointOf(node.provider);
              const flowing = node.lastTs > 0 && now - node.lastTs <= ACTIVE_WINDOW_SECS;
              const failed = flowing && node.lastError;
              const stroke = failed
                ? "var(--color-neg)"
                : flowing
                  ? providerBrand(node.provider).color
                  : "var(--color-line-strong)";
              return (
                <g key={node.provider}>
                  <line
                    x1={hubPoint.x}
                    y1={hubPoint.y}
                    x2={point.x}
                    y2={point.y}
                    stroke={stroke}
                    strokeWidth={flowing ? 1.35 : 1}
                    strokeOpacity={flowing ? 0.9 : 0.45}
                    strokeLinecap="round"
                    vectorEffect="non-scaling-stroke"
                    className={flowing && animateLive ? "route-edge" : undefined}
                  />
                  {flowing && latestProvider === node.provider && animateLive ? (
                    <circle r="1.35" fill={failed ? "var(--color-neg)" : providerBrand(node.provider).color}>
                      <animateMotion
                        dur="1.25s"
                        repeatCount="indefinite"
                        path={`M${hubPoint.x} ${hubPoint.y} L${point.x} ${point.y}`}
                      />
                    </circle>
                  ) : null}
                </g>
              );
            })}
          </svg>

          <div
            className="absolute z-20"
            style={
              canvasSize.width > 0 && canvasSize.height > 0
                ? {
                    left: 0,
                    top: 0,
                    transform: `translate3d(${((hubPoint.x / 100) * canvasSize.width).toFixed(1)}px, ${((hubPoint.y / 100) * canvasSize.height).toFixed(1)}px, 0) translate3d(-50%, -50%, 0)`,
                  }
                : {
                    left: `${hubPoint.x}%`,
                    top: `${hubPoint.y}%`,
                    transform: "translate3d(-50%, -50%, 0)",
                  }
            }
          >
            <div className="flex w-[152px] flex-col items-center rounded-2xl border border-[var(--color-line-strong)] bg-surface/95 px-3 py-2.5 text-center shadow-[var(--shadow-panel)] backdrop-blur">
              <BrandMark
                state={brandState}
                angleDeg={brandAngle}
                color={latestFailed ? "#EF4444" : latestProvider ? providerBrand(latestProvider).color : undefined}
                surface={theme}
                decorative
                className="h-11 w-[72px]"
              />
              <span className="mt-1 text-body font-semibold text-fg">RRouter</span>
              <span className="text-caption text-faint tnum">
                {totalRequests} in window
              </span>
            </div>
          </div>

          {nodes.map((node) => {
            const point = pointOf(node.provider);
            const flowing = node.lastTs > 0 && now - node.lastTs <= ACTIVE_WINDOW_SECS;
            const brand = providerBrand(node.provider);
            const name = NAMES[node.provider] ?? node.provider;
            const isDragging = dragging === node.provider;
            const hasSize = canvasSize.width > 0 && canvasSize.height > 0;
            const px = (point.x / 100) * canvasSize.width;
            const py = (point.y / 100) * canvasSize.height;
            return (
              <article
                key={node.provider}
                tabIndex={0}
                aria-label={`${name}: ${node.requests} requests, ${node.errors} errors, ${
                  flowing ? "active" : "idle"
                }. Drag or use the arrow keys to move.`}
                title={`${name} · ${node.requests} requests · ${node.errors} errors${
                  node.lastTs > 0 ? ` · last ${ago(node.lastTs)}` : ""
                }`}
                onPointerDown={(event) => beginDrag(event, node.provider)}
                onPointerMove={moveDrag}
                onPointerUp={endDrag}
                onPointerCancel={endDrag}
                onKeyDown={(event) => nudge(event, node.provider)}
                className={cx(
                  "absolute z-10 w-[168px] max-[760px]:w-[160px] select-none rounded-xl border bg-surface shadow-[var(--shadow-panel)] outline-none focus-visible:ring-2 focus-visible:ring-accent-text/40",
                  flowing ? "border-[var(--color-line-strong)]" : "border-line",
                  node.errors > 0 && "border-[var(--color-neg-line)]",
                  isDragging
                    ? "cursor-grabbing shadow-[var(--shadow-popover)]"
                    : "cursor-grab",
                )}
                style={
                  hasSize
                    ? {
                        left: 0,
                        top: 0,
                        transform: `translate3d(${px.toFixed(1)}px, ${py.toFixed(1)}px, 0) translate3d(-50%, -50%, 0)`,
                        transition:
                          isDragging || reduceMotion
                            ? "none"
                            : "transform 180ms cubic-bezier(0.16, 1, 0.3, 1), box-shadow 120ms ease",
                      }
                    : {
                        left: `${point.x}%`,
                        top: `${point.y}%`,
                        transform: "translate3d(-50%, -50%, 0)",
                        transition: "none",
                      }
                }
              >
                <div className="flex items-center gap-2 px-2.5 pt-2">
                  {/* Active providers read from the tile itself: the mark sits on a
                      tinted, outlined chip instead of a detached status dot. */}
                  <span
                    className={cx(
                      "grid h-7 w-7 shrink-0 place-items-center rounded-lg border",
                      flowing ? "border-transparent" : "border-line bg-surface-2",
                    )}
                    style={
                      flowing
                        ? {
                            background: `color-mix(in srgb, ${brand.color} 12%, var(--color-surface-2))`,
                            borderColor: `color-mix(in srgb, ${brand.color} 45%, transparent)`,
                            boxShadow: `0 0 0 3px color-mix(in srgb, ${brand.color} 10%, transparent)`,
                          }
                        : undefined
                    }
                  >
                    <ProviderIcon provider={node.provider} size={15} />
                  </span>
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-small font-semibold text-fg">{name}</div>
                    <div className="truncate text-caption text-faint tnum">
                      {node.requests} req
                      {node.errors > 0 ? ` · ${node.errors} err` : ""}
                      {node.lastTs > 0 ? ` · ${ago(node.lastTs)}` : " · idle"}
                    </div>
                  </div>
                  <DotsSixVertical size={14} aria-hidden className="shrink-0 text-faint/70" />
                </div>
                <div aria-hidden className="flex items-center px-2.5 pb-2 pt-2">
                  <span className="h-1 min-w-0 flex-1 overflow-hidden rounded-full bg-[var(--color-rule)]">
                    <span
                      className="block h-full rounded-full"
                      style={{
                        width: `${node.requests > 0 ? Math.max(6, Math.min(100, (node.requests / Math.max(1, totalRequests)) * 100)) : 0}%`,
                        background: brand.color,
                      }}
                    />
                  </span>
                </div>
              </article>
            );
          })}

          <div
            id="live-routing-help"
            className="pointer-events-none absolute bottom-2 left-3 right-3 flex flex-wrap items-center gap-x-3 gap-y-1 text-caption text-faint"
          >
            <span>Drag to arrange · arrow keys nudge · shift+arrow moves 5%</span>
            <span className="ml-auto">layout saved on this device</span>
          </div>
          <p className="sr-only" aria-live="polite">{announcement}</p>
        </div>
      )}

      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t border-line px-5 py-2.5 text-caption text-faint">
        <span className="flex items-center gap-1.5">
          <span aria-hidden className="h-1.5 w-5 rounded-full bg-[var(--color-accent)]" />
          traffic in the last 90 seconds
        </span>
        <span className="flex items-center gap-1.5">
          <span aria-hidden className="h-1.5 w-5 rounded-full bg-[var(--color-neg)]" />
          latest event failed
        </span>
        <span className="ml-auto">5s refresh · 15m window</span>
      </div>
    </Card>
  );
}
