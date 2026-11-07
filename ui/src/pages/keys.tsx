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
} from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { ModelPicker } from "../components/model-picker";
import { compact, parseUsd, usd, usdInput, when } from "../lib/format";
import { qk, useKeyModels, useKeys, useModels, useUpdateKey, useUpdateKeyModels } from "../lib/queries";
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
  const client = useQueryClient();
  const [name, setName] = useState("");
  const [issued, setIssued] = useState<NewKey | null>(null);
  const [editing, setEditing] = useState<KeyResult | null>(null);
  const [revoking, setRevoking] = useState<KeyResult | null>(null);

  // The Modal re-focuses itself whenever `onClose` changes identity; keep it stable.
  const closeEdit = useCallback(() => setEditing(null), []);

  const create = useMutation({
    mutationFn: () => api.post<NewKey>("/api/keys", { name: name || null }),
    onSuccess: (key) => {
      setIssued(key);
      setName("");
      void client.invalidateQueries({ queryKey: qk.keys });
      void client.invalidateQueries({ queryKey: qk.system });
    },
  });

  const remove = useMutation({
    mutationFn: (id: string) => api.del<{ deleted: boolean }>(`/api/keys/${id}`),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: qk.keys });
      void client.invalidateQueries({ queryKey: qk.system });
    },
  });

  const columns: Array<Column<KeyResult>> = [
    {
      key: "name",
      title: "Name",
      render: (r) => (
        <span className="flex items-center gap-1.5">
          <span className="font-medium">{r.name ?? <span className="text-faint">unnamed</span>}</span>
          {r.scope === "admin" ? (
            <Badge tone="accent" title="manages the gateway; cannot call /v1 or spend tokens">
              admin
            </Badge>
          ) : null}
        </span>
      ),
    },
    { key: "status", title: "Status", render: (r) => <Status row={r} /> },
    { key: "limits", title: "Limits", render: (r) => <Limits row={r} /> },
    { key: "spend", title: "Spend", render: (r) => <Spend row={r} /> },
    { key: "models", title: "Models", render: (r) => <Models row={r} /> },
    {
      key: "created",
      title: "Created",
      render: (r) => <span className="whitespace-nowrap text-muted">{when(r.created_at)}</span>,
    },
    {
      key: "actions",
      title: "",
      align: "right",
      render: (r) => (
        <div className="flex items-center justify-end gap-1.5">
          <Button onClick={() => setEditing(r)} title={`edit limits for ${r.name ?? r.id}`}>
            edit
          </Button>
          <Button variant="danger" onClick={() => setRevoking(r)}>
            delete
          </Button>
        </div>
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
      />

      {keys.isError ? (
        <div className="mb-4">
          <ErrorNote error={keys.error} onRetry={() => void keys.refetch()} />
        </div>
      ) : null}

      {issued ? (
        <Card className="mb-3 border-[var(--color-accent)]/35 bg-[var(--color-accent)]/10 px-5 py-4">
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <div className="text-[12px] text-[var(--color-accent-text)]">Copy this key now — it is not shown again.</div>
              <code className="mt-1 block truncate font-mono text-[13px] text-fg">{issued.key}</code>
            </div>
            <Button
              variant="primary"
              onClick={() => {
                void navigator.clipboard.writeText(issued.key);
              }}
            >
              copy
            </Button>
          </div>
        </Card>
      ) : null}

      <Card>
        <CardHeader
          title="Keys"
          action={
            <form
              className="flex items-center gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                create.mutate();
              }}
            >
              <Input value={name} onChange={setName} placeholder="key name" ariaLabel="new key name" className="w-[200px]" />
              <Button type="submit" variant="primary" disabled={create.isPending}>
                create key
              </Button>
            </form>
          }
        />
        {create.isError ? (
          <div className="px-5 pb-3">
            <ErrorNote error={create.error} />
          </div>
        ) : null}
        <DataTable
          columns={columns}
          rows={keys.data}
          rowKey={(r) => r.id}
          loading={keys.isLoading}
          empty="No keys yet. Create one above to require authentication on /v1."
        />
        <div className="flex flex-wrap items-center gap-2 border-t border-line px-5 py-3 text-[11px] text-faint">
          <span>endpoint</span>
          <CopyChip value={`${window.location.origin}/v1`} />
          <span className="ml-auto flex items-center gap-1.5">
            enforcement is controlled by <Badge>require_api_key</Badge> in the server config
          </span>
        </div>
      </Card>

      {editing ? (
        <KeyLimitsModal key={editing.id} row={editing} onClose={closeEdit} />
      ) : null}
    </>
  );
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

function Limits({ row }: { row: KeyResult }) {
  const parts: Array<[number, string]> = [
    [row.rpm, "rpm"],
    [row.tpm, "tpm"],
    [row.budget_tokens, "tokens"],
  ];
  return (
    <span className="whitespace-nowrap text-[12px]">
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
  if (cap === undefined && blocked.length === 0 && !guardrails.scan_output) return null;
  const limits = [
    cap === undefined ? "prompt length: unlimited" : `prompt: at most ${compact(cap)} chars`,
    blocked.length === 0 ? "no blocked phrases" : `blocked: ${blocked.join(", ")}`,
    guardrails.scan_output ? "responses scanned too — a hit blocks the reply" : "prompts only",
  ];
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
      <span className="font-mono text-[12px] text-muted tnum" title={title}>
        {usd(row.spent_micros)}
        {now ? <span className="ml-1 text-[11px] text-faint">{now}</span> : null}
      </span>
    );
  }
  const reached = row.spent_micros >= budget;
  const share = Math.min(100, (row.spent_micros / budget) * 100);
  return (
    <div className="min-w-[132px]" title={title}>
      <div className="flex items-baseline gap-1 font-mono text-[12px] tnum">
        <span className={reached ? "text-neg" : "text-fg"}>{usd(row.spent_micros)}</span>
        <span className="text-faint">/ {usd(budget)}</span>
        {now ? <span className="text-[11px] text-faint">{now}</span> : null}
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
      {reached ? <span className="mt-1 block text-[11px] text-neg">budget reached</span> : null}
    </div>
  );
}

function Models({ row }: { row: KeyResult }) {
  const list = row.allowed_models;
  const mapped = row.models ?? [];
  const many = list !== null && list.length > 0;
  const scope = many
    ? `${list.length} ${list.length === 1 ? "model" : "models"}`
    : "All models";
  const title = [
    ...(many ? list : ["any model"]),
    ...mapped.map((m) => `${m.from} → ${m.to}`),
  ].join("\n");
  return (
    <span className="whitespace-nowrap text-muted" title={title}>
      {scope}
      {mapped.length > 0 ? (
        <span className="text-faint"> · {mapped.length} mapped</span>
      ) : null}
    </span>
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
      className="h-8 w-full min-w-0 rounded-lg border border-line bg-surface-2 px-2.5 text-[13px] text-fg focus:border-[var(--color-accent)]/60 focus:outline-none [color-scheme:dark]"
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
    <div className="rounded-lg border border-[var(--color-neg)]/35 bg-[var(--color-neg)]/10 px-3 py-2 text-[12px]">
      <span className="break-words text-[color-mix(in_srgb,var(--color-neg)_62%,white)]">{message}</span>
      {code ? <span className="ml-2 font-mono text-[11px] text-faint">{code}</span> : null}
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

function KeyLimitsModal({ row, onClose }: { row: KeyResult; onClose: () => void }) {
  const [active, setActive] = useState(row.is_active);
  const [scope, setScope] = useState<KeyScope>(row.scope);
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

  const first = useRef<HTMLInputElement>(null);
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
      cap === undefined && blocked.length === 0 && !scanOutput
        ? null
        : {
            ...(cap === undefined ? {} : { max_prompt_chars: cap }),
            blocked,
            scan_output: scanOutput,
          };
    setFailure(null);
    setBusy(true);
    const patch: UpdateKey = {
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
      title="Key limits"
      width="lg"
      open
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>cancel</Button>
          <Button variant="primary" disabled={busy} onClick={submit}>
            {busy ? "saving…" : "save"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <label className="flex items-center gap-2 text-[13px] text-fg">
          <input
            ref={first}
            type="checkbox"
            checked={active}
            aria-label="key active"
            onChange={(e) => setActive(e.target.checked)}
            className="h-3.5 w-3.5 rounded border border-line-strong bg-[var(--color-surface-2)] accent-[var(--color-accent)]"
          />
          active
          <span className="text-[11px] text-faint">
            {active ? "accepting requests" : "every request is rejected with 403"}
          </span>
        </label>

        <div className="flex min-w-0 flex-col gap-1.5">
          <span className="text-[11px] font-medium uppercase tracking-[0.08em] text-faint">
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
          <span className="text-[11px] text-faint">
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
              <span role="alert" className="text-[11px] text-neg">
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
            <span className="text-[11px] font-medium uppercase tracking-[0.08em] text-faint">
              model mapping
            </span>
            <span className="text-[11px] text-faint tnum">{summary}</span>
          </div>
          <span className="text-[11px] text-faint">
            Rewrite the model a client sends to a model this gateway serves.
          </span>

          {map.isError ? (
            <ErrorNote error={map.error} onRetry={() => void map.refetch()} />
          ) : null}

          {baseline === null ? (
            map.isError ? null : <div className="text-[12px] text-faint">loading mapping…</div>
          ) : (
            <>
              {pairs.length === 0 ? (
                <div className="text-[12px] text-faint">
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
                    className="h-8 w-full min-w-0 rounded-lg border border-line bg-surface-2 px-2.5 font-mono text-[12px] text-fg placeholder:text-faint focus:border-[var(--color-accent)]/60 focus:outline-none"
                  />
                  <input
                    list={listId}
                    value={pair.to}
                    disabled={busy}
                    placeholder="provider/model"
                    aria-label={`mapping ${index + 1} to`}
                    onChange={(e) => setPair(index, { to: e.target.value })}
                    className="h-8 w-full min-w-0 rounded-lg border border-line bg-surface-2 px-2.5 font-mono text-[12px] text-fg placeholder:text-faint focus:border-[var(--color-accent)]/60 focus:outline-none"
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
                  className="text-[12px] text-muted underline underline-offset-2 hover:text-fg"
                >
                  add mapping
                </button>
              </div>
            </>
          )}

          {problem ? (
            <span role="alert" className="text-[12px] text-neg">
              {problem}
            </span>
          ) : null}
        </section>

        <section className="flex min-w-0 flex-col gap-2">
          <div className="flex items-baseline justify-between gap-3">
            <span className="text-[11px] font-medium uppercase tracking-[0.08em] text-faint">
              guardrails
            </span>
            <span className="text-[11px] text-faint tnum">
              {blocked.length}/{MAX_BLOCKED_TERMS} blocked
            </span>
          </div>
          <span className="text-[11px] text-faint">
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
            <span className="text-[11px] font-medium uppercase tracking-[0.08em] text-faint">
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
                className="h-8 w-full min-w-0 rounded-lg border border-line bg-surface-2 px-2.5 text-[13px] text-fg placeholder:text-faint focus:border-[var(--color-accent)]/60 focus:outline-none"
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
              <div className="text-[12px] text-faint">
                none — every prompt passes through
              </div>
            ) : (
              <div className="flex flex-wrap gap-1.5">
                {blocked.map((phrase) => (
                  <span
                    key={phrase}
                    className="inline-flex max-w-full items-center gap-1 rounded-md border border-line bg-surface-2 px-1.5 py-0.5 text-[11px] text-muted"
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

          <label className="flex items-start gap-2 text-[13px] text-fg">
            <input
              type="checkbox"
              checked={scanOutput}
              disabled={busy}
              aria-label="scan responses"
              onChange={(e) => setScanOutput(e.target.checked)}
              className="mt-0.5 h-3.5 w-3.5 rounded border border-line-strong bg-[var(--color-surface-2)] accent-[var(--color-accent)]"
            />
            <span>
              scan responses
              <span className="mt-0.5 block text-[11px] text-faint">
                a matching response is refused before it reaches the client, and a stream is
                cut with an error instead of completing. Needs at least one blocked phrase.
              </span>
            </span>
          </label>

          {guardProblem ? (
            <span role="alert" className="text-[12px] text-neg">
              {guardProblem}
            </span>
          ) : null}
        </section>

        <div className="flex items-center justify-between gap-3 text-[11px] text-faint">
          <span className="font-mono">{row.id}</span>
          <span>0 or empty limits mean unlimited, except max prompt characters</span>
        </div>

        {failure ? <InlineError error={failure} /> : null}
      </div>
    </Modal>
  );
}
