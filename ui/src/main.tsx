import { StrictMode, lazy } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createBrowserRouter, Navigate, RouterProvider } from "react-router-dom";
import "./index.css";
import { Shell } from "./app";
// Route-level chunks: the shell and Activity land first, the rest on demand.
const Activity = lazy(() => import("./pages/activity"));
const Health = lazy(() => import("./pages/health"));
const Models = lazy(() => import("./pages/models"));
const Providers = lazy(() => import("./pages/providers"));
const Keys = lazy(() => import("./pages/keys"));
const Aliases = lazy(() => import("./pages/aliases"));
const Combos = lazy(() => import("./pages/combos"));

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: 1,
      refetchOnWindowFocus: false,
      staleTime: 5_000,
    },
  },
});

const router = createBrowserRouter(
  [
    {
      path: "/",
      element: <Shell />,
      children: [
        { index: true, element: <Navigate to="/activity" replace /> },
        { path: "activity", element: <Activity /> },

        { path: "providers", element: <Providers /> },
        { path: "connections", element: <Navigate to="/providers" replace /> },
        { path: "models", element: <Models /> },
        { path: "keys", element: <Keys /> },
        { path: "aliases", element: <Aliases /> },
        { path: "combos", element: <Combos /> },
        { path: "health", element: <Health /> },
        { path: "*", element: <NotFound /> },
      ],
    },
  ],
  { basename: import.meta.env.BASE_URL.replace(/\/$/, "") || "/" },
);

function NotFound() {
  return (
    <div className="text-[13px] text-muted">
      No such page. <a className="underline" href="/app/activity">Back to activity</a>.
    </div>
  );
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
);
