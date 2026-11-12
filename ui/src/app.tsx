import { Suspense, useEffect, useMemo, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import {
  Pulse,
  ArrowsLeftRight,
  ChatCircleText,
  GearSix,
  Key,
  MagnifyingGlass,
  SidebarSimple,
  SignOut,
} from "@phosphor-icons/react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, Navigate, NavLink, Outlet, useLocation, useNavigate } from "react-router-dom";
import { ADMIN_REQUIRED_EVENT, api, setAdminToken } from "./lib/api";
import { useAuthState, useHealth, useIsAdmin, useModels, useSignOut, useSystem, useUpdateStatus } from "./lib/queries";
import { Button, CopyChip, Dot, cx } from "./components/ui";
import { uptime } from "./lib/format";

type NavItem = {
  to: string;
  label: string;
  icon: React.ReactNode;
  group: string;
  /** Hidden from members and viewers; the matching route is guarded too. */
  adminOnly?: boolean;
};

type NavTab = {
  to: string;
  label: string;
  /** Exact matching is needed when this tab is also the section root. */
  end?: boolean;
  adminOnly?: boolean;
};

type SectionId = "activity" | "playground" | "routing" | "access" | "system";

type NavSection = {
  id: SectionId;
  to: string;
  label: string;
  icon: React.ReactNode;
  tabs: NavTab[];
};

const glyph = (Icon: typeof Pulse) => <Icon size={17} weight="regular" aria-hidden />;

/** White-label settings written through `/api/settings`; every field is optional. */
type Brand = {
  brand_name?: string;
  brand_color?: string;
  logo_url?: string;
  footer_text?: string;
};

const DEFAULT_NAME = "RRouter";

/**
 * Five surfaces, each with tabs. The sidebar shows the surface; the tab strip shows the
 * pages. Daily pages are the first tab, rare configuration stays under System.
 */
const SECTIONS: NavSection[] = [
  {
    id: "activity",
    to: "/activity",
    label: "Activity",
    icon: glyph(Pulse),
    tabs: [
      { to: "/activity", label: "Activity", end: true },
      { to: "/activity/logs", label: "Logs", adminOnly: true },
      { to: "/activity/audit", label: "Audit", adminOnly: true },
    ],
  },
  {
    id: "playground",
    to: "/playground",
    label: "Playground",
    icon: glyph(ChatCircleText),
    tabs: [
      { to: "/playground/chat", label: "Playground", adminOnly: true },
      { to: "/playground/templates", label: "Templates" },
      { to: "/playground/evals", label: "Evals" },
    ],
  },
  {
    id: "routing",
    to: "/routing",
    label: "Routing",
    icon: glyph(ArrowsLeftRight),
    tabs: [
      { to: "/routing/models", label: "Models" },
      { to: "/routing/providers", label: "Providers" },
      { to: "/routing/aliases", label: "Aliases", adminOnly: true },
      { to: "/routing/combos", label: "Combos", adminOnly: true },
    ],
  },
  {
    id: "access",
    to: "/access",
    label: "Access",
    icon: glyph(Key),
    tabs: [
      { to: "/access/keys", label: "API Keys", adminOnly: true },
      { to: "/access/teams", label: "Teams" },
      { to: "/access/users", label: "Users", adminOnly: true },
    ],
  },
  {
    id: "system",
    to: "/system",
    label: "System",
    icon: glyph(GearSix),
    tabs: [
      { to: "/system/health", label: "Health", adminOnly: true },
      { to: "/system/webhooks", label: "Webhooks", adminOnly: true },
      { to: "/system/logs", label: "System logs", adminOnly: true },
      { to: "/system/settings", label: "Settings", adminOnly: true },
    ],
  },
];

function visibleTabs(section: NavSection, isAdmin: boolean) {
  return section.tabs.filter((tab) => isAdmin || !tab.adminOnly);
}

function sectionDefault(section: NavSection, isAdmin: boolean) {
  return visibleTabs(section, isAdmin)[0]?.to ?? section.to;
}

/** A first run (no accounts, no keys) lands on the wizard once; the flag is set by the
 *  wizard itself when the operator skips or finishes, so this never fights a choice. */
const SETUP_DISMISSED = "rrouter:setup-dismissed";
const SIDEBAR_COLLAPSED = "rrouter:sidebar-collapsed";

export function Shell() {
  const isAdmin = useIsAdmin();
  const health = useHealth();
  // `/api/system` is admin surface; asking as a reader would only produce a 403 toast.
  const system = useSystem(isAdmin);
  // White-label settings are admin surface; a reader gets the built-in brand rather
  // than a 403 behind every page load.
  const settings = useQuery({
    queryKey: ["gateway-settings"],
    queryFn: () => api.get<Brand>("/api/settings"),
    staleTime: 60_000,
    enabled: isAdmin,
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
  const location = useLocation();
  const [paletteOpen, setPaletteOpen] = useState(false);
  // The rail fades its ends only when the list actually scrolls, so a short nav has no
  // unexplained gradient. The gutter keeps labels from shifting when it grows.
  const navRef = useRef<HTMLElement | null>(null);
  const [navEdges, setNavEdges] = useState({ top: false, bottom: false });
  // The phone tab strip is a horizontal scroller with no visible bar: the active route has
  // to bring itself into view, or a deep link lands on a tab that is off-screen.
  const tabStrip = useRef<HTMLElement | null>(null);
  useEffect(() => {
    const active = tabStrip.current?.querySelector<HTMLElement>('[aria-current="page"]');
    active?.scrollIntoView({ inline: "center", block: "nearest" });
  }, [location.pathname]);
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
    if (!isAdmin) return;
    const counts = system.data?.counts;
    if (!counts) return;
    if (counts.connections > 0 || counts.api_keys > 0) return;
    if (window.localStorage.getItem(SETUP_DISMISSED) === "1") return;
    navigate("/setup", { replace: true });
  }, [isAdmin, system.data, navigate]);

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

  /** The rail shows surfaces; ⌘K and the tab strips show the pages inside them. */
  const sections = useMemo(
    () =>
      SECTIONS.map((section) => ({
        ...section,
        tabs: visibleTabs(section, isAdmin),
      })).filter((section) => section.tabs.length > 0),
    [isAdmin],
  );
  const nav = useMemo(
    () =>
      sections.flatMap((section) =>
        section.tabs.map((tab) => ({
          to: tab.to,
          label: tab.label,
          icon: section.icon,
          group: section.label,
        })),
      ),
    [sections],
  );

  useEffect(() => {
    const element = navRef.current;
    if (!element) return;
    const update = () =>
      setNavEdges({
        top: element.scrollTop > 2,
        bottom: element.scrollTop + element.clientHeight < element.scrollHeight - 2,
      });
    update();
    element.addEventListener("scroll", update, { passive: true });
    const observer = new ResizeObserver(update);
    observer.observe(element);
    return () => {
      element.removeEventListener("scroll", update);
      observer.disconnect();
    };
  }, [collapsed]);

  const navFade = `linear-gradient(to bottom, transparent 0, black ${
    navEdges.top ? "12px" : "0px"
  }, black calc(100% - ${navEdges.bottom ? "12px" : "0px"}), transparent 100%)`;

  const endpoint = `${window.location.origin}/v1`;
  const running = !health.isError;

  if (adminRequired) {
    return <AdminGate onUnlock={() => setAdminRequired(false)} />;
  }

  return (
    // A fixed shell removes the document's own scroll area: `main` is the only scrollport
    // and `document.scrollHeight` equals the viewport. `h-dvh` alone still let a scrollable
    // `main` report its overflow upward in Chromium (41px of phantom document scroll).
    <div className="fixed inset-0 flex flex-col overflow-hidden min-[820px]:flex-row">
      <a
        href="#main-content"
        className="sr-only focus:not-sr-only focus:fixed focus:left-3 focus:top-3 focus:z-50 focus:rounded-full focus:bg-accent focus:px-4 focus:py-2 focus:text-body focus:font-semibold focus:text-[var(--color-accent-ink)]"
      >
        Skip to content
      </a>
      <aside
        className={cx(
          "hidden shrink-0 flex-col border-r border-line bg-[var(--color-sidebar)] transition-[width] duration-200 min-[820px]:flex",
          collapsed ? "w-[68px]" : "w-[236px]",
        )}
      >
        <div className={cx("flex items-center gap-2 pt-3", collapsed ? "justify-center px-2" : "px-3")}>
          {collapsed ? (
            /* One target in the rail: the logo becomes the expand control on hover. */
            <button
              type="button"
              onClick={toggleCollapsed}
              aria-label="expand sidebar"
              aria-expanded={false}
              title="expand sidebar"
              className="group relative flex h-10 w-10 shrink-0 items-center justify-center rounded-lg hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
            >
              <img
                src={logoOk ? logo : `${import.meta.env.BASE_URL}logo-light.png`}
                alt=""
                width={30}
                height={30}
                className="h-[30px] w-auto max-w-[36px] select-none object-contain transition-opacity group-hover:opacity-0 group-focus-visible:opacity-0"
              />
              <SidebarSimple
                size={17}
                aria-hidden
                className="absolute text-muted opacity-0 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100"
              />
            </button>
          ) : (
            <>
              <img
                src={logoOk ? logo : `${import.meta.env.BASE_URL}logo-light.png`}
                alt=""
                width={30}
                height={30}
                className="h-[30px] w-auto max-w-[52px] shrink-0 select-none object-contain"
              />
              <span className="min-w-0 flex-1 truncate text-lead font-semibold tracking-[-0.01em] text-fg">
                {name}
              </span>
              <button
                type="button"
                onClick={toggleCollapsed}
                aria-label="collapse sidebar"
                aria-expanded
                title="collapse sidebar"
                className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-faint hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
              >
                <SidebarSimple size={16} aria-hidden />
              </button>
            </>
          )}
        </div>

        <div className={collapsed ? "px-2 pb-1 pt-3" : "px-3 pb-2 pt-2"}>
          <JumpButton onClick={() => setPaletteOpen(true)} collapsed={collapsed} />
        </div>

        <nav
          ref={navRef}
          className={cx("nav-scroll mt-2 flex-1 overflow-y-auto pb-3", collapsed ? "px-2" : "px-2")}
          style={{ maskImage: navFade, WebkitMaskImage: navFade }}
          aria-label="Dashboard"
        >
          <ul className="space-y-0.5">
            {sections.map((section) => (
              <li key={section.id}>
                <NavSectionLink section={section} variant="sidebar" collapsed={collapsed} />
              </li>
            ))}
          </ul>
        </nav>

        <UpdateMarker collapsed={collapsed} />
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
            src={logoOk ? logo : `${import.meta.env.BASE_URL}logo-light.png`}
            alt=""
            width={24}
            height={24}
            className="h-6 w-6 shrink-0 select-none object-contain"
          />
          <span className="text-body font-semibold text-fg">{name}</span>
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
        <nav
          ref={tabStrip}
          className="tab-strip flex gap-1 overflow-x-auto px-2 pb-2"
          aria-label="Dashboard"
        >
          {sections.map((section) => (
            <NavSectionLink key={section.id} section={section} variant="tab" />
          ))}
        </nav>
      </header>

      <main
        id="main-content"
        tabIndex={-1}
        className="main-scroll min-h-0 min-w-0 flex-1 overflow-y-auto px-4 py-4 focus:outline-none min-[820px]:px-10 min-[820px]:pb-14 min-[820px]:pt-8"
      >
        <div className="mx-auto w-full max-w-[1160px]">
          {/* Keyed by path, not by query: a filter change inside a page must not remount
              it, while a route change fades the new page in. */}
          <div key={location.pathname} className="page-enter">
            <Suspense fallback={<RouteLoading />}>
              <Outlet />
            </Suspense>
          </div>
        </div>
      </main>

      <CommandPalette open={paletteOpen} onClose={() => setPaletteOpen(false)} nav={nav} />
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
      className="flex h-9 w-full items-center gap-2 rounded-full border border-line-strong bg-surface px-3 text-left text-body text-muted transition-colors hover:bg-surface-3 hover:text-fg"
    >
      <MagnifyingGlass size={14} aria-hidden />
      <span className="truncate">Jump to…</span>
      <kbd className="ml-auto shrink-0 rounded border border-line px-1 py-0.5 font-sans text-caption text-faint">
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
/** A new release is otherwise invisible until someone opens Settings. */
function UpdateMarker({ collapsed }: { collapsed?: boolean }) {
  const admin = useIsAdmin();
  const update = useUpdateStatus(admin);
  const status = update.data;
  if (!admin || !status?.update_available || !status.latest_version) return null;
  const version = `v${status.latest_version}`;
  const preview = (status.notes ?? "")
    .split("\n")
    .map((line) => line.replace(/^#{1,6}\s*/, "").replace(/^[-*]\s+/, "").trim())
    .filter(Boolean)
    .slice(0, 3)
    .join(" · ");
  if (collapsed) {
    return (
      <Link
        to="/system/settings"
        title={`${version} available${preview ? ` — ${preview}` : ""}`}
        aria-label={`${version} update available`}
        className="mx-auto mb-1 flex h-8 w-8 items-center justify-center rounded-full bg-[var(--color-accent)] text-small font-semibold text-[var(--color-accent-ink)]"
      >
        ↑
      </Link>
    );
  }
  return (
    <Link
      to="/system/settings"
      className="mx-3 mb-2 block rounded-[8px] border border-[var(--color-accent)]/30 bg-[var(--color-accent)]/10 px-2.5 py-2 hover:brightness-[1.02]"
    >
      <div className="flex items-center gap-2">
        <span className="text-caption font-semibold text-[var(--color-accent-text)]">{version} available</span>
        <span className="ml-auto text-caption text-faint">review →</span>
      </div>
      {preview ? (
        <p className="mt-1 line-clamp-2 text-caption leading-snug text-muted" title={status.notes ?? undefined}>
          {preview}
        </p>
      ) : null}
    </Link>
  );
}

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
        <Link
          to="/account"
          aria-label="My account"
          title={`${user.name} · ${user.email}`}
          className="flex h-8 w-8 items-center justify-center rounded-full bg-surface-3 text-small font-semibold text-fg hover:bg-surface-2"
        >
          {user.name.slice(0, 1).toUpperCase()}
        </Link>
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
      <Link
        to="/account"
        className="min-w-0 flex-1 rounded-md px-1 py-0.5 hover:bg-surface-3"
      >
        <div className="truncate text-small font-medium text-fg">{user.name}</div>
        <div className="truncate text-caption text-faint">{user.email}</div>
      </Link>
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
        <div className="text-caption font-semibold uppercase tracking-[0.12em] text-faint">Endpoint</div>
        <div className="mt-1 flex min-w-0">
          <CopyChip value={endpoint} label="endpoint" className="w-full" />
        </div>
        <div className="mt-2 flex items-center gap-1.5 text-caption text-muted">
          <RunningDot running={running} />
          <span className="truncate">
            {running ? `Running · up ${uptime(uptimeSecs)}` : "unreachable"}
          </span>
        </div>
        <div className="mt-0.5 truncate text-caption text-faint">
          v{version ?? "—"} · {note?.trim() || "MIT"}
        </div>
      </div>
    </div>
  );
}

/** One sidebar/tab entry for a whole surface. The active state covers every tab. */
function NavSectionLink({
  section,
  variant,
  collapsed = false,
}: {
  section: NavSection;
  variant: "sidebar" | "tab";
  collapsed?: boolean;
}) {
  const location = useLocation();
  const isAdmin = useIsAdmin();
  const active =
    location.pathname === section.to || location.pathname.startsWith(`${section.to}/`);
  return (
    <Link
      to={sectionDefault(section, isAdmin)}
      aria-current={active ? "page" : undefined}
      title={variant === "sidebar" && collapsed ? section.label : undefined}
      aria-label={variant === "sidebar" && collapsed ? section.label : undefined}
      className={cx(
        "relative flex items-center text-body transition-colors",
        variant === "tab"
          ? "h-14 min-w-0 flex-1 flex-col justify-center gap-0.5 rounded-lg px-1 text-caption"
          : collapsed
            ? "justify-center rounded-[8px] px-0 py-2"
            : "gap-2.5 rounded-[8px] px-2.5 py-2",
        variant === "sidebar"
          ? active
            ? "bg-[var(--color-selected)] font-medium text-accent-text"
            : "text-ink-soft hover:bg-surface-3 hover:text-fg"
          : active
            ? "bg-accent font-medium text-[var(--color-accent-ink)]"
            : "text-muted hover:bg-surface-3 hover:text-fg",
      )}
    >
      {active ? (
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
      <span className="shrink-0">{section.icon}</span>
      {collapsed ? null : (
        <span className={variant === "tab" ? "max-w-full truncate" : undefined}>
          {section.label}
        </span>
      )}
    </Link>
  );
}

/** Tab strip for one surface. The page content is the nested route's `<Outlet/>`. */
/** Section root opens the first tab the current role can use. */
export function SectionIndex({ section: id }: { section: SectionId }) {
  const isAdmin = useIsAdmin();
  const section = SECTIONS.find((candidate) => candidate.id === id);
  if (!section) return <Navigate to="/activity" replace />;
  return <Navigate to={sectionDefault(section, isAdmin)} replace />;
}

export function SectionLayout({ section: id }: { section: SectionId }) {
  const isAdmin = useIsAdmin();
  const section = SECTIONS.find((candidate) => candidate.id === id);
  if (!section) return <Outlet />;
  const tabs = visibleTabs(section, isAdmin);
  return (
    <>
      <nav
        aria-label={`${section.label} sections`}
        className="tab-strip mb-4 flex gap-1 overflow-x-auto border-b border-line"
      >
        {tabs.map((tab) => (
          <NavLink
            key={tab.to}
            to={tab.to}
            end={tab.end}
            className={({ isActive }) =>
              cx(
                "relative shrink-0 px-3 py-2 text-body transition-colors",
                isActive
                  ? "font-medium text-accent-text"
                  : "text-muted hover:text-fg",
              )
            }
          >
            {({ isActive }) => (
              <>
                {tab.label}
                {isActive ? (
                  <span
                    aria-hidden
                    className="absolute inset-x-2 bottom-0 h-[2px] rounded-full bg-[var(--color-nav-accent)]"
                  />
                ) : null}
              </>
            )}
          </NavLink>
        ))}
      </nav>
      <Outlet />
    </>
  );
}

function CommandPalette({ open, onClose, nav }: { open: boolean; onClose: () => void; nav: NavItem[] }) {
  const [query, setQuery] = useState("");
  const navigate = useNavigate();
  const models = useModels();

  useEffect(() => {
    if (open) setQuery("");
  }, [open]);

  const results = useMemo(() => {
    const q = query.trim().toLowerCase();
    const pages = nav.filter((n) => !q || n.label.toLowerCase().includes(q)).slice(0, 6);
    const modelHits = (models.data ?? [])
      .filter((m) => q && m.ref.toLowerCase().includes(q))
      .slice(0, 6);
    return { pages, modelHits };
  }, [nav, query, models.data]);

  const go = (path: string) => {
    navigate(path);
    onClose();
  };

  return (
    <Dialog.Root open={open} onOpenChange={(next) => { if (!next) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-50 bg-black/70" />
        <Dialog.Content className="fixed left-1/2 top-[14vh] z-50 w-full max-w-xl -translate-x-1/2 overflow-hidden rounded-[var(--radius-card)] border border-line-strong bg-surface shadow-2xl focus:outline-none">
          <Dialog.Title className="sr-only">Jump to a page or model</Dialog.Title>
          <Dialog.Description className="sr-only">
            Search dashboard pages and models.
          </Dialog.Description>
          <input
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Jump to a page or model…"
            aria-label="Search pages and models"
            className="h-11 w-full border-b border-line bg-transparent px-4 text-lead text-fg placeholder:text-faint focus:outline-none"
          />
          <div className="max-h-[50vh] overflow-y-auto p-2">
            {results.pages.length > 0 ? (
              <Section title="Pages">
                {results.pages.map((p) => (
                  <button
                    key={p.to}
                    onClick={() => go(p.to)}
                    className="flex w-full items-center gap-2.5 rounded-md px-2 py-2 text-left text-body text-muted hover:bg-surface-2 hover:text-fg"
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
                    className="flex w-full items-center justify-between gap-2 rounded-md px-2 py-2 text-left text-body text-muted hover:bg-surface-2 hover:text-fg"
                  >
                    <span className="truncate">{m.ref}</span>
                    <span className="text-caption text-faint">{m.provider}</span>
                  </button>
                ))}
              </Section>
            ) : null}
            {results.pages.length === 0 && results.modelHits.length === 0 ? (
              <div className="px-3 py-6 text-center text-body text-faint">no matches</div>
            ) : null}
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="mb-1">
      <div className="px-2 py-1 text-caption uppercase tracking-[0.1em] text-faint">{title}</div>
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
            src={`${import.meta.env.BASE_URL}logo-light.png`}
            alt=""
            width={26}
            height={26}
            className="h-[26px] w-[26px]"
          />
          <span className="text-lead font-semibold">Admin token required</span>
        </div>
        <p className="mt-2 text-body text-muted">
          This gateway was started with <span className="font-mono text-small">RROUTER_ADMIN_TOKEN</span>.
          Paste it to manage providers, keys and models from this browser. It is kept for this
          tab only.
        </p>
        <label className="mt-5 flex flex-col gap-1.5">
          <span className="text-caption font-medium uppercase tracking-[0.08em] text-faint">Token</span>
          <input
            type="password"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            autoFocus
            aria-label="admin token"
            className="h-9 w-full rounded-lg border border-line bg-surface-2 px-2.5 font-mono text-body text-fg placeholder:text-faint focus:border-[var(--color-accent)]/60 focus:outline-none"
            placeholder="the value of RROUTER_ADMIN_TOKEN"
          />
        </label>
        {error ? <p className="mt-3 text-small text-neg">{error}</p> : null}
        <div className="mt-5 flex items-center justify-between gap-3">
          <a href="/health" className="text-small text-faint underline underline-offset-4 hover:text-fg">
            gateway health
          </a>
          <button
            type="submit"
            disabled={busy || token.trim().length === 0}
            className="inline-flex h-9 items-center rounded-lg bg-accent px-4 text-body font-semibold text-[var(--color-accent-ink)] disabled:opacity-40"
          >
            {busy ? "Checking…" : "Unlock"}
          </button>
        </div>
      </form>
    </div>
  );
}

/** Route chunk fallback. A chunk that never resolves must not trap the tab on one word. */
function RouteLoading() {
  const [slow, setSlow] = useState(false);
  useEffect(() => {
    const timer = window.setTimeout(() => setSlow(true), 8_000);
    return () => window.clearTimeout(timer);
  }, []);
  return (
    <div className="flex flex-col items-start gap-2 text-body text-faint" role="status">
      <span>loading…</span>
      {slow ? (
        <span className="text-small">
          This is taking longer than expected.{" "}
          <button
            type="button"
            onClick={() => window.location.reload()}
            className="underline underline-offset-2 hover:text-fg"
          >
            Reload the dashboard
          </button>
          .
        </span>
      ) : null}
    </div>
  );
}
