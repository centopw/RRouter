import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  CopyChip,
  Dot,
  ErrorNote,
  Field,
  Input,
  Modal,
  Segmented,
  Select,
  PageHeader,
  ConfirmDialog,
  cx,
} from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { ModelPicker } from "../components/model-picker";
import { Checkbox, IconButton } from "../components/atoms";
import { PencilSimple, Trash } from "@phosphor-icons/react";
import { compact, parseUsd, usd, usdInput, when } from "../lib/format";
import {
  qk,
  useKeyModels,
  useKeys,
  useModels,
  useSystem,
  useTeams,
  useUpdateKey,
  useUpdateKeyModels,
} from "../lib/queries";
import {
  ApiError,
  api,
  type BudgetWindow,
  type Guardrails,
  type KeyModelMap,
  type KeyResult,
  type KeyScope,
  type NewKey,
  type UpdateKey,
} from "../lib/api";

export default function Keys() {
  const keys = useKeys();
  const system = useSystem();
  // Team names for the badge; the picker inside the dialogs reads the same list.
  const teams = useTeams();
  const teamName = (id: string | null | undefined) =>
    id === null || id === undefined
      ? null
      : (teams.data?.find((team) => team.id === id)?.name ?? id.slice(0, 8));
  const client = useQueryClient();
  const [editing, setEditing] = useState<KeyResult | null>(null);
  const [creating, setCreating] = useState(false);
  const [revoking, setRevoking] = useState<KeyResult | null>(null);

  // The Modal re-focuses itself whenever `onClose` changes identity; keep it stable.
  const closeEdit = useCallback(() => setEditing(null), []);


  const remove = useMutation({
    mutationFn: (id: string) => api.del<{ deleted: boolean }>(`/api/keys/${id}`),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: qk.keys });
      void client.invalidateQueries({ queryKey: qk.system });
    },
  });

  const columns: Array<Column<KeyResult>> = [
    {
      key: "key",
      title: "Key",
      width: "42%",
      className: "min-w-0",
      render: (r) => <KeyCell row={r} team={teamName(r.team_id)} />,
    },
    {
      key: "limits",
      title: "Limits",
      className: "min-w-0",
      render: (r) => <Limits row={r} />,
    },
    {
      key: "spend",
      title: "Spend",
      width: "9.5rem",
      className: "hidden md:table-cell",
      render: (r) => <Spend row={r} />,
    },
    {
      key: "actions",
      title: "Actions",
      align: "right",
      width: "5rem",
      render: (r) => (
        <span className="flex justify-end gap-0.5">
          <IconButton
            label={`Edit key settings for ${r.name ?? r.id}`}
            onClick={() => setEditing(r)}
          >
            <PencilSimple size={15} aria-hidden />
          </IconButton>
          <IconButton
            label={`Delete key ${r.name ?? r.id}`}
            onClick={() => setRevoking(r)}
            className="hover:bg-[var(--color-neg-tint)] hover:text-[var(--color-neg-ink)]"
          >
            <Trash size={15} aria-hidden />
          </IconButton>
        </span>
      ),
    },
  ];

  return (
    <>
      <ConfirmDialog
        open={revoking !== null}
        title="Revoke key"
        message={<>Every client using <span className="font-mono">{revoking?.name ?? revoking?.id}</span> stops working immediately. This cannot be undone.</>}
        confirmLabel="Revoke key"
        busy={remove.isPending}
        onConfirm={() => {
          if (revoking) remove.mutate(revoking.id);
          setRevoking(null);
        }}
        onClose={() => setRevoking(null)}
      />

      <PageHeader
        title="API Keys"
        subtitle="Clients authenticate with these against /v1. Keys are stored hashed; the plaintext is shown once."
        actions={
          <Button variant="primary" onClick={() => setCreating(true)}>
            create key
          </Button>
        }
      />

      {keys.isError ? (
        <div className="mb-4">
          <ErrorNote error={keys.error} onRetry={() => void keys.refetch()} />
        </div>
      ) : null}

      <Card>
        <CardHeader
          title="Keys"
          action={
            <span className="text-caption text-faint">{keys.data?.length ?? 0} configured</span>
          }
        />
        <DataTable
          columns={columns}
          rows={keys.data}
          rowKey={(r) => r.id}
          loading={keys.isLoading}
          empty="No keys yet. Create one above to require authentication on /v1."
          tableLayout="fixed"
          rowHeaderKey="key"
          stickyRight="actions"
          scrollLabel="API keys"
        />
        <div className="flex flex-wrap items-center gap-2 border-t border-line px-5 py-3 text-caption text-faint">
          <span>endpoint</span>
          <CopyChip value={`${window.location.origin}/v1`} />
          <span className="ml-auto flex items-center gap-1.5">
            {system.data?.require_api_key ? (
              <>
                clients must present a key · <Badge>RROUTER_REQUIRE_API_KEY=true</Badge>
              </>
            ) : (
              <>
                enforcement is off · <Badge>RROUTER_REQUIRE_API_KEY=false</Badge>
              </>
            )}
          </span>
        </div>
      </Card>

      {creating ? (
        <CreateKeyModal
          onClose={() => setCreating(false)}
          onCreated={() => {
            setCreating(false);
            void client.invalidateQueries({ queryKey: qk.keys });
            void client.invalidateQueries({ queryKey: qk.system });
          }}
        />
      ) : null}

      {editing ? (
        <KeySettingsModal key={editing.id} row={editing} onClose={closeEdit} />
      ) : null}
    </>
  );
}

/** Everything hidden by a narrow breakpoint, kept as the row's title/aria summary. */
function rowSummary(row: KeyResult, team: string | null): string {
  const limits = [
    row.rpm > 0 ? `${compact(row.rpm)} rpm` : "unlimited rpm",
    row.tpm > 0 ? `${compact(row.tpm)} tpm` : "unlimited tpm",
    row.budget_tokens > 0 ? `${compact(row.budget_tokens)} tokens` : "unlimited tokens",
    row.budget_micros > 0 ? `${usd(row.budget_micros)} budget` : null,
  ]
    .filter((part): part is string => part !== null)
    .join(" · ");
  const models = modelScopeLabel(row);
  return [
    `team ${team ?? "none"}`,
    `spend ${usd(row.spent_micros)} · ${compact(row.spent_tokens)} tokens used`,
    `limits ${limits}`,
    `models ${models}`,
    `created ${when(row.created_at)}`,
  ].join(" · ");
}

/** Inactive outranks expired: a revoked key is disabled whatever its date says. */
function statusOf(row: KeyResult): { label: string; tone: "pos" | "neutral" | "warn"; dot: "pos" | "warn" | "muted" } {
  if (!row.is_active) return { label: "disabled", tone: "neutral", dot: "muted" };
  if (row.expires_at !== null && row.expires_at <= Date.now() / 1000) {
    return { label: "expired", tone: "warn", dot: "warn" };
  }
  return { label: "active", tone: "pos", dot: "pos" };
}

function Status({ row }: { row: KeyResult }) {
  const { label, tone, dot } = statusOf(row);
  const title = [
    row.is_active ? "accepting requests" : "every request is rejected with 403",
    row.expires_at === null ? "never expires" : `expires ${when(row.expires_at)}`,
  ].join(" · ");
  return (
    <Badge tone={tone} title={title}>
      <Dot tone={dot} />
      {label}
    </Badge>
  );
}

/** One key's identity: name, live status, tenancy/scope badges, and where it sits. */
function KeyCell({ row, team }: { row: KeyResult; team: string | null }) {
  const scope = modelScopeLabel(row);
  return (
    <div className="flex min-w-0 flex-col gap-1">
      <div className="flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-0.5">
        <span
          className="min-w-0 basis-full truncate text-body font-semibold text-fg min-[520px]:basis-auto"
          title={rowSummary(row, team)}
        >
          {row.name ?? <span className="font-normal text-faint">unnamed</span>}
        </span>
        <Status row={row} />
      </div>
      <div className="flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-0.5 text-caption text-faint">
        {row.scope === "admin" ? (
          <Badge tone="accent" title="manages the gateway; cannot call /v1 or spend tokens">
            admin
          </Badge>
        ) : null}
        {team === null ? (
          <Badge tone="neutral" title="not assigned to a team; serves instance-wide keys">
            shared
          </Badge>
        ) : (
          <Badge tone="neutral" title="keys assigned to this team">
            {team}
          </Badge>
        )}
        <span
          className="min-w-0 flex-1 basis-40 break-words"
          title={`created ${when(row.created_at)} · ${scope}`}
        >
          created {createdDay(row.created_at)} · {scope}
        </span>
      </div>
    </div>
  );
}

function createdDay(ts: number): string {
  return new Date(ts * 1000).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
  });
}

function modelScopeLabel(row: KeyResult): string {
  const allowed = row.allowed_models;
  const mapped = row.models?.length ?? 0;
  const scope =
    allowed === null || allowed.length === 0
      ? "all models"
      : `${allowed.length} ${allowed.length === 1 ? "model" : "models"}`;
  return mapped > 0 ? `${scope} · ${mapped} mapped` : scope;
}

function Limits({ row }: { row: KeyResult }) {
  const parts: Array<[number, string]> = [
    [row.rpm, "rpm"],
    [row.tpm, "tpm"],
    [row.budget_tokens, "tokens"],
  ];
  const summary = [
    ...parts.map(([value, unit]) => (value > 0 ? `${compact(value)} ${unit}` : `unlimited ${unit}`)),
    row.budget_micros > 0 ? usd(row.budget_micros) : null,
    (row.budget_micros > 0 || row.budget_tokens > 0) && row.budget_window !== "total"
      ? row.budget_window
      : null,
    row.guardrails ? "guarded" : null,
  ]
    .filter((part): part is string => part !== null)
    .join(" · ");
  return (
    <span className="block min-w-0 text-small leading-snug" title={summary}>
      {parts.map(([value, unit], i) => (
        <span key={unit}>
          {i > 0 ? <span className="text-faint"> · </span> : null}
          {value > 0 ? (
            <span className="text-muted tnum">{`${compact(value)} ${unit}`}</span>
          ) : (
            <span className="text-faint">{`unlimited ${unit}`}</span>
          )}
        </span>
      ))}
      {row.budget_micros > 0 ? (
        <span title="USD budget">
          <span className="text-faint"> · </span>
          <span className="text-muted tnum">{usd(row.budget_micros)}</span>
        </span>
      ) : null}
      {(row.budget_micros > 0 || row.budget_tokens > 0) && row.budget_window !== "total" ? (
        <span title="budget reset window">
          <span className="text-faint"> · </span>
          <span className="text-muted">{row.budget_window}</span>
        </span>
      ) : null}
      {row.guardrails ? (
        <span>
          <span className="text-faint"> · </span>
          <Guarded guardrails={row.guardrails} />
        </span>
      ) : null}
    </span>
  );
}

/** Faint table marker; the tooltip spells out every check the key carries. */
function Guarded({ guardrails }: { guardrails: Guardrails }) {
  const cap = guardrails.max_prompt_chars;
  // Defaults are dropped on the wire, so a policy that only caps prompts has no `blocked`.
  const blocked = guardrails.blocked ?? [];
  if (
    cap === undefined &&
    blocked.length === 0 &&
    !guardrails.scan_output &&
    !guardrails.redact_pii
  ) {
    return null;
  }
  const limits = [
    cap === undefined ? "prompt length: unlimited" : `prompt: at most ${compact(cap)} chars`,
    blocked.length === 0 ? "no blocked phrases" : `blocked: ${blocked.join(", ")}`,
    guardrails.scan_output ? "responses scanned too — a hit blocks the reply" : "prompts only",
    guardrails.redact_pii ? "PII redacted in prompts and response text, including streams" : null,
  ].filter((line): line is string => line !== null);
  return (
    <Badge title={limits.join("\n")}>
      {blocked.length > 0 ? `${blocked.length} blocked` : "guarded"}
    </Badge>
  );
}

/** Spend counters are already scoped to the key's window; this only names that window. */
const WINDOW_NOW: Record<BudgetWindow, string | null> = {
  total: null,
  daily: "today",
  weekly: "this week",
  monthly: "this month",
};

/** Money spent against the USD budget; the token budget stays a hover detail. */
function Spend({ row }: { row: KeyResult }) {
  const now = WINDOW_NOW[row.budget_window];
  const dollarNote =
    row.budget_micros > 0
      ? `${usd(row.spent_micros)} of ${usd(row.budget_micros)} USD used`
      : `${usd(row.spent_micros)} USD used · no USD budget`;
  const tokenNote =
    row.budget_tokens > 0
      ? `${compact(row.spent_tokens)} of ${compact(row.budget_tokens)} tokens used`
      : `${compact(row.spent_tokens)} tokens used · no token budget`;
  const title = [dollarNote, tokenNote, ...(now ? [`resets ${row.budget_window}`] : [])].join(" · ");
  const budget = row.budget_micros;
  if (!(budget > 0)) {
    return (
      <span className="font-mono text-small text-muted tnum" title={title}>
        {usd(row.spent_micros)}
        {now ? <span className="ml-1 text-caption text-faint">{now}</span> : null}
      </span>
    );
  }
  const reached = row.spent_micros >= budget;
  const share = Math.min(100, (row.spent_micros / budget) * 100);
  return (
    <div className="min-w-0" title={title}>
      <div className="flex flex-wrap items-baseline gap-1 font-mono text-small tnum">
        <span className={reached ? "text-neg" : "text-fg"}>{usd(row.spent_micros)}</span>
        <span className="text-faint">/ {usd(budget)}</span>
        {now ? <span className="text-caption text-faint">{now}</span> : null}
      </div>
      <span className="mt-1 block h-1.5 w-full overflow-hidden rounded-full bg-[var(--color-rule)]">
        <span
          className="block h-full rounded-full"
          style={{
            width: `${Math.max(2, share)}%`,
            background: reached ? "var(--color-neg)" : "var(--color-accent)",
          }}
        />
      </span>
      {reached ? <span className="mt-1 block text-caption text-neg">budget reached</span> : null}
    </div>
  );
}

/** Empty means unlimited; a bare `0` is the API's own spelling of the same thing. */
function toCount(value: string): number {
  const n = Math.floor(Number(value));
  return Number.isFinite(n) && n > 0 ? n : 0;
}

/** `YYYY-MM-DD` in the operator's own timezone. */
function toDateInput(ts: number | null): string {
  if (ts === null) return "";
  const d = new Date(ts * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** A date picker means "good through the end of that day", never midnight. */
function fromDateInput(value: string): number | null {
  if (value === "") return null;
  const [y, m, d] = value.split("-").map(Number);
  if (y === undefined || m === undefined || d === undefined) return null;
  const at = new Date(y, m - 1, d, 23, 59, 59, 0);
  return Number.isNaN(at.getTime()) ? null : Math.floor(at.getTime() / 1000);
}

function DateInput({
  value,
  onChange,
  ariaLabel,
  disabled,
}: {
  value: string;
  onChange: (v: string) => void;
  ariaLabel: string;
  disabled?: boolean;
}) {
  return (
    <input
      type="date"
      value={value}
      disabled={disabled}
      aria-label={ariaLabel}
      onChange={(e) => onChange(e.target.value)}
      className="h-8 w-full min-w-0 rounded-lg border border-line bg-surface-2 px-2.5 text-body text-fg focus:border-[var(--color-accent)]/60 focus:outline-none [color-scheme:dark]"
    />
  );
}

/** The API's machine-readable 403/429 reason, when the body carries one. */
function errorCode(error: unknown): string | null {
  if (!(error instanceof ApiError)) return null;
  const body: unknown = error.body;
  if (typeof body !== "object" || body === null || !("error" in body)) return null;
  const inner = body.error;
  if (typeof inner !== "object" || inner === null || !("code" in inner)) return null;
  return typeof inner.code === "string" ? inner.code : null;
}

/** Server errors carry `{error:{message,code}}`; both belong next to the field, not in a banner. */
function InlineError({ error }: { error: unknown }) {
  const message = error instanceof Error ? error.message : String(error);
  const code = errorCode(error);
  return (
    <div className="rounded-lg border border-[var(--color-neg)]/35 bg-[var(--color-neg)]/10 px-3 py-2 text-small">
      <span className="break-words text-[color-mix(in_srgb,var(--color-neg)_62%,white)]">{message}</span>
      {code ? <span className="ml-2 font-mono text-caption text-faint">{code}</span> : null}
    </div>
  );
}

/** Rows are compared after trimming, so a stray space is not a change. */
function mapKey(pairs: KeyModelMap[]): string {
  return JSON.stringify(pairs.map((p) => [p.from.trim(), p.to]));
}

/**
 * `from` must be present and unique; `to` must be a registry ref. `refs === null` means the
 * registry has not loaded, so membership cannot be judged yet; an untouched `to` stays
 * legal even after the registry dropped it, so a stale row never blocks a limits save.
 */
function validateMap(
  pairs: KeyModelMap[],
  refs: Set<string> | null,
  original: Map<string, string>,
): string | null {
  const seen = new Set<string>();
  for (const [i, pair] of pairs.entries()) {
    const from = pair.from.trim();
    if (from === "") return `mapping ${i + 1}: enter the model name a client sends`;
    if (seen.has(from)) return `mapping ${i + 1}: "${from}" is already mapped`;
    seen.add(from);
    if (pair.to === "") return `mapping ${i + 1}: choose a model this gateway serves`;
    if (refs !== null && !refs.has(pair.to) && original.get(from) !== pair.to) {
      return `mapping ${i + 1}: "${pair.to}" is not in the model list`;
    }
  }
  return null;
}

/** The server's policy limits; enforcing them here keeps a 400 from ever being the first hint. */
const MAX_BLOCKED_TERMS = 200;
const MAX_TERM_CHARS = 200;

/** Inference is the default: switching a key to admin is the deliberate move. */
const SCOPES: ReadonlyArray<{ id: KeyScope; label: string }> = [
  { id: "inference", label: "Inference" },
  { id: "admin", label: "Admin" },
];

/** Configure a key before it exists; the plaintext is revealed inside this drawer once. */
function CreateKeyModal({ onClose, onCreated }: { onClose: () => void; onCreated: () => void }) {
  const [name, setName] = useState("");
  const [scope, setScope] = useState<KeyScope>("inference");
  const [team, setTeam] = useState("");
  const teams = useTeams();
  const [rpm, setRpm] = useState("");
  const [tpm, setTpm] = useState("");
  const [tokens, setTokens] = useState("");
  const [budget, setBudget] = useState("");
  const [reset, setReset] = useState<BudgetWindow>("total");
  const [models, setModels] = useState<string[]>([]);
  const [redact, setRedact] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<unknown>(null);
  const [created, setCreated] = useState<NewKey | null>(null);

  const submit = async () => {
    setBusy(true);
    setFailure(null);
    try {
      const key = await api.post<NewKey>("/api/keys", {
        name: name.trim() || null,
        team_id: team === "" ? null : team,
      });
      const patch: UpdateKey = {
        scope,
        rpm: Number(rpm) || 0,
        tpm: Number(tpm) || 0,
        budget_tokens: Number(tokens) || 0,
        budget_window: reset,
        allowed_models: models.length > 0 ? models : null,
        // Guardrails are a separate policy document; a fresh key has none, so the toggle
        // is applied with the same PATCH as the limits.
        guardrails: redact ? { blocked: [], scan_output: false, redact_pii: true } : null,
      };
      const parsed = parseUsd(budget);
      if (!("problem" in parsed)) patch.budget_micros = parsed.micros;
      await api.patch<KeyResult>(`/api/keys/${key.id}`, patch);
      setCreated(key);
    } catch (e) {
      setFailure(e);
    } finally {
      setBusy(false);
    }
  };

  if (created) {
    return (
      <Modal
        title="Key created"
        open
        onClose={onCreated}
        description="This is the only time the plaintext is shown. Store it before closing."
        footer={<Button variant="primary" onClick={onCreated}>done</Button>}
      >
        <div className="flex flex-col gap-3">
          <CopyChip value={created.key} />
          <p className="text-small text-muted">
            Clients send it as <code className="font-mono text-caption">Authorization: Bearer …</code>{" "}
            against the endpoint on the Keys page.
          </p>
        </div>
      </Modal>
    );
  }

  return (
    <Modal
      title="Create API key"
      open
      onClose={onClose}
      description="Limits are optional; empty means unlimited. The key is minted only when you create it."
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={busy}>
            {busy ? "creating…" : "create key"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <Field label="name" hint="how this key appears in logs and budgets">
          <Input value={name} onChange={setName} placeholder="production-app" ariaLabel="key name" />
        </Field>
        <Field label="scope" hint="admin keys manage the gateway and cannot spend tokens">
          <Segmented
            options={[
              { id: "inference", label: "Inference" },
              { id: "admin", label: "Admin" },
            ]}
            value={scope}
            onChange={(v) => setScope(v as KeyScope)}
            ariaLabel="key scope"
          />
        </Field>
        <Field label="team" hint="instance-wide unless assigned; a team owns the key's spend report">
          <Select value={team} onChange={setTeam} ariaLabel="key team">
            <option value="">instance-wide</option>
            {(teams.data ?? []).map((team) => (
              <option key={team.id} value={team.id}>
                {team.name}
              </option>
            ))}
          </Select>
        </Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label="requests / min" hint="empty = unlimited">
            <Input type="number" value={rpm} onChange={setRpm} ariaLabel="requests per minute" />
          </Field>
          <Field label="tokens / min" hint="empty = unlimited">
            <Input type="number" value={tpm} onChange={setTpm} ariaLabel="tokens per minute" />
          </Field>
          <Field label="token budget" hint="empty = unlimited">
            <Input type="number" value={tokens} onChange={setTokens} ariaLabel="token budget" />
          </Field>
          <Field label="spend budget (USD)" hint="empty = unlimited">
            <Input value={budget} onChange={setBudget} ariaLabel="spend budget" />
          </Field>
        </div>
        <Field label="budget window" hint="when the counters reset">
          <Select
            value={reset}
            onChange={(v) => setReset(v as BudgetWindow)}
            ariaLabel="budget window"
          >
            <option value="total">total (never)</option>
            <option value="daily">daily</option>
            <option value="weekly">weekly</option>
            <option value="monthly">monthly</option>
          </Select>
        </Field>
        <ModelPicker selected={models} onChange={setModels} />

        <div className="flex items-start gap-2 text-body text-fg">
          <Checkbox
            checked={redact}
            disabled={busy}
            ariaLabel="redact PII"
            onChange={setRedact}
            className="mt-0.5"
          />
          <span>
            redact PII
            <span className="mt-0.5 block text-caption text-faint">
              masks email addresses, phone numbers, card numbers and credential-shaped strings
              in prompts and in response text, streamed text included (rewritten with a bounded
              carry). Tool-call argument deltas are not rewritten. Off by default; more
              guardrails can be added after creation.
            </span>
          </span>
        </div>
        {failure ? <ErrorNote error={failure} /> : null}
      </div>
    </Modal>
  );
}

const MAX_KEY_NAME = 100;

function KeySettingsModal({ row, onClose }: { row: KeyResult; onClose: () => void }) {
  const [name, setName] = useState(row.name ?? "");
  const [active, setActive] = useState(row.is_active);
  const [scope, setScope] = useState<KeyScope>(row.scope);
  const [team, setTeam] = useState(row.team_id ?? "");
  const teams = useTeams();
  const [rpm, setRpm] = useState(row.rpm > 0 ? String(row.rpm) : "");
  const [tpm, setTpm] = useState(row.tpm > 0 ? String(row.tpm) : "");
  const [budget, setBudget] = useState(row.budget_tokens > 0 ? String(row.budget_tokens) : "");
  const [budgetUsd, setBudgetUsd] = useState(usdInput(row.budget_micros));
  const [budgetProblem, setBudgetProblem] = useState<string | null>(null);
  const [expires, setExpires] = useState(toDateInput(row.expires_at));
  const [allowed, setAllowed] = useState<string[]>(row.allowed_models ?? []);
  const [resets, setResets] = useState<BudgetWindow>(row.budget_window);
  const [maxPrompt, setMaxPrompt] = useState(
    row.guardrails?.max_prompt_chars !== undefined ? String(row.guardrails.max_prompt_chars) : "",
  );
  const [blocked, setBlocked] = useState<string[]>(row.guardrails?.blocked ?? []);
  const [scanOutput, setScanOutput] = useState(row.guardrails?.scan_output ?? false);
  const [redact, setRedact] = useState(row.guardrails?.redact_pii ?? false);
  const [term, setTerm] = useState("");
  const [guardProblem, setGuardProblem] = useState<string | null>(null);

  // `baseline === null` until the map has been read once: until then the section shows
  // loading or its retry, and a save leaves the server's map untouched rather than clearing it.
  const map = useKeyModels(row.id);
  const [pairs, setPairs] = useState<KeyModelMap[]>([]);
  const [baseline, setBaseline] = useState<KeyModelMap[] | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const nameInvalid = [...name.trim()].length > MAX_KEY_NAME;

  // Only refs the registry lists are offered as targets; a null set means it has not loaded.
  const catalog = useModels();
  const refs = useMemo(
    () => (catalog.isSuccess ? new Set((catalog.data ?? []).map((m) => m.ref)) : null),
    [catalog.isSuccess, catalog.data],
  );
  const targets = useMemo(() => [...(refs ?? [])].sort(), [refs]);
  // One datalist per modal: the key id already makes a unique, stable DOM id.
  const listId = `key-mappings-${row.id}`;

  useEffect(() => {
    if (baseline !== null || map.data === undefined) return;
    setPairs(map.data);
    setBaseline(map.data);
  }, [baseline, map.data]);

  const first = useRef<HTMLButtonElement>(null);
  // Modal focuses its own container in an effect that runs after this one; the
  // animation frame puts the caret in the first field instead.
  useEffect(() => {
    const frame = window.requestAnimationFrame(() => first.current?.focus());
    return () => window.cancelAnimationFrame(frame);
  }, []);

  const patchKey = useUpdateKey();
  const putMap = useUpdateKeyModels(row.id);

  const setPair = (index: number, next: Partial<KeyModelMap>) => {
    setPairs(pairs.map((pair, i) => (i === index ? { ...pair, ...next } : pair)));
  };

  const addTerm = () => {
    // Stored trimmed and lowercased: the server matches substrings case-insensitively,
    // so two spellings of one phrase would be two chips that block the same text.
    const next = term.trim().toLowerCase();
    if (next === "") return;
    if (blocked.length >= MAX_BLOCKED_TERMS) {
      setGuardProblem(`a key carries at most ${MAX_BLOCKED_TERMS} blocked phrases`);
      return;
    }
    if ([...next].length > MAX_TERM_CHARS) {
      setGuardProblem(`a blocked phrase is limited to ${MAX_TERM_CHARS} characters`);
      return;
    }
    if (blocked.includes(next)) {
      setTerm("");
      setGuardProblem(null);
      return;
    }
    setBlocked([...blocked, next]);
    setTerm("");
    setGuardProblem(null);
  };

  const summary =
    baseline === null
      ? map.isError
        ? "unavailable"
        : "loading…"
      : pairs.length === 0
        ? "off"
        : `${pairs.length} rewrite${pairs.length === 1 ? "" : "s"}`;

  const submit = () => {
    const trimmedName = name.trim();
    if ([...trimmedName].length > MAX_KEY_NAME) return;
    const parsed = parseUsd(budgetUsd);
    if ("problem" in parsed) {
      setBudgetProblem(parsed.problem);
      return;
    }
    setBudgetProblem(null);
    const clean = pairs.map((p) => ({ from: p.from.trim(), to: p.to }));
    const invalid = validateMap(clean, refs, new Map((baseline ?? []).map((p) => [p.from, p.to])));
    if (invalid !== null) {
      setProblem(invalid);
      return;
    }
    setProblem(null);
    // `0` is a real cap here ("no prompt at all"), so empty must stay absent rather than
    // collapse into the 0-is-unlimited spelling the other limits use.
    const capText = maxPrompt.trim();
    const cap = capText === "" ? undefined : Number(capText);
    if (cap !== undefined && (!Number.isInteger(cap) || cap < 0)) {
      setGuardProblem("max prompt characters: whole number, or empty for unlimited");
      return;
    }
    setGuardProblem(null);
    const guardrails: Guardrails | null =
      cap === undefined && blocked.length === 0 && !scanOutput && !redact
        ? null
        : {
            ...(cap === undefined ? {} : { max_prompt_chars: cap }),
            blocked,
            scan_output: scanOutput,
            redact_pii: redact,
          };
    setFailure(null);
    setBusy(true);
    const patch: UpdateKey = {
      name: trimmedName === "" ? null : trimmedName,
      is_active: active,
      scope,
      rpm: toCount(rpm),
      tpm: toCount(tpm),
      budget_tokens: toCount(budget),
      budget_micros: parsed.micros,
      budget_window: resets,
      allowed_models: allowed.length === 0 ? null : allowed,
      expires_at: fromDateInput(expires),
      guardrails,
      team_id: team === "" ? null : team,
    };
    void (async () => {
      // Limits first, then the map: a failed PUT keeps the modal open with the error and
      // every edit still in place, instead of closing over a half-saved key.
      try {
        await patchKey.mutateAsync({ id: row.id, patch });
        if (baseline !== null && mapKey(clean) !== mapKey(baseline)) {
          await putMap.mutateAsync(clean);
        }
        onClose();
      } catch (error) {
        setFailure(error);
        setBusy(false);
      }
    })();
  };

  return (
    <Modal
      title="Key settings"
      width="lg"
      open
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>cancel</Button>
          <Button variant="primary" disabled={busy || nameInvalid} onClick={submit}>
            {busy ? "saving…" : "save"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <div className="flex min-w-0 flex-col gap-1">
          <Field label="name" hint="how this key appears in logs and budgets">
            <Input
              value={name}
              onChange={(value) => {
                setName(value);
                setFailure(null);
              }}
              placeholder="empty = unnamed"
              ariaLabel="key name"
              disabled={busy}
            />
          </Field>
          <span
            role={nameInvalid ? "alert" : undefined}
            className={cx("text-caption", nameInvalid ? "text-neg" : "text-faint")}
          >
            {nameInvalid
              ? `a key name is limited to ${MAX_KEY_NAME} characters`
              : `${[...name.trim()].length}/${MAX_KEY_NAME} characters · empty means unnamed`}
          </span>
        </div>

        <div className="flex items-center gap-2 text-body text-fg">
          <Checkbox
            rootRef={first}
            checked={active}
            ariaLabel="key active"
            onChange={setActive}
          />
          active
          <span className="text-caption text-faint">
            {active ? "accepting requests" : "every request is rejected with 403"}
          </span>
        </div>

        <div className="flex min-w-0 flex-col gap-1.5">
          <span className="text-caption font-medium uppercase tracking-[0.08em] text-faint">
            scope
          </span>
          <Segmented
            options={SCOPES}
            value={scope}
            onChange={(v) => {
              if (!busy) setScope(v);
            }}
            ariaLabel="key scope"
            className={busy ? "pointer-events-none opacity-60" : undefined}
          />
          <span className="text-caption text-faint">
            admin keys manage the gateway and cannot spend tokens; inference keys call /v1 and
            obey limits and guardrails.
          </span>
        </div>

        <div className="grid grid-cols-2 gap-3">
          <Field label="requests/min" hint="empty = unlimited">
            <Input
              type="number"
              value={rpm}
              onChange={setRpm}
              placeholder="60"
              ariaLabel="requests per minute"
              disabled={busy}
            />
          </Field>
          <Field label="tokens/min" hint="empty = unlimited">
            <Input
              type="number"
              value={tpm}
              onChange={setTpm}
              placeholder="90000"
              ariaLabel="tokens per minute"
              disabled={busy}
            />
          </Field>
          <Field label="token budget" hint="empty = unlimited">
            <Input
              type="number"
              value={budget}
              onChange={setBudget}
              placeholder="5000000"
              ariaLabel="token budget"
              disabled={busy}
            />
          </Field>
          <div className="flex min-w-0 flex-col gap-1">
            <Field label="budget (USD)" hint="empty = unlimited">
              <Input
                value={budgetUsd}
                onChange={(v) => {
                  setBudgetUsd(v);
                  setBudgetProblem(null);
                }}
                placeholder="1.00"
                ariaLabel="budget in USD"
                disabled={busy}
              />
            </Field>
            {budgetProblem ? (
              <span role="alert" className="text-caption text-neg">
                {budgetProblem}
              </span>
            ) : null}
          </div>
        </div>

        <Field label="resets" hint="spend shown is for the current window; budgets are enforced against it">
          <Select
            value={resets}
            onChange={(v) => setResets(v as BudgetWindow)}
            ariaLabel="budget reset window"
            disabled={busy}
          >
            <option value="total">total (never)</option>
            <option value="daily">daily</option>
            <option value="weekly">weekly</option>
            <option value="monthly">monthly</option>
          </Select>
        </Field>

        <Field label="team" hint="instance-wide unless assigned">
          <Select value={team} onChange={setTeam} ariaLabel="key team" disabled={busy}>
            <option value="">instance-wide</option>
            {(teams.data ?? []).map((team) => (
              <option key={team.id} value={team.id}>
                {team.name}
              </option>
            ))}
          </Select>
        </Field>

        <Field label="expires" hint="empty = never">
          <DateInput
            value={expires}
            onChange={setExpires}
            ariaLabel="expiry date"
            disabled={busy}
          />
        </Field>

        <ModelPicker selected={allowed} onChange={setAllowed} disabled={busy} />

        <section className="flex min-w-0 flex-col gap-2">
          <div className="flex items-baseline justify-between gap-3">
            <span className="text-caption font-medium uppercase tracking-[0.08em] text-faint">
              model mapping
            </span>
            <span className="text-caption text-faint tnum">{summary}</span>
          </div>
          <span className="text-caption text-faint">
            Rewrite the model a client sends to a model this gateway serves.
          </span>

          {map.isError ? (
            <ErrorNote error={map.error} onRetry={() => void map.refetch()} />
          ) : null}

          {baseline === null ? (
            map.isError ? null : <div className="text-small text-faint">loading mapping…</div>
          ) : (
            <>
              {pairs.length === 0 ? (
                <div className="text-small text-faint">
                  no rewrites — model names pass through unchanged
                </div>
              ) : null}

              {pairs.map((pair, index) => (
                <div
                  key={index}
                  className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] items-center gap-2"
                >
                  <input
                    value={pair.from}
                    disabled={busy}
                    placeholder="default"
                    aria-label={`mapping ${index + 1} from`}
                    onChange={(e) => setPair(index, { from: e.target.value })}
                    className="h-8 w-full min-w-0 rounded-lg border border-line bg-surface-2 px-2.5 font-mono text-small text-fg placeholder:text-faint focus:border-[var(--color-accent)]/60 focus:outline-none"
                  />
                  <input
                    list={listId}
                    value={pair.to}
                    disabled={busy}
                    placeholder="provider/model"
                    aria-label={`mapping ${index + 1} to`}
                    onChange={(e) => setPair(index, { to: e.target.value })}
                    className="h-8 w-full min-w-0 rounded-lg border border-line bg-surface-2 px-2.5 font-mono text-small text-fg placeholder:text-faint focus:border-[var(--color-accent)]/60 focus:outline-none"
                  />
                  <button
                    type="button"
                    disabled={busy}
                    aria-label={`remove mapping ${index + 1}`}
                    onClick={() => setPairs(pairs.filter((_, i) => i !== index))}
                    className="h-8 w-7 rounded-md text-muted hover:bg-[var(--color-neg-tint)] hover:text-neg"
                  >
                    ×
                  </button>
                </div>
              ))}

              <datalist id={listId}>
                {targets.map((ref) => (
                  <option key={ref} value={ref} />
                ))}
              </datalist>

              <div>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => setPairs([...pairs, { from: "", to: "" }])}
                  className="text-small text-muted underline underline-offset-2 hover:text-fg"
                >
                  add mapping
                </button>
              </div>
            </>
          )}

          {problem ? (
            <span role="alert" className="text-small text-neg">
              {problem}
            </span>
          ) : null}
        </section>

        <section className="flex min-w-0 flex-col gap-2">
          <div className="flex items-baseline justify-between gap-3">
            <span className="text-caption font-medium uppercase tracking-[0.08em] text-faint">
              guardrails
            </span>
            <span className="text-caption text-faint tnum">
              {blocked.length}/{MAX_BLOCKED_TERMS} blocked
            </span>
          </div>
          <span className="text-caption text-faint">
            Case-insensitive substring match, no regex. A hit refuses the request; with
            response scanning it refuses the reply too.
          </span>

          <Field label="max prompt characters" hint="empty = unlimited; 0 refuses every prompt">
            <Input
              type="number"
              value={maxPrompt}
              onChange={(v) => {
                setMaxPrompt(v);
                setGuardProblem(null);
              }}
              placeholder="unlimited"
              ariaLabel="max prompt characters"
              disabled={busy}
            />
          </Field>

          <div className="flex min-w-0 flex-col gap-1.5">
            <span className="text-caption font-medium uppercase tracking-[0.08em] text-faint">
              blocked phrases
            </span>
            <form
              className="flex items-center gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                addTerm();
              }}
            >
              <input
                value={term}
                disabled={busy}
                placeholder="phrase to block"
                aria-label="blocked phrase"
                onChange={(e) => {
                  setTerm(e.target.value);
                  setGuardProblem(null);
                }}
                className="h-8 w-full min-w-0 rounded-lg border border-line bg-surface-2 px-2.5 text-body text-fg placeholder:text-faint focus:border-[var(--color-accent)]/60 focus:outline-none"
              />
              <Button
                type="submit"
                disabled={
                  busy || term.trim() === "" || blocked.length >= MAX_BLOCKED_TERMS
                }
              >
                add
              </Button>
            </form>
            {blocked.length === 0 ? (
              <div className="text-small text-faint">
                none — every prompt passes through
              </div>
            ) : (
              <div className="flex flex-wrap gap-1.5">
                {blocked.map((phrase) => (
                  <span
                    key={phrase}
                    className="inline-flex max-w-full items-center gap-1 rounded-md border border-line bg-surface-2 px-1.5 py-0.5 text-caption text-muted"
                  >
                    <span className="break-all font-mono">{phrase}</span>
                    <button
                      type="button"
                      disabled={busy}
                      aria-label={`remove blocked phrase ${phrase}`}
                      onClick={() => setBlocked(blocked.filter((p) => p !== phrase))}
                      className="text-faint hover:text-neg"
                    >
                      ×
                    </button>
                  </span>
                ))}
              </div>
            )}
          </div>

          <div className="flex items-start gap-2 text-body text-fg">
            <Checkbox
              checked={scanOutput}
              disabled={busy}
              ariaLabel="scan responses"
              onChange={setScanOutput}
              className="mt-0.5"
            />
            <span>
              scan responses
              <span className="mt-0.5 block text-caption text-faint">
                a matching response is refused before it reaches the client, and a stream is
                cut with an error instead of completing. Needs at least one blocked phrase.
              </span>
            </span>
          </div>

          <div className="flex items-start gap-2 text-body text-fg">
            <Checkbox
              checked={redact}
              disabled={busy}
              ariaLabel="redact PII"
              onChange={setRedact}
              className="mt-0.5"
            />
            <span>
              redact PII
              <span className="mt-0.5 block text-caption text-faint">
                masks email addresses, phone numbers, card numbers and credential-shaped
                strings in the prompt before it is sent and in response text before the client
                reads it, streamed text included (rewritten with a bounded carry). Tool-call
                argument deltas are not rewritten. Off by default.
              </span>
            </span>
          </div>

          {guardProblem ? (
            <span role="alert" className="text-small text-neg">
              {guardProblem}
            </span>
          ) : null}
        </section>

        <div className="flex items-center justify-between gap-3 text-caption text-faint">
          <span className="font-mono">{row.id}</span>
          <span>0 or empty limits mean unlimited, except max prompt characters</span>
        </div>

        {failure ? <InlineError error={failure} /> : null}
      </div>
    </Modal>
  );
}
