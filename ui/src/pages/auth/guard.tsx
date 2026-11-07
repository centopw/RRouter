import { useEffect, type ReactNode } from "react";
import { Navigate, useLocation, useNavigate } from "react-router-dom";
import { adminToken, SIGN_IN_REQUIRED_EVENT } from "../../lib/api";
import { useAuthState, useSignOut } from "../../lib/queries";
import { AuthCard, AuthNotice, AuthShell, AuthTitle } from "../../components/auth-form";
import { Button } from "../../components/ui";

/** Where a signed-out operator is sent, preserving the page they wanted. */
function signInPath(location: { pathname: string; search: string }): string {
  const next = `${location.pathname}${location.search}`;
  return `/sign-in?next=${encodeURIComponent(next)}`;
}

/**
 * The dashboard's door. It reads the unauthenticated auth state and routes the three
 * first-run cases: no users at all → the setup screen, a user without a session → sign
 * in, a non-admin session → a plain "this account is limited" card.
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
  if (data.user && data.user.role !== "admin") return <LimitedAccount />;
  return <>{children}</>;
}

/** Members and viewers can sign in but the admin API refuses them; say so plainly. */
function LimitedAccount() {
  const state = useAuthState();
  const signOut = useSignOut();
  const navigate = useNavigate();
  return (
    <AuthShell>
      <AuthCard>
        <AuthTitle
          title="This account cannot administer the gateway"
          lead={`Signed in as ${state.data?.user?.email ?? "a limited account"}. Ask an administrator for the admin role, or sign out to use another account.`}
        />
        <Button
          variant="primary"
          className="w-full"
          onClick={() => {
            signOut.mutate(undefined, { onSettled: () => navigate("/sign-in", { replace: true }) });
          }}
        >
          Sign out
        </Button>
      </AuthCard>
    </AuthShell>
  );
}
