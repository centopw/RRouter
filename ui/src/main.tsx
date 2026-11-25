import { Component, StrictMode, lazy, type ComponentType, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createBrowserRouter,
  isRouteErrorResponse,
  Navigate,
  RouterProvider,
  useLocation,
  useRouteError,
} from "react-router-dom";
import "./index.css";
import { SectionLayout, Shell } from "./app";
import { ToastProvider } from "./components/toast";
import { useIsAdmin } from "./lib/queries";
import { AppearanceProvider } from "./lib/appearance";
import { AuthGuard, RequireAdmin } from "./pages/auth/guard";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: 1,
      refetchOnWindowFocus: false,
      staleTime: 5_000,
    },
  },
});

/**
 * A rebuilt binary ships new asset hashes, so a tab opened before the deploy asks for
 * chunks that no longer exist. One reload fetches the new entry point; a flag keeps a
 * genuinely broken build from looping.
 */
const RELOAD_FLAG = "rrouter:chunk-reloaded";

function isStaleChunk(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return /dynamically imported module|Importing a module script failed|ChunkLoadError/i.test(message);
}

function page(load: () => Promise<{ default: ComponentType }>) {
  return lazy(async () => {
    try {
      return await load();
    } catch (error) {
      if (isStaleChunk(error) && sessionStorage.getItem(RELOAD_FLAG) !== "1") {
        sessionStorage.setItem(RELOAD_FLAG, "1");
        window.location.reload();
      }
      throw error;
    }
  });
}

// Route-level chunks: the shell and Activity land first, the rest on demand.
const Activity = page(() => import("./pages/activity"));
const Health = page(() => import("./pages/health"));
const SystemLogs = page(() => import("./pages/system-logs"));
const Webhooks = page(() => import("./pages/webhooks"));
const Keys = page(() => import("./pages/keys"));
const QuickStart = page(() => import("./pages/quickstart"));
const Aliases = page(() => import("./pages/aliases"));
const Combos = page(() => import("./pages/combos"));
const Compression = page(() => import("./pages/compression"));
const CapabilitySearch = page(() => import("./pages/capability/search"));
const CapabilityRerank = page(() => import("./pages/capability/rerank"));
const CapabilityEmbeddings = page(() => import("./pages/capability/embeddings"));
const Templates = page(() => import("./pages/templates"));
const Evals = page(() => import("./pages/evals"));
const Models = page(() => import("./pages/models"));
const Presets = page(() => import("./pages/presets"));
const Playground = page(() => import("./pages/playground"));
const Usage = page(() => import("./pages/usage"));
const SettingsLayout = page(() => import("./pages/settings/layout"));
const SettingsGeneral = page(() => import("./pages/settings/general"));
const SettingsAppearance = page(() => import("./pages/settings/appearance"));
const SettingsRuntime = page(() => import("./pages/settings/runtime"));
const SettingsProfile = page(() => import("./pages/settings/profile"));
const SettingsSecurity = page(() => import("./pages/settings/security"));
const SettingsSessions = page(() => import("./pages/settings/sessions"));
const Providers = page(() => import("./pages/providers"));
const FreeMode = page(() => import("./pages/free-mode"));
const Setup = page(() => import("./pages/setup"));
const Users = page(() => import("./pages/users"));
const Teams = page(() => import("./pages/teams"));
const Audit = page(() => import("./pages/audit"));
const Topology = page(() =>
  import("./components/live-routing").then((m) => ({ default: m.LiveRoutingCard })),
);
const SignIn = page(() => import("./pages/auth/sign-in"));
const Register = page(() => import("./pages/auth/register"));
const TokenSignIn = page(() => import("./pages/auth/token-signin"));
const SetupAdmin = page(() => import("./pages/auth/setup-admin"));
const Forgot = page(() => import("./pages/auth/forgot"));
const Invite = page(() => import("./pages/auth/invite"));

/** An admin page: hidden from a reader's nav, and redirected here on a typed URL. */
function AdminPage({ children }: { children: ReactNode }) {
  return <RequireAdmin>{children}</RequireAdmin>;
}

/** A surface whose root has no screen of its own: open its first role-visible tab. */
function TabHome({ admin, member }: { admin: string; member: string }) {
  return <Navigate to={useIsAdmin() ? admin : member} replace />;
}

/**
 * Old URLs keep working, query string included: a bookmark or a palette hit carries
 * filter state a bare `<Navigate>` would drop. `map` rewrites params the new surface
 * spells differently, so a stale link still lands on the intended filter.
 */
function Legacy({ to, map }: { to: string; map?: (params: URLSearchParams) => void }) {
  const { search } = useLocation();
  const next = new URLSearchParams(search);
  map?.(next);
  return <Navigate to={{ pathname: to, search: next.toString() }} replace />;
}

/** The retired `/system/diagnostics` tab strip: `?tab=logs` was its logs tab. */
function DiagnosticsRedirect() {
  const { search } = useLocation();
  return <Navigate to={search.includes("tab=logs") ? "/system/logs" : "/system/health"} replace />;
}

/** `/routing/providers?custom=openai` predates the custom-endpoint form's own params. */
function customEndpointParams(params: URLSearchParams) {
  const protocol = params.get("custom");
  if (protocol !== "openai" && protocol !== "anthropic") return;
  params.delete("custom");
  params.set("add", "custom");
  params.set("protocol", protocol);
}

/**
 * A tab left open across a deploy keeps the old bundle in memory; no HTTP header can
 * replace JavaScript that is already running. Compare the loaded entry chunk with the one
 * the server currently serves, and reload once when they differ. Dev mode has no hashed
 * entry, so the check is a no-op there.
 */
function watchForNewBuild() {
  const loaded = document.querySelector('script[type="module"][src]')?.getAttribute("src") ?? "";
  const loadedChunk = /assets\/index-[^/]+\.js/.exec(loaded)?.[0];
  if (!loadedChunk) return;
  const CHECK_KEY = "rrouter:build-check";
  const RELOADED_KEY = "rrouter:build-reloaded-at";
  let last = 0;
  const check = async () => {
    const now = Date.now();
    if (now - last < 60_000) return;
    last = now;
    try {
      const response = await fetch(`${import.meta.env.BASE_URL}index.html`, { cache: "no-store" });
      if (!response.ok) return;
      const served = /assets\/index-[^/]+\.js/.exec(await response.text())?.[0];
      if (!served || served === loadedChunk) return;
      const reloadedAt = Number(sessionStorage.getItem(RELOADED_KEY) ?? "0");
      if (Date.now() - reloadedAt < 10 * 60_000) return;
      sessionStorage.setItem(RELOADED_KEY, String(Date.now()));
      window.location.reload();
    } catch {
      // Offline or a transient failure: try again on the next focus/tick.
    }
  };
  window.addEventListener("focus", () => void check());
  window.setInterval(() => void check(), 5 * 60_000);
  void check();
  sessionStorage.setItem(CHECK_KEY, String(Date.now()));
}

const router = createBrowserRouter(
  [
    // Signed-out surfaces. They own their chrome: no sidebar.
    { path: "setup/admin", element: <SetupAdmin />, errorElement: <RouteError /> },
    { path: "sign-in", element: <SignIn />, errorElement: <RouteError /> },
    { path: "sign-in/token", element: <TokenSignIn />, errorElement: <RouteError /> },
    { path: "register", element: <Register />, errorElement: <RouteError /> },
    { path: "forgot", element: <Forgot />, errorElement: <RouteError /> },
    { path: "invite/:token", element: <Invite />, errorElement: <RouteError /> },
    // The wizard and the app both need a signed-in admin.
    {
      path: "setup",
      element: (
        <AuthGuard>
          <RequireAdmin>
            <Setup />
          </RequireAdmin>
        </AuthGuard>
      ),
      errorElement: <RouteError />,
    },
    {
      path: "/",
      element: (
        <AuthGuard>
          <AppearanceProvider>
            <Shell />
          </AppearanceProvider>
        </AuthGuard>
      ),
      errorElement: <RouteError />,
      children: [
        {
          // Pathless layout: a failure renders inside the shell, so the sidebar and
          // the rest of the dashboard stay usable.
          errorElement: <RouteError />,
          children: [
            { index: true, element: <Navigate to="/activity" replace /> },
            {
              path: "activity",
              children: [
                { index: true, element: <Activity /> },
                // Logs and audit have canonical homes now; the old tabs stay as redirects.
                { path: "logs", element: <Navigate to="/system/logs" replace /> },
                { path: "audit", element: <Legacy to="/access/audit" /> },
              ],
            },
            { path: "usage", element: <Usage /> },
            { path: "compression", element: <AdminPage><Compression /></AdminPage> },
            { path: "capability/search", element: <AdminPage><CapabilitySearch /></AdminPage> },
            { path: "capability/rerank", element: <AdminPage><CapabilityRerank /></AdminPage> },
            {
              path: "capability/embeddings",
              element: <AdminPage><CapabilityEmbeddings /></AdminPage>,
            },
            {
              path: "playground",
              element: <SectionLayout surface="playground" />,
              children: [
                {
                  index: true,
                  element: <TabHome admin="/playground/chat" member="/playground/templates" />,
                },
                { path: "chat", element: <AdminPage><Playground /></AdminPage> },
                { path: "templates", element: <Templates /> },
                { path: "evals", element: <Evals /> },
              ],
            },
            { path: "models", element: <Models /> },
            { path: "providers", element: <Providers /> },
            { path: "free-mode", element: <AdminPage><FreeMode /></AdminPage> },
            { path: "quickstart", element: <QuickStart /> },
            {
              path: "routing",
              element: <SectionLayout surface="routing" />,
              children: [
                {
                  index: true,
                  element: <TabHome admin="/routing/presets" member="/routing/topology" />,
                },
                { path: "presets", element: <AdminPage><Presets /></AdminPage> },
                { path: "aliases", element: <AdminPage><Aliases /></AdminPage> },
                { path: "combos", element: <AdminPage><Combos /></AdminPage> },
                { path: "topology", element: <Topology /> },
              ],
            },
            {
              path: "access",
              children: [
                { index: true, element: <Navigate to="/access/teams" replace /> },
                { path: "keys", element: <AdminPage><Keys /></AdminPage> },
                { path: "quickstart", element: <Navigate to="/quickstart" replace /> },
                { path: "teams", element: <Teams /> },
                { path: "users", element: <AdminPage><Users /></AdminPage> },
                { path: "audit", element: <AdminPage><Audit /></AdminPage> },
              ],
            },
            {
              path: "system",
              children: [
                { index: true, element: <Navigate to="/system/health" replace /> },
                { path: "health", element: <AdminPage><Health /></AdminPage> },
                { path: "webhooks", element: <AdminPage><Webhooks /></AdminPage> },
                { path: "logs", element: <AdminPage><SystemLogs /></AdminPage> },
                { path: "settings", element: <Legacy to="/settings/general" /> },
              ],
            },
            {
              path: "settings",
              element: <SettingsLayout />,
              children: [
                { index: true, element: <Navigate to="/settings/general" replace /> },
                { path: "general", element: <AdminPage><SettingsGeneral /></AdminPage> },
                { path: "appearance", element: <SettingsAppearance /> },
                { path: "runtime", element: <AdminPage><SettingsRuntime /></AdminPage> },
                { path: "profile", element: <SettingsProfile /> },
                { path: "security", element: <SettingsSecurity /> },
                { path: "sessions", element: <SettingsSessions /> },
              ],
            },
            { path: "account", element: <Legacy to="/settings/profile" /> },
            // Legacy URLs keep working, filters included; `/models` and `/providers` are
            // canonical, so only the old surfaces redirect.
            { path: "logs", element: <Navigate to="/system/logs" replace /> },
            { path: "audit", element: <Legacy to="/access/audit" /> },
            { path: "topology", element: <Legacy to="/routing/topology" /> },
            { path: "templates", element: <Navigate to="/playground/templates" replace /> },
            { path: "evals", element: <Navigate to="/playground/evals" replace /> },
            { path: "routing/models", element: <Legacy to="/models" /> },
            {
              path: "routing/providers",
              element: <Legacy to="/providers" map={customEndpointParams} />,
            },
            { path: "connections", element: <Legacy to="/providers" /> },
            { path: "aliases", element: <Legacy to="/routing/aliases" /> },
            { path: "combos", element: <Legacy to="/routing/combos" /> },
            { path: "keys", element: <Navigate to="/access/keys" replace /> },
            { path: "teams", element: <Navigate to="/access/teams" replace /> },
            { path: "users", element: <Navigate to="/access/users" replace /> },
            // Diagnostics split back into Health and System logs; a bookmarked tab picks its half.
            { path: "diagnostics", element: <DiagnosticsRedirect /> },
            { path: "system/diagnostics", element: <DiagnosticsRedirect /> },
            { path: "health", element: <Navigate to="/system/health" replace /> },
            { path: "webhooks", element: <Navigate to="/system/webhooks" replace /> },
            { path: "system-logs", element: <Navigate to="/system/logs" replace /> },
            { path: "*", element: <NotFound /> },
          ],
        },
      ],
    },
  ],
  { basename: import.meta.env.BASE_URL.replace(/\/$/, "") || "/" },
);

/** Route errors keep the shell usable: the sidebar stays, the page explains itself. */
function RouteError() {
  const error = useRouteError();
  const stale = isStaleChunk(error);
  const detail = isRouteErrorResponse(error)
    ? `${error.status} ${error.statusText}`
    : error instanceof Error
      ? error.message
      : String(error);

  return (
    <div className="mx-auto max-w-[560px] py-16 text-center">
      <h1 className="text-subtitle font-bold">{stale ? "This dashboard was updated" : "This page failed to load"}</h1>
      <p className="mt-2 text-body text-muted">
        {stale
          ? "A newer build is running. Reload to pick up its files."
          : "The page could not be rendered. The rest of the dashboard still works."}
      </p>
      <pre className="mt-4 max-h-40 overflow-auto rounded-lg border border-line bg-surface-2 p-3 text-left font-mono text-caption text-faint">
        {detail}
      </pre>
      <div className="mt-5 flex justify-center gap-2">
        <button
          onClick={() => {
            sessionStorage.removeItem(RELOAD_FLAG);
            window.location.reload();
          }}
          className="inline-flex h-8 items-center gap-1.5 rounded-lg bg-accent px-3 text-body font-semibold text-[var(--color-accent-ink)] hover:brightness-110"
        >
          reload
        </button>
        <a
          href="/app/activity"
          className="inline-flex h-8 items-center rounded-lg border border-line bg-surface-2 px-3 text-body text-fg hover:border-line-strong"
        >
          go to activity
        </a>
      </div>
    </div>
  );
}

function NotFound() {
  return (
    <div className="text-body text-muted">
      No such page.{" "}
      <a className="underline" href="/app/activity">
        Back to activity
      </a>
      .
    </div>
  );
}

/** Last line of defence: a crash outside the router still renders something usable. */
class FatalBoundary extends Component<{ children: ReactNode }, { error: Error | null }> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="grid min-h-full place-items-center px-6">
        <div className="max-w-[520px] text-center">
          <h1 className="text-subtitle font-bold">The dashboard crashed</h1>
          <p className="mt-2 text-body text-muted">{this.state.error.message}</p>
          <button
            onClick={() => window.location.reload()}
            className="mt-5 inline-flex h-8 items-center rounded-lg bg-accent px-3 text-body font-semibold text-[var(--color-accent-ink)]"
          >
            reload
          </button>
        </div>
      </div>
    );
  }
}

watchForNewBuild();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <FatalBoundary>
      <QueryClientProvider client={queryClient}>
        <ToastProvider>
          <RouterProvider router={router} />
        </ToastProvider>
      </QueryClientProvider>
    </FatalBoundary>
  </StrictMode>,
);

// A successful load means the tab is on current assets again; allow the next deploy to
// trigger its own one-shot reload.
setTimeout(() => sessionStorage.removeItem(RELOAD_FLAG), 5_000);
