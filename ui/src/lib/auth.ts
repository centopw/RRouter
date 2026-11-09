/** Dashboard identity: the signed-out auth surface and user administration. */
import { api } from "./api";

export type UserRole = "admin" | "member" | "viewer";

export type AuthUser = {
  id: string;
  name: string;
  email: string;
  role: UserRole;
  is_owner: boolean;
  disabled: boolean;
  created_at: number;
  last_sign_in_at: number | null;
};

export type AuthState = {
  initialized: boolean;
  setup_code_required: boolean;
  signup: "invite_only";
  user: AuthUser | null;
};

export type InviteMeta = {
  email: string;
  role: UserRole;
  inviter_name: string;
  expires_at: number;
};

export type Invitation = {
  id: string;
  email: string;
  role: UserRole;
  created_at: number;
  expires_at: number;
  used_at: number | null;
};

export type Credentials = { user: AuthUser; recovery_codes: string[] };

export function authState(): Promise<AuthState> {
  return api.get<AuthState>("/api/auth/state");
}

export function setupAdmin(input: {
  setup_code: string;
  name: string;
  email: string;
  password: string;
}): Promise<Credentials> {
  return api.post<Credentials>("/api/auth/setup", input);
}

export function signIn(input: {
  email: string;
  password: string;
  remember: boolean;
}): Promise<{ user: AuthUser }> {
  return api.post<{ user: AuthUser }>("/api/auth/login", input);
}

export function signOut(): Promise<void> {
  return api.post<void>("/api/auth/logout");
}

export function recover(input: {
  email: string;
  recovery_code: string;
  new_password: string;
}): Promise<void> {
  return api.post<void>("/api/auth/recover", input);
}

export function inviteMeta(token: string): Promise<InviteMeta> {
  return api.get<InviteMeta>(`/api/auth/invites/${encodeURIComponent(token)}`);
}

export function signUp(input: {
  token: string;
  name: string;
  password: string;
}): Promise<Credentials> {
  return api.post<Credentials>("/api/auth/signup", input);
}

export async function listUsers(): Promise<AuthUser[]> {
  const body = await api.get<{ users?: AuthUser[] }>("/api/users");
  return body.users ?? [];
}

export function updateUser(
  id: string,
  patch: Partial<Pick<AuthUser, "role" | "disabled">>,
): Promise<{ user: AuthUser }> {
  return api.put<{ user: AuthUser }>(`/api/users/${encodeURIComponent(id)}`, patch);
}

export function removeUser(id: string): Promise<void> {
  return api.del<void>(`/api/users/${encodeURIComponent(id)}`);
}

export function createInvite(input: {
  email: string;
  role: UserRole;
}): Promise<{ invite: Invitation; token: string; path: string }> {
  return api.post("/api/users/invites", input);
}

export async function listInvites(): Promise<Invitation[]> {
  const body = await api.get<{ invites?: Invitation[] }>("/api/users/invites");
  return body.invites ?? [];
}

export function revokeInvite(id: string): Promise<void> {
  return api.del<void>(`/api/users/invites/${encodeURIComponent(id)}`);
}

/** Password rules shared by every set-password screen. */
export const MIN_PASSWORD = 12;

export function passwordStrength(value: string): { filled: number; label: string } {
  const length = value.length;
  const filled = length >= 16 ? 4 : length >= 12 ? 3 : length >= 8 ? 2 : length > 0 ? 1 : 0;
  const label = length >= 16 ? "Strong" : length >= 12 ? "Good" : length >= 8 ? "Fair" : "Too short";
  return { filled, label };
}

/** The machine-readable `error.code` of an admin API failure, or "". */
export function apiCode(error: unknown): string {
  const body = (error as { body?: unknown })?.body;
  if (typeof body === "object" && body !== null && "error" in body) {
    const inner = (body as { error?: { code?: unknown } }).error;
    if (inner && typeof inner.code === "string") return inner.code;
  }
  return "";
}

export type AuditEvent = {
  id: number;
  ts: number;
  kind: string;
  actor: string | null;
  ip: string | null;
  detail: string | null;
};

export async function listAudit(kind?: string, before?: number): Promise<AuditEvent[]> {
  const params = new URLSearchParams({ limit: "100" });
  if (kind) params.set("kind", kind);
  if (before) params.set("before", String(before));
  const body = await api.get<{ data?: AuditEvent[] }>(`/api/audit?${params}`);
  return body.data ?? [];
}

export type SessionRow = {
  id: string;
  created_at: number;
  expires_at: number;
  current: boolean;
  /**
   * Address that created the session, as the gateway received it. `null` for rows written
   * before schema v28, which stored no session metadata.
   */
  ip: string | null;
  /** Raw `User-Agent`, bounded by the gateway. `null` for legacy rows. */
  user_agent: string | null;
  /** A short label the gateway derives from the user agent, e.g. "Chrome on macOS". */
  device_label: string | null;
};

export type Me = { user: AuthUser; sessions: SessionRow[] };

export function getMe(): Promise<Me> {
  return api.get<Me>("/api/auth/me");
}

export function updateMe(patch: {
  name?: string;
  current_password?: string;
  new_password?: string;
}): Promise<{ user: AuthUser }> {
  return api.put<{ user: AuthUser }>("/api/auth/me", patch);
}

export function revokeSession(id: string): Promise<void> {
  return api.del<void>(`/api/auth/me/sessions/${encodeURIComponent(id)}`);
}

export function revokeOtherSessions(): Promise<{ revoked: number }> {
  return api.post<{ revoked: number }>("/api/auth/me/sessions/revoke-others");
}
