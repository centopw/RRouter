import { Suspense, useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { NavLink, Outlet, useNavigate } from "react-router-dom";
import { api } from "./lib/api";
import { useHealth, useModels, useSystem } from "./lib/queries";
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

/** White-label settings written through `/api/settings`; every field is optional. */
type Brand = {
  brand_name?: string;
  brand_color?: string;
  logo_url?: string;
  footer_text?: string;
};

const DEFAULT_NAME = "RRouter";

const NAV: NavItem[] = [
  { to: "/activity", label: "Activity", icon: icon("M3 12h4l2-6 3 12 2-6h5"), group: "Gateway" },
  { to: "/playground", label: "Playground", icon: icon("M4 5h16v11H8l-4 4V5z"), group: "Gateway" },
  { to: "/providers", label: "Providers", icon: icon("M9 15l6-6M8 7H6a4 4 0 000 8h2M16 17h2a4 4 0 000-8h-2"), group: "Gateway" },
  { to: "/models", label: "Models", icon: icon("M12 3l8 4.5v9L12 21l-8-4.5v-9L12 3zM4 7.5l8 4.5 8-4.5M12 12v9"), group: "Gateway" },
  { to: "/keys", label: "API Keys", icon: icon("M15 7a4 4 0 11-3.5 5.9L4 20H2v-3l8.1-8.1A4 4 0 0115 7z"), group: "Access" },
  { to: "/aliases", label: "Aliases", icon: icon("M4 8h13l-3-3M20 16H7l3 3"), group: "Routing" },
  { to: "/combos", label: "Combos", icon: icon("M4 7h4l8 10h4M20 7h-4L8 17H4"), group: "Routing" },
  { to: "/health", label: "Health", icon: icon("M3 12h4l2-5 3 10 2-5h7"), group: "System" },
  { to: "/settings", label: "Settings", icon: icon("M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12M20 18h0M16 4v4M8 10v4M16 16v4"), group: "System" },
];

/** A first run (no accounts, no keys) lands on the wizard once; the flag is set by the
 *  wizard itself when the operator skips or finishes, so this never fights a choice. */
const SETUP_DISMISSED = "rrouter:setup-dismissed";

export function Shell() {
  const health = useHealth();
  const system = useSystem();
  const settings = useQuery({
    queryKey: ["gateway-settings"],
    queryFn: () => api.get<Brand>("/api/settings"),
    staleTime: 60_000,
  });
  const brand = settings.data ?? {};
  const name = brand.brand_name?.trim() || DEFAULT_NAME;
  const logo = brand.logo_url?.trim();
  const logoOk = logo !== undefined && /^(https?:\/\/|data:image\/)/.test(logo);

  useEffect(() => {
    document.title = name;
    const color = brand.brand_color?.trim();
    if (color && /^#[0-9a-f]{3,8}$/i.test(color)) {
      document.documentElement.style.setProperty("--color-accent", color);
    }
  }, [name, brand.brand_color]);

  const navigate = useNavigate();
  const [paletteOpen, setPaletteOpen] = useState(false);

  useEffect(() => {
    const counts = system.data?.counts;
    if (!counts) return;
    if (counts.connections > 0 || counts.api_keys > 0) return;
    if (window.localStorage.getItem(SETUP_DISMISSED) === "1") return;
    navigate("/setup", { replace: true });
  }, [system.data, navigate]);

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

  const endpoint = `${window.location.origin}/v1`;
  const running = !health.isError;

  return (
    <div className="flex h-full flex-col min-[820px]:flex-row">
      <aside className="hidden w-[236px] shrink-0 flex-col border-r border-line bg-[var(--color-sidebar)] min-[820px]:flex">
        <div className="px-3 pt-3">
          <div className="flex items-center gap-2 rounded-lg px-2 py-2">
            <img
              src={logoOk ? logo : `${import.meta.env.BASE_URL}logo-mark.png`}
              alt=""
              width={30}
              height={30}
              className="h-[30px] w-[30px] shrink-0 select-none object-contain"
            />
            <span className="truncate text-[14px] font-semibold tracking-[-0.01em]">{name}</span>
          </div>
        </div>

        <div className="px-3 pb-2">
          <JumpButton onClick={() => setPaletteOpen(true)} />
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
                    <NavItemLink item={item} variant="sidebar" />
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </nav>

        <SidebarFooter
          endpoint={endpoint}
          running={running}
          uptimeSecs={health.data?.uptime_secs ?? 0}
          version={health.data?.version}
          note={brand.footer_text}
        />
      </aside>

      <header className="shrink-0 border-b border-line bg-[var(--color-sidebar)] min-[820px]:hidden">
        <div className="flex h-14 items-center gap-3 px-3">
          <img
            src={logoOk ? logo : `${import.meta.env.BASE_URL}logo-mark.png`}
            alt=""
            width={24}
            height={24}
            className="h-6 w-6 shrink-0 select-none object-contain"
          />
          <span className="text-[13px] font-semibold">{name}</span>
          <span className="flex items-center gap-1.5">
            <RunningDot running={running} />
            <span className="sr-only">{running ? "Gateway running" : "Gateway unreachable"}</span>
          </span>
          <button
            type="button"
            onClick={() => setPaletteOpen(true)}
            aria-label="Jump to a page or model"
            className="ml-auto flex h-11 w-11 items-center justify-center rounded-lg border border-line bg-surface text-muted transition-colors hover:text-fg"
          >
            <SearchIcon size={18} />
          </button>
        </div>
        <nav className="flex gap-1 overflow-x-auto px-2 pb-2" aria-label="Dashboard">
          {NAV.map((item) => (
            <NavItemLink key={item.to} item={item} variant="tab" />
          ))}
        </nav>
      </header>

      <main className="min-h-0 min-w-0 flex-1 overflow-y-auto px-4 py-4 min-[820px]:px-10 min-[820px]:pb-14 min-[820px]:pt-8">
        <div className="mx-auto w-full max-w-[1160px]">
          <Suspense fallback={<div className="text-[13px] text-faint">loading…</div>}>
            <Outlet />
          </Suspense>
        </div>
      </main>

      <CommandPalette open={paletteOpen} onClose={() => setPaletteOpen(false)} />
    </div>
  );
}

function SearchIcon({ size = 14 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      aria-hidden
      className="shrink-0"
    >
      <circle cx="11" cy="11" r="7" />
      <path d="M20 20l-3.5-3.5" />
    </svg>
  );
}

/** The sidebar's palette affordance. It is a button, not an input: the real field lives
 *  in the dialog, so ⌘K and the click share one entry point. */
function JumpButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label="Jump to a page or model"
      className="flex h-9 w-full items-center gap-2 rounded-lg border border-line bg-surface px-2.5 text-left text-[13px] text-faint transition-colors hover:border-line-strong hover:text-fg"
    >
      <SearchIcon />
      <span className="truncate">Jump to…</span>
      <kbd className="ml-auto shrink-0 rounded border border-line bg-surface-2 px-1 py-0.5 font-sans text-[10px] text-muted">
        ⌘K
      </kbd>
    </button>
  );
}

/** Status reads from colour alone nowhere: the dot is paired with text everywhere. */
function RunningDot({ running }: { running: boolean }) {
  if (!running) return <Dot tone="neg" />;
  return (
    <span className="pulse inline-flex">
      <Dot tone="pos" />
    </span>
  );
}

/** Endpoint, liveness and version in one card. `CopyChip` carries its own aria-label,
 *  and truncates rather than widening the 236px rail. */
function SidebarFooter({
  endpoint,
  running,
  uptimeSecs,
  version,
  note,
}: {
  endpoint: string;
  running: boolean;
  uptimeSecs: number;
  version?: string;
  note?: string;
}) {
  return (
    <div className="border-t border-line px-3 py-3">
      <div className="rounded-lg border border-line bg-surface px-2.5 py-2">
        <div className="text-[10px] font-medium uppercase tracking-[0.12em] text-faint">Endpoint</div>
        <div className="mt-1 flex min-w-0">
          <CopyChip value={endpoint} display={endpoint.replace(/^https?:\/\//, "")} />
        </div>
        <div className="mt-2 flex items-center gap-1.5 text-[11px] text-muted">
          <RunningDot running={running} />
          <span className="truncate">
            {running ? `Running · up ${uptime(uptimeSecs)}` : "unreachable"}
          </span>
        </div>
        <div className="mt-0.5 truncate text-[11px] text-faint">
          v{version ?? "—"} · {note?.trim() || "MIT"}
        </div>
      </div>
    </div>
  );
}

/** One nav item, two orientations: the rail stacks a left accent bar, the mobile tab row
 *  a bottom one. `NavLink` owns `aria-current`, so active state is announced, not just seen. */
function NavItemLink({ item, variant }: { item: NavItem; variant: "sidebar" | "tab" }) {
  return (
    <NavLink
      to={item.to}
      className={({ isActive }) =>
        cx(
          "relative flex items-center text-[13px] transition-colors",
          variant === "tab"
            ? "h-11 shrink-0 gap-2 whitespace-nowrap rounded-lg px-3"
            : "gap-2.5 rounded-lg px-2.5 py-2",
          isActive ? "font-medium text-fg" : "text-muted hover:bg-row-hover hover:text-fg",
          variant === "sidebar" && isActive && "bg-surface-2",
        )
      }
    >
      {({ isActive }) => (
        <>
          {isActive ? (
            <span
              aria-hidden
              className={cx(
                "absolute rounded-full bg-accent",
                variant === "tab"
                  ? "inset-x-2 bottom-0 h-[2px]"
                  : "left-0 top-1.5 h-[calc(100%-12px)] w-[2px]",
              )}
            />
          ) : null}
          <span className="shrink-0 opacity-90">{item.icon}</span>
          {item.label}
        </>
      )}
    </NavLink>
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
