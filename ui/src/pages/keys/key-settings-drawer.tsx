import { useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import {
  Button,
  CopyChip,
  ErrorNote,
  Field,
  Input,
  Modal,
  Segmented,
  Select,
  TabPanel,
  TabStrip,
  control,
  cx,
} from "../../components/ui";
import { Checkbox, SectionLabel } from "../../components/atoms";
import { ModelPicker } from "../../components/model-picker";
import { compact, parseUsd, usd, usdInput } from "../../lib/format";
import {
  useKeyModels,
  useModels,
  usePlaygroundKey,
  useTeams,
  useUpdateKey,
  useUpdateKeyModels,
} from "../../lib/queries";
import {
  ApiError,
  type BudgetWindow,
  type Guardrails,
  type KeyModelMap,
  type KeyResult,
  type KeyScope,
  type UpdateKey,
} from "../../lib/api";
import { SpendProgressBar, WINDOW_NOW } from "./spend-cell";

/** The server's policy limits; enforcing them here keeps a 400 from ever being the first hint. */
export const MAX_KEY_NAME = 100;
export const MAX_BLOCKED_TERMS = 200;
export const MAX_TERM_CHARS = 200;

/** Inference is the default: switching a key to admin is the deliberate move. */
export const SCOPES: ReadonlyArray<{ id: KeyScope; label: string }> = [
  { id: "inference", label: "Inference" },
  { id: "admin", label: "Admin" },
];

type SettingsTab = "identity" | "budgets" | "models";

const SETTINGS_TABS: ReadonlyArray<{ id: SettingsTab; label: string }> = [
  { id: "identity", label: "Identity & Rate Limits" },
  { id: "budgets", label: "Spend & Token Budgets" },
  { id: "models", label: "Model Access & Guardrails" },
];

/** Empty means unlimited; a bare `0` is the API's own spelling of the same thing. */
export function toCount(value: string): number {
  const n = Math.floor(Number(value));
  return Number.isFinite(n) && n > 0 ? n : 0;
}

/** `YYYY-MM-DD` in the operator's own timezone. */
export function toDateInput(ts: number | null): string {
  if (ts === null) return "";
  const d = new Date(ts * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** A date picker means "good through the end of that day", never midnight. */
export function fromDateInput(value: string): number | null {
  if (value === "") return null;
  const [y, m, d] = value.split("-").map(Number);
  if (y === undefined || m === undefined || d === undefined) return null;
  const at = new Date(y, m - 1, d, 23, 59, 59, 0);
  return Number.isNaN(at.getTime()) ? null : Math.floor(at.getTime() / 1000);
}

export function intLimitProblem(value: string): string | null {
  const text = value.trim();
  if (text === "") return null;
  if (!/^\d+$/.test(text)) return "Enter a whole number, or leave empty for unlimited.";
  const parsed = Number(text);
  if (!Number.isSafeInteger(parsed) || parsed < 0) {
    return "Enter a non-negative whole number.";
  }
  return null;
}

/** The API's machine-readable 403/429 reason, when the body carries one. */
export function errorCode(error: unknown): string | null {
  if (!(error instanceof ApiError)) return null;
  const body: unknown = error.body;
  if (typeof body !== "object" || body === null || !("error" in body)) return null;
  const inner = body.error;
  if (typeof inner !== "object" || inner === null || !("code" in inner)) return null;
  return typeof inner.code === "string" ? inner.code : null;
}

/** Rows are compared after trimming, so a stray space is not a change. */
export function mapKey(pairs: KeyModelMap[]): string {
  return JSON.stringify(pairs.map((p) => [p.from.trim(), p.to]));
}

/**
 * `from` must be present and unique; `to` must be a registry ref. `refs === null` means the
 * registry has not loaded, so membership cannot be judged yet; an untouched `to` stays
 * legal even after the registry dropped it, so a stale row never blocks a limits save.
 */
export function validateMap(
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

/** Build the server's sparse guardrails document, or return an inline validation problem. */
export function buildGuardrailsPolicy(input: {
  maxPrompt: string;
  blocked: string[];
  scanOutput: boolean;
  redact: boolean;
}): { guardrails: Guardrails | null } | { problem: string } {
  // `0` is a real cap here ("no prompt at all"), so empty must stay absent rather than
  // collapse into the 0-is-unlimited spelling the other limits use.
  const capText = input.maxPrompt.trim();
  const cap = capText === "" ? undefined : Number(capText);
  if (cap !== undefined && (!Number.isInteger(cap) || cap < 0)) {
    return { problem: "max prompt characters: whole number, or empty for unlimited" };
  }
  const guardrails: Guardrails | null =
    cap === undefined && input.blocked.length === 0 && !input.scanOutput && !input.redact
      ? null
      : {
          ...(cap === undefined ? {} : { max_prompt_chars: cap }),
          blocked: input.blocked,
          scan_output: input.scanOutput,
          redact_pii: input.redact,
        };
  return { guardrails };
}

/** Server errors carry `{error:{message,code}}`; both belong next to the form, not in a banner. */
export function InlineError({ error }: { error: unknown }) {
  const message = error instanceof Error ? error.message : String(error);
  const code = errorCode(error);
  return (
    <div
      role="alert"
      className="rounded-[var(--radius-card)] border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-3 py-2 text-small"
    >
      <span className="break-words text-neg">{message}</span>
      {code ? <span className="ml-2 font-mono text-caption text-faint">{code}</span> : null}
    </div>
  );
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
      className={cx(control, "tnum")}
    />
  );
}

/**
 * The playground key's plaintext. The only endpoint that returns it is the ensure route,
 * so this modal asks for it rather than reading it off a list row.
 */
export function PlaygroundKeyModal({ onClose }: { onClose: () => void }) {
  const key = usePlaygroundKey();
  return (
    <Modal
      title="Playground key"
      variant="dialog"
      width="sm"
      open
      onClose={onClose}
      description="Issued and kept by the gateway. It is accepted on /v1 only from this dashboard's playground, with an admin session."
      footer={
        <Button variant="primary" onClick={onClose}>
          Done
        </Button>
      }
    >
      <div className="flex flex-col gap-3">
        {key.isError ? (
          <ErrorNote error={key.error} onRetry={() => void key.refetch()} />
        ) : key.data ? (
          <CopyChip value={key.data.key} label="playground key" className="w-full" />
        ) : (
          <p className="text-small text-faint">minting the key…</p>
        )}
        <p className="text-small text-muted">
          Deleting the row revokes this key; the playground mints a new one on its next visit.
          Limits, models and guardrails stay editable here like any other key.
        </p>
      </div>
    </Modal>
  );
}

export function KeySettingsDrawer({ row, onClose }: { row: KeyResult; onClose: () => void }) {
  const [tab, setTab] = useState<SettingsTab>("identity");
  const [name, setName] = useState(row.name ?? "");
  const [active, setActive] = useState(row.is_active);
  const [scope, setScope] = useState<KeyScope>(row.scope);
  const [team, setTeam] = useState(row.team_id ?? "");
  const teams = useTeams();
  const [rpm, setRpm] = useState(row.rpm > 0 ? String(row.rpm) : "");
  const [tpm, setTpm] = useState(row.tpm > 0 ? String(row.tpm) : "");
  const [budget, setBudget] = useState(row.budget_tokens > 0 ? String(row.budget_tokens) : "");
  const [budgetUsd, setBudgetUsd] = useState(usdInput(row.budget_micros));
  const [limitsProblem, setLimitsProblem] = useState<{
    rpm: string | null;
    tpm: string | null;
    budget: string | null;
  }>({ rpm: null, tpm: null, budget: null });
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
  // One datalist per drawer: the key id already makes a unique, stable DOM id.
  const listId = `key-mappings-${row.id}`;

  useEffect(() => {
    if (baseline !== null || map.data === undefined) return;
    setPairs(map.data);
    setBaseline(map.data);
  }, [baseline, map.data]);

  const first = useRef<HTMLButtonElement>(null);
  // Modal focuses its own container in an effect that runs after this one; the
  // animation frame puts focus in the first control instead.
  useEffect(() => {
    const frame = window.requestAnimationFrame(() => first.current?.focus());
    return () => window.cancelAnimationFrame(frame);
  }, []);

  const patchKey = useUpdateKey();
  const putMap = useUpdateKeyModels(row.id);

  const isDirty = useMemo(() => {
    const initialMaxPrompt =
      row.guardrails?.max_prompt_chars !== undefined ? String(row.guardrails.max_prompt_chars) : "";
    return (
      name.trim() !== (row.name ?? "").trim() ||
      active !== row.is_active ||
      (row.kind !== "playground" && scope !== row.scope) ||
      team !== (row.team_id ?? "") ||
      expires !== toDateInput(row.expires_at) ||
      toCount(rpm) !== row.rpm ||
      toCount(tpm) !== row.tpm ||
      toCount(budget) !== row.budget_tokens ||
      budgetUsd.trim() !== usdInput(row.budget_micros) ||
      resets !== row.budget_window ||
      JSON.stringify(allowed) !== JSON.stringify(row.allowed_models ?? []) ||
      (baseline !== null && mapKey(pairs) !== mapKey(baseline)) ||
      maxPrompt.trim() !== initialMaxPrompt ||
      JSON.stringify(blocked) !== JSON.stringify(row.guardrails?.blocked ?? []) ||
      scanOutput !== (row.guardrails?.scan_output ?? false) ||
      redact !== (row.guardrails?.redact_pii ?? false)
    );
  }, [
    row,
    name,
    active,
    scope,
    team,
    expires,
    rpm,
    tpm,
    budget,
    budgetUsd,
    resets,
    allowed,
    baseline,
    pairs,
    maxPrompt,
    blocked,
    scanOutput,
    redact,
  ]);

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
    if ([...trimmedName].length > MAX_KEY_NAME) {
      setTab("identity");
      return;
    }
    const rpmErr = intLimitProblem(rpm);
    const tpmErr = intLimitProblem(tpm);
    const tokenErr = intLimitProblem(budget);
    setLimitsProblem({ rpm: rpmErr, tpm: tpmErr, budget: tokenErr });
    if (rpmErr || tpmErr) {
      setTab("identity");
      return;
    }
    const parsed = parseUsd(budgetUsd);
    if ("problem" in parsed) {
      setBudgetProblem(parsed.problem);
      setTab("budgets");
      return;
    }
    setBudgetProblem(null);
    if (tokenErr) {
      setTab("budgets");
      return;
    }
    const clean = pairs.map((p) => ({ from: p.from.trim(), to: p.to }));
    const invalid = validateMap(clean, refs, new Map((baseline ?? []).map((p) => [p.from, p.to])));
    if (invalid !== null) {
      setProblem(invalid);
      setTab("models");
      return;
    }
    setProblem(null);
    const policy = buildGuardrailsPolicy({ maxPrompt, blocked, scanOutput, redact });
    if ("problem" in policy) {
      setGuardProblem(policy.problem);
      setTab("models");
      return;
    }
    setGuardProblem(null);
    setFailure(null);
    setBusy(true);
    const patch: UpdateKey = {
      name: trimmedName === "" ? null : trimmedName,
      is_active: active,
      // A system key's scope is the gateway's decision; sending one is a 400 on admin.
      ...(row.kind === "playground" ? {} : { scope }),
      rpm: toCount(rpm),
      tpm: toCount(tpm),
      budget_tokens: toCount(budget),
      budget_micros: parsed.micros,
      budget_window: resets,
      allowed_models: allowed.length === 0 ? null : allowed,
      expires_at: fromDateInput(expires),
      guardrails: policy.guardrails,
      team_id: team === "" ? null : team,
    };
    void (async () => {
      // Limits first, then the map: a failed PUT keeps the drawer open with the error and
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

  const hasUsdBudget = row.budget_micros > 0;
  const hasTokenBudget = row.budget_tokens > 0;
  const usdShare = hasUsdBudget ? Math.min(100, (row.spent_micros / row.budget_micros) * 100) : 0;
  const tokShare = hasTokenBudget
    ? Math.min(100, (row.spent_tokens / row.budget_tokens) * 100)
    : 0;
  const currentReached =
    (hasUsdBudget && row.spent_micros >= row.budget_micros) ||
    (hasTokenBudget && row.spent_tokens >= row.budget_tokens);
  const currentWindowLabel = WINDOW_NOW[row.budget_window] ?? "total";

  return (
    <Modal
      title="Key settings"
      description={row.name ? `${row.name} · ${row.id}` : row.id}
      variant="drawer"
      width="lg"
      open
      dirty={isDirty}
      onClose={onClose}
      footer={
        <>
          <Link
            to="/quickstart"
            onClick={onClose}
            className="mr-auto text-small font-medium text-[var(--color-accent-text)] hover:underline"
          >
            Quick start with this key →
          </Link>
          <Button onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" disabled={busy || nameInvalid} onClick={submit}>
            {busy ? "Saving…" : "Save changes"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-5">
        <TabStrip
          options={SETTINGS_TABS}
          value={tab}
          onChange={setTab}
          ariaLabel="Key settings sections"
          className="max-w-full overflow-x-auto"
        >
          <TabPanel value="identity" className="mt-5 flex flex-col gap-5 focus:outline-none">
            <section className="flex min-w-0 flex-col gap-3">
              <SectionLabel>Identity &amp; access</SectionLabel>
              <div className="flex min-w-0 flex-col gap-1">
                <Field label="Name" hint="How this key appears in logs and budgets">
                  <Input
                    value={name}
                    onChange={(value) => {
                      setName(value);
                      setFailure(null);
                    }}
                    placeholder="empty = unnamed"
                    ariaLabel="key name"
                    invalid={nameInvalid}
                    disabled={busy}
                  />
                </Field>
                <span
                  role={nameInvalid ? "alert" : undefined}
                  className={cx("text-caption tnum", nameInvalid ? "text-neg" : "text-faint")}
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
                Active
                <span className="text-caption text-faint">
                  {active ? "accepting requests" : "every request is rejected with 403"}
                </span>
              </div>

              <div className="flex min-w-0 flex-col gap-1.5">
                <span className="text-small font-semibold text-ink-soft">Scope</span>
                {row.kind === "playground" ? (
                  <span className="text-small text-muted">
                    Inference, fixed — the playground key is accepted on /v1 only from the playground,
                    and cannot administer the gateway.
                  </span>
                ) : (
                  <>
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
                      Admin keys manage the gateway and cannot spend tokens; inference keys call /v1 and
                      obey limits and guardrails.
                    </span>
                  </>
                )}
              </div>

              <div className="grid gap-3 sm:grid-cols-2">
                <Field label="Team" hint="Instance-wide unless assigned">
                  <Select value={team} onChange={setTeam} ariaLabel="key team" disabled={busy}>
                    <option value="">Instance-wide</option>
                    {(teams.data ?? []).map((t) => (
                      <option key={t.id} value={t.id}>
                        {t.name}
                      </option>
                    ))}
                  </Select>
                </Field>

                <Field label="Expires" hint="Empty = never">
                  <DateInput
                    value={expires}
                    onChange={setExpires}
                    ariaLabel="expiry date"
                    disabled={busy}
                  />
                </Field>
              </div>
            </section>

            <section className="flex min-w-0 flex-col gap-3 border-t border-line pt-4">
              <SectionLabel>Rate limits</SectionLabel>
              <div className="grid grid-cols-2 gap-3">
                <div className="flex min-w-0 flex-col gap-1">
                  <Field label="Requests/min" hint="Empty = unlimited">
                    <Input
                      type="number"
                      value={rpm}
                      onChange={(v) => {
                        setRpm(v);
                        setLimitsProblem((prev) => ({ ...prev, rpm: intLimitProblem(v) }));
                      }}
                      placeholder="60"
                      ariaLabel="requests per minute"
                      invalid={limitsProblem.rpm !== null}
                      className="tnum"
                      disabled={busy}
                    />
                  </Field>
                  {limitsProblem.rpm ? (
                    <span role="alert" className="text-caption text-neg">
                      {limitsProblem.rpm}
                    </span>
                  ) : null}
                </div>
                <div className="flex min-w-0 flex-col gap-1">
                  <Field label="Tokens/min" hint="Empty = unlimited">
                    <Input
                      type="number"
                      value={tpm}
                      onChange={(v) => {
                        setTpm(v);
                        setLimitsProblem((prev) => ({ ...prev, tpm: intLimitProblem(v) }));
                      }}
                      placeholder="90000"
                      ariaLabel="tokens per minute"
                      invalid={limitsProblem.tpm !== null}
                      className="tnum"
                      disabled={busy}
                    />
                  </Field>
                  {limitsProblem.tpm ? (
                    <span role="alert" className="text-caption text-neg">
                      {limitsProblem.tpm}
                    </span>
                  ) : null}
                </div>
              </div>
            </section>
          </TabPanel>

          <TabPanel value="budgets" className="mt-5 flex flex-col gap-5 focus:outline-none">
            <section className="flex min-w-0 flex-col gap-3">
              <SectionLabel>Current window usage</SectionLabel>
              <div className="rounded-[var(--radius-card)] border border-line bg-surface-2 px-4 py-3">
                <div className="flex flex-wrap items-baseline justify-between gap-3">
                  <div className="flex items-baseline gap-1.5 tnum">
                    <span className={cx("text-lead font-semibold", currentReached ? "text-neg" : "text-fg")}>
                      {usd(row.spent_micros)}
                    </span>
                    {hasUsdBudget ? (
                      <span className="text-small text-faint">/ {usd(row.budget_micros)} USD</span>
                    ) : (
                      <span className="text-small text-faint">USD spent</span>
                    )}
                  </div>
                  <div className="text-small text-muted tnum">
                    {hasTokenBudget
                      ? `${compact(row.spent_tokens)} / ${compact(row.budget_tokens)} tokens`
                      : `${compact(row.spent_tokens)} tokens`}
                  </div>
                </div>
                {hasUsdBudget || hasTokenBudget ? (
                  <>
                    <SpendProgressBar
                      share={
                        hasUsdBudget && hasTokenBudget
                          ? Math.max(usdShare, tokShare)
                          : hasUsdBudget
                            ? usdShare
                            : tokShare
                      }
                      reached={currentReached}
                    />
                    <div className="mt-1.5 flex items-center justify-between text-caption">
                      <span className="text-faint">Window: {currentWindowLabel}</span>
                      {currentReached ? <span className="text-neg">budget reached</span> : null}
                    </div>
                  </>
                ) : (
                  <p className="mt-1 text-caption text-faint">
                    No spend or token budget configured ({currentWindowLabel}).
                  </p>
                )}
              </div>
            </section>

            <section className="flex min-w-0 flex-col gap-3 border-t border-line pt-4">
              <SectionLabel>Spend &amp; token budgets</SectionLabel>
              <div className="grid grid-cols-2 gap-3">
                <div className="flex min-w-0 flex-col gap-1">
                  <Field label="Token budget" hint="Empty = unlimited">
                    <Input
                      type="number"
                      value={budget}
                      onChange={(v) => {
                        setBudget(v);
                        setLimitsProblem((prev) => ({ ...prev, budget: intLimitProblem(v) }));
                      }}
                      placeholder="5000000"
                      ariaLabel="token budget"
                      invalid={limitsProblem.budget !== null}
                      className="tnum"
                      disabled={busy}
                    />
                  </Field>
                  {limitsProblem.budget ? (
                    <span role="alert" className="text-caption text-neg">
                      {limitsProblem.budget}
                    </span>
                  ) : null}
                </div>
                <div className="flex min-w-0 flex-col gap-1">
                  <Field label="Budget (USD)" hint="Empty = unlimited">
                    <Input
                      value={budgetUsd}
                      onChange={(v) => {
                        setBudgetUsd(v);
                        setBudgetProblem(null);
                      }}
                      placeholder="1.00"
                      ariaLabel="budget in USD"
                      invalid={budgetProblem !== null}
                      className="tnum"
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

              <Field
                label="Resets"
                hint="Spend shown is for the current window; budgets are enforced against it"
              >
                <Select
                  value={resets}
                  onChange={(v) => setResets(v as BudgetWindow)}
                  ariaLabel="budget reset window"
                  disabled={busy}
                >
                  <option value="total">Total (never)</option>
                  <option value="daily">Daily</option>
                  <option value="weekly">Weekly</option>
                  <option value="monthly">Monthly</option>
                </Select>
              </Field>
            </section>
          </TabPanel>

          <TabPanel value="models" className="mt-5 flex flex-col gap-5 focus:outline-none">
            <section className="flex min-w-0 flex-col gap-3">
              <SectionLabel>Model access &amp; rewrites</SectionLabel>
              <ModelPicker selected={allowed} onChange={setAllowed} disabled={busy} />

              <div className="flex min-w-0 flex-col gap-2">
                <div className="flex items-baseline justify-between gap-3">
                  <span className="text-small font-semibold text-ink-soft">Model mapping</span>
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
                          className={cx(control, "font-mono text-small")}
                        />
                        <input
                          list={listId}
                          value={pair.to}
                          disabled={busy}
                          placeholder="provider/model"
                          aria-label={`mapping ${index + 1} to`}
                          onChange={(e) => setPair(index, { to: e.target.value })}
                          className={cx(control, "font-mono text-small")}
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
                        Add mapping
                      </button>
                    </div>
                  </>
                )}

                {problem ? (
                  <span role="alert" className="text-small text-neg">
                    {problem}
                  </span>
                ) : null}
              </div>
            </section>

            <section className="flex min-w-0 flex-col gap-3 border-t border-line pt-4">
              <div className="flex items-baseline justify-between gap-3">
                <SectionLabel>Guardrails</SectionLabel>
                <span className="text-caption text-faint tnum">
                  {blocked.length}/{MAX_BLOCKED_TERMS} blocked
                </span>
              </div>
              <span className="text-caption text-faint">
                Case-insensitive substring match, no regex. A hit refuses the request; with
                response scanning it refuses the reply too.
              </span>

              <Field label="Max prompt characters" hint="Empty = unlimited; 0 refuses every prompt">
                <Input
                  type="number"
                  value={maxPrompt}
                  onChange={(v) => {
                    setMaxPrompt(v);
                    setGuardProblem(null);
                  }}
                  placeholder="unlimited"
                  ariaLabel="max prompt characters"
                  className="tnum"
                  disabled={busy}
                />
              </Field>

              <div className="flex min-w-0 flex-col gap-1.5">
                <span className="text-small font-semibold text-ink-soft">Blocked phrases</span>
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
                    className={control}
                  />
                  <Button
                    type="submit"
                    disabled={
                      busy || term.trim() === "" || blocked.length >= MAX_BLOCKED_TERMS
                    }
                  >
                    Add
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
                  Scan responses
                  <span className="mt-0.5 block text-caption text-faint">
                    A matching response is refused before it reaches the client, and a stream is
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
                  Redact PII
                  <span className="mt-0.5 block text-caption text-faint">
                    Masks email addresses, phone numbers, card numbers and credential-shaped
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
          </TabPanel>
        </TabStrip>

        <div className="flex items-center justify-between gap-3 border-t border-line pt-3 text-caption text-faint">
          <span className="font-mono">{row.id}</span>
          <span>0 or empty limits mean unlimited, except max prompt characters</span>
        </div>

        {failure ? <InlineError error={failure} /> : null}
      </div>
    </Modal>
  );
}
