import { Suspense, useEffect, useMemo, useState } from "react";
import {
  Pulse,
  ArrowsLeftRight,
  ChatCircleText,
  Cube,
  FileText,
  GearSix,
  Heartbeat,
  ListMagnifyingGlass,
  UserCircle,
  Key,
  ListBullets,
  MagnifyingGlass,
  PlugsConnected,
  SidebarSimple,
  SignOut,
  Shuffle,
  UsersThree,
} from "@phosphor-icons/react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { NavLink, Outlet, useNavigate } from "react-router-dom";
import { ADMIN_REQUIRED_EVENT, api, setAdminToken } from "./lib/api";
import { useAuthState, useHealth, useModels, useSignOut, useSystem } from "./lib/queries";
import { Button, CopyChip, Dot, cx } from "./components/ui";
import { uptime } from "./lib/format";

type NavItem = { to: string; label: string; icon: React.ReactNode; group: string };

const glyph = (Icon: typeof Pulse) => <Icon size={17} weight="regular" aria-hidden />;

/** White-label settings written through `/api/settings`; every field is optional. */
type Brand = {
  brand_name?: string;
  brand_color?: string;
  logo_url?: string;
  footer_text?: string;
};

const DEFAULT_NAME = "RRouter";

const NAV: NavItem[] = [
  { to: "/activity", label: "Activity", icon: glyph(Pulse), group: "Gateway" },
  { to: "/logs", label: "Logs", icon: glyph(ListBullets), group: "Gateway" },
  { to: "/playground", label: "Playground", icon: glyph(ChatCircleText), group: "Gateway" },
  { to: "/templates", label: "Templates", icon: glyph(FileText), group: "Gateway" },
  { to: "/providers", label: "Providers", icon: glyph(PlugsConnected), group: "Gateway" },
  { to: "/models", label: "Models", icon: glyph(Cube), group: "Gateway" },
  { to: "/keys", label: "API Keys", icon: glyph(Key), group: "Access" },
  { to: "/users", label: "Users", icon: glyph(UsersThree), group: "Access" },
  { to: "/aliases", label: "Aliases", icon: glyph(ArrowsLeftRight), group: "Routing" },
  { to: "/combos", label: "Combos", icon: glyph(Shuffle), group: "Routing" },
  { to: "/account", label: "My account", icon: glyph(UserCircle), group: "System" },
  { to: "/audit", label: "Audit", icon: glyph(ListMagnifyingGlass), group: "System" },
  { to: "/health", label: "Health", icon: glyph(Heartbeat), group: "System" },
  { to: "/settings", label: "Settings", icon: glyph(GearSix), group: "System" },
];

/** A first run (no accounts, no keys) lands on the wizard once; the flag is set by the
 *  wizard itself when the operator skips or finishes, so this never fights a choice. */
const SETUP_DISMISSED = "rrouter:setup-dismissed";
const SIDEBAR_COLLAPSED = "rrouter:sidebar-collapsed";

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
  // A collapsed rail keeps the icons and drops the labels; the choice survives reloads.
  const [collapsed, setCollapsed] = useState(() => {
    try {
      return window.localStorage.getItem(SIDEBAR_COLLAPSED) === "1";
    } catch {
      return false;
    }
  });
  const toggleCollapsed = () => {
    setCollapsed((value) => {
      try {
        window.localStorage.setItem(SIDEBAR_COLLAPSED, value ? "0" : "1");
      } catch {
        // Storage can be unavailable; the in-memory choice still works.
      }
      return !value;
    });
  };
  // Any 401 from an admin path means the deployment set RROUTER_ADMIN_TOKEN and this tab
  // has not presented it yet (or presented a wrong one).
  const [adminRequired, setAdminRequired] = useState(false);

  useEffect(() => {
    const require = () => setAdminRequired(true);
    window.addEventListener(ADMIN_REQUIRED_EVENT, require);
    return () => window.removeEventListener(ADMIN_REQUIRED_EVENT, require);
  }, []);

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

  if (adminRequired) {
    return <AdminGate onUnlock={() => setAdminRequired(false)} />;
  }

  return (
    <div className="flex h-full flex-col min-[820px]:flex-row">
      <aside
        className={cx(
          "hidden shrink-0 flex-col border-r border-line bg-[var(--color-sidebar)] transition-[width] duration-200 min-[820px]:flex",
          collapsed ? "w-[68px]" : "w-[236px]",
        )}
      >
        <div className={cx("flex items-center gap-2 pt-3", collapsed ? "justify-center px-2" : "px-3")}>
          <img
            src={logoOk ? logo : `${import.meta.env.BASE_URL}logo-mark-light.svg`}
            alt=""
            width={30}
            height={30}
            className="h-[30px] w-[30px] shrink-0 select-none object-contain"
          />
          {!collapsed ? (
            <span className="min-w-0 flex-1 truncate text-[14px] font-semibold tracking-[-0.01em] text-fg">
              {name}
            </span>
          ) : null}
          <button
            type="button"
            onClick={toggleCollapsed}
            aria-label={collapsed ? "expand sidebar" : "collapse sidebar"}
            aria-expanded={!collapsed}
            title={collapsed ? "expand sidebar" : "collapse sidebar"}
            className={cx(
              "flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-faint hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text",
              collapsed && "mt-1",
            )}
          >
            <SidebarSimple size={16} aria-hidden />
          </button>
        </div>

        <div className={collapsed ? "px-2 pb-1 pt-3" : "px-3 pb-2 pt-2"}>
          <JumpButton onClick={() => setPaletteOpen(true)} collapsed={collapsed} />
        </div>

        <nav className={cx("mt-2 flex-1 overflow-y-auto pb-3", collapsed ? "px-2" : "px-2")} aria-label="Dashboard">
          {groups.map(([group, items]) => (
            <div key={group} className="mb-3">
              {collapsed ? (
                <div aria-hidden className="mx-2 mb-2 border-t border-line" />
              ) : (
                <div className="px-2 pb-1.5 text-[10.5px] font-semibold uppercase tracking-[0.12em] text-faint">
                  {group}
                </div>
              )}
              <ul className="space-y-0.5">
                {items.map((item) => (
                  <li key={item.to}>
                    <NavItemLink item={item} variant="sidebar" collapsed={collapsed} />
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </nav>

        <SessionFooter collapsed={collapsed} />

        {collapsed ? (
          <div className="flex justify-center border-t border-line py-3" title={running ? "gateway running" : "gateway unreachable"}>
            <RunningDot running={running} />
            <span className="sr-only">{running ? "Gateway running" : "Gateway unreachable"}</span>
          </div>
        ) : (
          <SidebarFooter
            endpoint={endpoint}
            running={running}
            uptimeSecs={health.data?.uptime_secs ?? 0}
            version={health.data?.version}
            note={brand.footer_text}
          />
        )}
      </aside>

      <header className="shrink-0 border-b border-line bg-surface min-[820px]:hidden">
        <div className="flex h-14 items-center gap-3 px-3">
          <img
            src={logoOk ? logo : `${import.meta.env.BASE_URL}logo-mark-light.svg`}
            alt=""
            width={24}
            height={24}
            className="h-6 w-6 shrink-0 select-none object-contain"
          />
          <span className="text-[13px] font-semibold text-fg">{name}</span>
          <span className="flex items-center gap-1.5">
            <RunningDot running={running} />
            <span className="sr-only">{running ? "Gateway running" : "Gateway unreachable"}</span>
          </span>
          <button
            type="button"
            onClick={() => setPaletteOpen(true)}
            aria-label="Jump to a page or model"
            className="ml-auto flex h-11 w-11 items-center justify-center rounded-full border border-line-strong bg-surface text-muted transition-colors hover:bg-surface-3 hover:text-fg"
          >
            <MagnifyingGlass size={18} aria-hidden />
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


/** The sidebar's palette affordance. It is a button, not an input: the real field lives
 *  in the dialog, so ⌘K and the click share one entry point. */
function JumpButton({ onClick, collapsed }: { onClick: () => void; collapsed?: boolean }) {
  if (collapsed) {
    return (
      <button
        type="button"
        onClick={onClick}
        aria-label="Jump to a page or model"
        title="Jump to… (⌘K)"
        className="mx-auto flex h-9 w-9 items-center justify-center rounded-full border border-line-strong bg-surface text-faint hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
      >
        <MagnifyingGlass size={15} aria-hidden />
      </button>
    );
  }
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label="Jump to a page or model"
      className="flex h-9 w-full items-center gap-2 rounded-full border border-line-strong bg-surface px-3 text-left text-[13px] text-muted transition-colors hover:bg-surface-3 hover:text-fg"
    >
      <MagnifyingGlass size={14} aria-hidden />
      <span className="truncate">Jump to…</span>
      <kbd className="ml-auto shrink-0 rounded border border-line px-1 py-0.5 font-sans text-[10px] text-faint">
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
/** The signed-in person, and the way out. */
function SessionFooter({ collapsed }: { collapsed?: boolean }) {
  const state = useAuthState();
  const signOut = useSignOut();
  const navigate = useNavigate();
  const user = state.data?.user;
  if (!user) return null;
  const signOutAndLeave = () =>
    signOut.mutate(undefined, {
      onSettled: () => navigate("/sign-in", { replace: true }),
    });
  if (collapsed) {
    return (
      <div className="flex flex-col items-center gap-1 border-t border-line py-2">
        <span
          title={`${user.name} · ${user.email}`}
          className="flex h-8 w-8 items-center justify-center rounded-full bg-surface-3 text-[12px] font-semibold text-fg"
        >
          {user.name.slice(0, 1).toUpperCase()}
        </span>
        <button
          type="button"
          onClick={signOutAndLeave}
          disabled={signOut.isPending}
          aria-label="sign out"
          title="sign out"
          className="flex h-8 w-8 items-center justify-center rounded-full text-faint hover:bg-surface-3 hover:text-fg"
        >
          <SignOut size={15} aria-hidden />
        </button>
      </div>
    );
  }
  return (
    <div className="flex items-center gap-2 border-t border-line px-3 py-2.5">
      <div className="min-w-0 flex-1">
        <div className="truncate text-[12px] font-medium text-fg">{user.name}</div>
        <div className="truncate text-[11px] text-faint">{user.email}</div>
      </div>
      <Button variant="quiet" disabled={signOut.isPending} onClick={signOutAndLeave}>
        sign out
      </Button>
    </div>
  );
}

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
      <div className="rounded-[8px] border border-line bg-surface-2 px-2.5 py-2">
        <div className="text-[10.5px] font-semibold uppercase tracking-[0.12em] text-faint">Endpoint</div>
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
function NavItemLink({
  item,
  variant,
  collapsed = false,
}: {
  item: NavItem;
  variant: "sidebar" | "tab";
  collapsed?: boolean;
}) {
  return (
    <NavLink
      to={item.to}
      title={variant === "sidebar" && collapsed ? item.label : undefined}
      aria-label={variant === "sidebar" && collapsed ? item.label : undefined}
      className={({ isActive }) =>
        cx(
          "relative flex items-center text-[13px] transition-colors",
          variant === "tab"
            ? "h-11 shrink-0 gap-2 whitespace-nowrap rounded-lg px-3"
            : collapsed
              ? "justify-center rounded-[8px] px-0 py-2"
              : "gap-2.5 rounded-[8px] px-2.5 py-2",
          variant === "sidebar"
            ? isActive
              ? "bg-[var(--color-selected)] font-medium text-accent-text"
              : "text-ink-soft hover:bg-surface-3 hover:text-fg"
            : isActive
              ? "bg-accent font-medium text-[var(--color-accent-ink)]"
              : "text-muted hover:bg-surface-3 hover:text-fg",
        )
      }
    >
      {({ isActive }) => (
        <>
          {isActive ? (
            <span
              aria-hidden
              className={cx(
                "absolute rounded-full bg-[var(--color-nav-accent)]",
                variant === "tab"
                  ? "inset-x-2 bottom-0 h-[2px]"
                  : collapsed
                    ? "inset-y-1.5 left-0 w-[2px]"
                    : "left-0 top-1.5 h-[calc(100%-12px)] w-[2px]",
              )}
            />
          ) : null}
          <span className="shrink-0">{item.icon}</span>
          {collapsed ? null : item.label}
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

/**
 * Shown when the gateway requires `RROUTER_ADMIN_TOKEN`. The dashboard runs on the same
 * origin, so the operator pastes the token once per tab; it is held in sessionStorage and
 * sent as `x-admin-token` on admin calls.
 */
function AdminGate({ onUnlock }: { onUnlock: () => void }) {
  const client = useQueryClient();
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    const value = token.trim();
    if (!value || busy) return;
    setBusy(true);
    setError(null);
    setAdminToken(value);
    try {
      await api.get("/api/system");
      await client.invalidateQueries();
      onUnlock();
    } catch (e) {
      setAdminToken(null);
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex min-h-full items-center justify-center bg-bg px-4 py-10">
      <form
        className="w-full max-w-[420px] rounded-[var(--radius-card)] border border-line bg-surface p-6"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <div className="flex items-center gap-2.5">
          <img
            src={`${import.meta.env.BASE_URL}logo-mark-light.svg`}
            alt=""
            width={26}
            height={26}
            className="h-[26px] w-[26px]"
          />
          <span className="text-[15px] font-semibold">Admin token required</span>
        </div>
        <p className="mt-2 text-[13px] text-muted">
          This gateway was started with <span className="font-mono text-[12px]">RROUTER_ADMIN_TOKEN</span>.
          Paste it to manage providers, keys and models from this browser. It is kept for this
          tab only.
        </p>
        <label className="mt-5 flex flex-col gap-1.5">
          <span className="text-[11px] font-medium uppercase tracking-[0.08em] text-faint">Token</span>
          <input
            type="password"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            autoFocus
            aria-label="admin token"
            className="h-9 w-full rounded-lg border border-line bg-surface-2 px-2.5 font-mono text-[13px] text-fg placeholder:text-faint focus:border-[var(--color-accent)]/60 focus:outline-none"
            placeholder="the value of RROUTER_ADMIN_TOKEN"
          />
        </label>
        {error ? <p className="mt-3 text-[12.5px] text-neg">{error}</p> : null}
        <div className="mt-5 flex items-center justify-between gap-3">
          <a href="/health" className="text-[12px] text-faint underline underline-offset-4 hover:text-fg">
            gateway health
          </a>
          <button
            type="submit"
            disabled={busy || token.trim().length === 0}
            className="inline-flex h-9 items-center rounded-lg bg-accent px-4 text-[13px] font-semibold text-[var(--color-accent-ink)] disabled:opacity-40"
          >
            {busy ? "Checking…" : "Unlock"}
          </button>
        </div>
      </form>
    </div>
  );
}
