import { Suspense, useEffect, useMemo, useState } from "react";
import { NavLink, Outlet, useNavigate } from "react-router-dom";
import { useHealth, useModels } from "./lib/queries";
import { CopyChip, Dot, cx } from "./components/ui";
import { uptime } from "./lib/format";

type NavItem = { to: string; label: string; icon: React.ReactNode; group: string };

const icon = (path: string) => (
  <svg
    width="16"
    height="16"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.6"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden
  >
    <path d={path} />
  </svg>
);

const NAV: NavItem[] = [
  { to: "/activity", label: "Activity", icon: icon("M3 12h4l2-6 3 12 2-6h5"), group: "Gateway" },
  { to: "/providers", label: "Providers", icon: icon("M9 15l6-6M8 7H6a4 4 0 000 8h2M16 17h2a4 4 0 000-8h-2"), group: "Gateway" },
  { to: "/models", label: "Models", icon: icon("M12 3l8 4.5v9L12 21l-8-4.5v-9L12 3zM4 7.5l8 4.5 8-4.5M12 12v9"), group: "Gateway" },
  { to: "/keys", label: "API Keys", icon: icon("M15 7a4 4 0 11-3.5 5.9L4 20H2v-3l8.1-8.1A4 4 0 0115 7z"), group: "Access" },
  { to: "/aliases", label: "Aliases", icon: icon("M4 8h13l-3-3M20 16H7l3 3"), group: "Routing" },
  { to: "/combos", label: "Combos", icon: icon("M4 7h4l8 10h4M20 7h-4L8 17H4"), group: "Routing" },
  { to: "/health", label: "Health", icon: icon("M3 12h4l2-5 3 10 2-5h7"), group: "System" },
];

export function Shell() {
  const health = useHealth();
  const [paletteOpen, setPaletteOpen] = useState(false);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPaletteOpen((v) => !v);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const groups = useMemo(() => {
    const out = new Map<string, NavItem[]>();
    for (const item of NAV) {
      const bucket = out.get(item.group);
      if (bucket) bucket.push(item);
      else out.set(item.group, [item]);
    }
    return [...out.entries()];
  }, []);

  return (
    <div className="flex h-full">
      <aside className="flex w-[236px] shrink-0 flex-col border-r border-line bg-[var(--color-sidebar)]">
        <div className="px-3 pt-3">
          <div className="flex items-center gap-2 rounded-lg px-2 py-2">
            <img
              src={`${import.meta.env.BASE_URL}logo-mark.png`}
              alt=""
              width={30}
              height={30}
              className="h-[30px] w-[30px] shrink-0 select-none"
            />
            <span className="truncate text-[14px] font-semibold tracking-[-0.01em]">RRouter</span>
          </div>
        </div>

        <nav className="mt-3 flex-1 overflow-y-auto px-2 pb-3" aria-label="Dashboard">
          {groups.map(([group, items]) => (
            <div key={group} className="mb-4">
              <div className="px-2 pb-1.5 text-[10px] font-medium uppercase tracking-[0.12em] text-faint">
                {group}
              </div>
              <ul className="space-y-0.5">
                {items.map((item) => (
                  <li key={item.to}>
                    <NavLink
                      to={item.to}
                      className={({ isActive }) =>
                        cx(
                          "relative flex items-center gap-2.5 rounded-lg px-2.5 py-2 text-[13px] transition-colors",
                          isActive
                            ? "bg-[#00d4ff]/10 font-medium text-fg"
                            : "text-muted hover:bg-surface-2 hover:text-fg",
                        )
                      }
                    >
                      {({ isActive }) => (
                        <>
                          {isActive ? (
                            <span
                              aria-hidden
                              className="absolute left-0 top-1.5 h-[calc(100%-12px)] w-[2px] rounded-full bg-accent"
                            />
                          ) : null}
                          <span className="shrink-0 opacity-90">{item.icon}</span>
                          {item.label}
                        </>
                      )}
                    </NavLink>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </nav>

        <div className="border-t border-line px-4 py-3 text-[11px] text-faint">
          <div className="flex items-center gap-2">
            <Dot tone={health.isError ? "neg" : "pos"} />
            <span className="truncate">
              {health.isError ? "unreachable" : `up ${uptime(health.data?.uptime_secs ?? 0)}`}
            </span>
            <button
              onClick={() => setPaletteOpen(true)}
              className="ml-auto rounded border border-line-strong px-1.5 py-0.5 text-[10px] text-muted hover:text-fg"
            >
              ⌘K
            </button>
          </div>
          <div className="mt-1.5 flex items-center justify-between">
            <span>v{health.data?.version ?? "—"}</span>
            <span className="text-[10px] text-faint">MIT · self-hosted</span>
          </div>
        </div>
      </aside>

      <div className="flex min-w-0 flex-1 flex-col">
        <TopBar onSearch={() => setPaletteOpen(true)} />
        <main className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
          <div className="mx-auto max-w-[1180px]">
            <Suspense fallback={<div className="text-[13px] text-faint">loading…</div>}>
              <Outlet />
            </Suspense>
          </div>
        </main>
      </div>

      <CommandPalette open={paletteOpen} onClose={() => setPaletteOpen(false)} />
    </div>
  );
}

function TopBar({ onSearch }: { onSearch: () => void }) {
  return (
    <header className="flex h-[52px] shrink-0 items-center gap-4 border-b border-line px-8">
      <button
        onClick={onSearch}
        className="flex h-8 w-[320px] items-center gap-2 rounded-lg border border-line-strong bg-[var(--color-surface-2)] px-2.5 text-left text-[13px] text-faint hover:border-[var(--color-line-strong)]"
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden>
          <circle cx="11" cy="11" r="7" />
          <path d="M20 20l-3.5-3.5" />
        </svg>
        Search
        <kbd className="ml-auto rounded border border-line-strong px-1 py-0.5 text-[10px]">⌘K</kbd>
      </button>
      <div className="ml-auto flex min-w-0 items-center gap-3 text-[12px] text-muted">
        <span className="hidden shrink-0 sm:inline">OpenAI-compatible gateway</span>
        <CopyChip value={`${window.location.origin}/v1`} />
      </div>
    </header>
  );
}

function CommandPalette({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [query, setQuery] = useState("");
  const navigate = useNavigate();
  const models = useModels();

  useEffect(() => {
    if (open) setQuery("");
  }, [open]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    if (open) document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  const results = useMemo(() => {
    const q = query.trim().toLowerCase();
    const pages = NAV.filter((n) => !q || n.label.toLowerCase().includes(q)).slice(0, 6);
    const modelHits = (models.data ?? [])
      .filter((m) => q && m.ref.toLowerCase().includes(q))
      .slice(0, 6);
    return { pages, modelHits };
  }, [query, models.data]);

  if (!open) return null;

  const go = (path: string) => {
    navigate(path);
    onClose();
  };

  return (
    <div className="fixed inset-0 z-50 bg-black/70 p-4 pt-[14vh]" onClick={onClose}>
      <div
        className="mx-auto w-full max-w-xl overflow-hidden rounded-[var(--radius-card)] border border-line-strong bg-surface shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <input
          autoFocus
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Jump to a page or model…"
          aria-label="Search pages and models"
          className="h-11 w-full border-b border-line bg-transparent px-4 text-[14px] text-fg placeholder:text-faint focus:outline-none"
        />
        <div className="max-h-[50vh] overflow-y-auto p-2">
          {results.pages.length > 0 ? (
            <Section title="Pages">
              {results.pages.map((p) => (
                <button
                  key={p.to}
                  onClick={() => go(p.to)}
                  className="flex w-full items-center gap-2.5 rounded-md px-2 py-2 text-left text-[13px] text-muted hover:bg-surface-2 hover:text-fg"
                >
                  {p.icon}
                  {p.label}
                </button>
              ))}
            </Section>
          ) : null}
          {results.modelHits.length > 0 ? (
            <Section title="Models">
              {results.modelHits.map((m) => (
                <button
                  key={m.ref}
                  onClick={() => go(`/models?q=${encodeURIComponent(m.ref)}`)}
                  className="flex w-full items-center justify-between gap-2 rounded-md px-2 py-2 text-left text-[13px] text-muted hover:bg-surface-2 hover:text-fg"
                >
                  <span className="truncate">{m.ref}</span>
                  <span className="text-[11px] text-faint">{m.provider}</span>
                </button>
              ))}
            </Section>
          ) : null}
          {results.pages.length === 0 && results.modelHits.length === 0 ? (
            <div className="px-3 py-6 text-center text-[13px] text-faint">no matches</div>
          ) : null}
        </div>
      </div>
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="mb-1">
      <div className="px-2 py-1 text-[10px] uppercase tracking-[0.1em] text-faint">{title}</div>
      {children}
    </div>
  );
}
