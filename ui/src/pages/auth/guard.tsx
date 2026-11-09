import { useEffect, type ReactNode } from "react";
import { Navigate, useLocation, useNavigate } from "react-router-dom";
import { adminToken, SIGN_IN_REQUIRED_EVENT } from "../../lib/api";
import { useAuthState, useIsAdmin } from "../../lib/queries";
import { AuthNotice, AuthShell } from "../../components/auth-form";

/** Where a signed-out operator is sent, preserving the page they wanted. */
function signInPath(location: { pathname: string; search: string }): string {
  const next = `${location.pathname}${location.search}`;
  return `/sign-in?next=${encodeURIComponent(next)}`;
}

/**
 * The dashboard's door. It reads the unauthenticated auth state and routes the three
 * first-run cases: no users at all → the setup screen, a user without a session → sign
 * in, a session of any role → the shell.
 *
 * Roles are not decided here: every page a reader may not use is either absent from the
 * nav or wrapped in [`RequireAdmin`], and the API refuses it regardless of what is
 * painted.
 */
export function AuthGuard({ children }: { children: ReactNode }) {
  const state = useAuthState();
  const location = useLocation();
  const navigate = useNavigate();

  useEffect(() => {
    const toSignIn = () => navigate(signInPath(location), { replace: true });
    window.addEventListener(SIGN_IN_REQUIRED_EVENT, toSignIn);
    return () => window.removeEventListener(SIGN_IN_REQUIRED_EVENT, toSignIn);
  }, [navigate, location]);

  if (state.isLoading) {
    return (
      <AuthShell footer={false}>
        <p className="text-center text-body text-muted">Loading…</p>
      </AuthShell>
    );
  }
  if (state.isError) {
    return (
      <AuthShell>
        <AuthNotice>Cannot reach the gateway. Check that RRouter is running.</AuthNotice>
      </AuthShell>
    );
  }
  const data = state.data;
  if (!data) return null;
  if (!data.initialized) return <Navigate to="/setup/admin" replace />;
  // A stored admin token is break-glass access: the API accepts it, so let the shell
  // render even though no user session exists.
  if (!data.user && !adminToken()) return <Navigate to={signInPath(location)} replace />;
  return <>{children}</>;
}

/**
 * Page guard for the admin-only routes. The nav already hides them, so this covers a
 * typed URL or a bookmark: a reader lands on Activity instead of a page whose first
 * request would be refused.
 */
export function RequireAdmin({ children }: { children: ReactNode }) {
  const state = useAuthState();
  const admin = useIsAdmin();
  if (state.isLoading) return null;
  if (!admin) return <Navigate to="/activity" replace />;
  return <>{children}</>;
}
