import { Component, StrictMode, lazy, type ComponentType, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createBrowserRouter,
  isRouteErrorResponse,
  Navigate,
  RouterProvider,
  useRouteError,
} from "react-router-dom";
import "./index.css";
import { Shell } from "./app";
import { ToastProvider } from "./components/toast";
import { AuthGuard } from "./pages/auth/guard";

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
const Keys = page(() => import("./pages/keys"));
const Aliases = page(() => import("./pages/aliases"));
const Combos = page(() => import("./pages/combos"));
const Templates = page(() => import("./pages/templates"));
const Models = page(() => import("./pages/models"));
const Playground = page(() => import("./pages/playground"));
const Settings = page(() => import("./pages/settings"));
const Providers = page(() => import("./pages/providers"));
const Setup = page(() => import("./pages/setup"));
const Logs = page(() => import("./pages/logs"));
const Users = page(() => import("./pages/users"));
const Audit = page(() => import("./pages/audit"));
const Account = page(() => import("./pages/account"));
const SignIn = page(() => import("./pages/auth/sign-in"));
const Register = page(() => import("./pages/auth/register"));
const TokenSignIn = page(() => import("./pages/auth/token-signin"));
const SetupAdmin = page(() => import("./pages/auth/setup-admin"));
const Forgot = page(() => import("./pages/auth/forgot"));
const Invite = page(() => import("./pages/auth/invite"));

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
          <Setup />
        </AuthGuard>
      ),
      errorElement: <RouteError />,
    },
    {
      path: "/",
      element: (
        <AuthGuard>
          <Shell />
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
            { path: "activity", element: <Activity /> },
            { path: "logs", element: <Logs /> },
            { path: "providers", element: <Providers /> },
            { path: "connections", element: <Navigate to="/providers" replace /> },
            { path: "models", element: <Models /> },
            { path: "playground", element: <Playground /> },
            { path: "templates", element: <Templates /> },
            { path: "settings", element: <Settings /> },
            { path: "keys", element: <Keys /> },
            { path: "users", element: <Users /> },
            { path: "audit", element: <Audit /> },
            { path: "account", element: <Account /> },
            { path: "aliases", element: <Aliases /> },
            { path: "combos", element: <Combos /> },
            { path: "health", element: <Health /> },
            { path: "system-logs", element: <SystemLogs /> },
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
      <h1 className="text-[20px] font-bold">{stale ? "This dashboard was updated" : "This page failed to load"}</h1>
      <p className="mt-2 text-[13px] text-muted">
        {stale
          ? "A newer build is running. Reload to pick up its files."
          : "The page could not be rendered. The rest of the dashboard still works."}
      </p>
      <pre className="mt-4 max-h-40 overflow-auto rounded-lg border border-line bg-surface-2 p-3 text-left font-mono text-[11px] text-faint">
        {detail}
      </pre>
      <div className="mt-5 flex justify-center gap-2">
        <button
          onClick={() => {
            sessionStorage.removeItem(RELOAD_FLAG);
            window.location.reload();
          }}
          className="inline-flex h-8 items-center gap-1.5 rounded-lg bg-accent px-3 text-[13px] font-semibold text-[var(--color-accent-ink)] hover:brightness-110"
        >
          reload
        </button>
        <a
          href="/app/activity"
          className="inline-flex h-8 items-center rounded-lg border border-line bg-surface-2 px-3 text-[13px] text-fg hover:border-line-strong"
        >
          go to activity
        </a>
      </div>
    </div>
  );
}

function NotFound() {
  return (
    <div className="text-[13px] text-muted">
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
          <h1 className="text-[20px] font-bold">The dashboard crashed</h1>
          <p className="mt-2 text-[13px] text-muted">{this.state.error.message}</p>
          <button
            onClick={() => window.location.reload()}
            className="mt-5 inline-flex h-8 items-center rounded-lg bg-accent px-3 text-[13px] font-semibold text-[var(--color-accent-ink)]"
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
