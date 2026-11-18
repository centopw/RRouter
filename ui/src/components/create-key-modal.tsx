import { useRef, useState } from "react";
import { Link } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  Button,
  CopyChip,
  ErrorNote,
  Field,
  Input,
  Modal,
  Segmented,
  Select,
} from "./ui";
import { Checkbox, SectionLabel } from "./atoms";
import { ModelPicker } from "./model-picker";
import {
  api,
  type BudgetWindow,
  type KeyResult,
  type KeyScope,
  type NewKey,
  type UpdateKey,
} from "../lib/api";
import { parseUsd } from "../lib/format";
import { qk, useTeams } from "../lib/queries";

function intLimitProblem(value: string): string | null {
  const text = value.trim();
  if (text === "") return null;
  if (!/^\d+$/.test(text)) return "Enter a whole number, or leave empty for unlimited.";
  const parsed = Number(text);
  if (!Number.isSafeInteger(parsed) || parsed < 0) {
    return "Enter a non-negative whole number.";
  }
  return null;
}

/**
 * Configure a key before it exists; the plaintext is revealed inside this drawer once.
 * Shared by the API-keys page and Quick start so both flows configure the same fields.
 */
export function CreateKeyModal({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  /** Called with the minted key when the operator finishes the success drawer. */
  onCreated: (created: NewKey) => void;
}) {
  const client = useQueryClient();
  const formRef = useRef<HTMLDivElement>(null);
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
  const [touched, setTouched] = useState({
    rpm: false,
    tpm: false,
    tokens: false,
    budget: false,
  });
  const [submitted, setSubmitted] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<unknown>(null);
  // Preserved after POST /api/keys succeeds so a PATCH retry never mints a duplicate key.
  const [minted, setMinted] = useState<NewKey | null>(null);
  const [created, setCreated] = useState<NewKey | null>(null);

  const rpmProblem = intLimitProblem(rpm);
  const tpmProblem = intLimitProblem(tpm);
  const tokensProblem = intLimitProblem(tokens);
  const budgetParsed = parseUsd(budget);
  const budgetProblem = "problem" in budgetParsed ? budgetParsed.problem : null;

  const showRpmError = (touched.rpm || submitted) && rpmProblem !== null;
  const showTpmError = (touched.tpm || submitted) && tpmProblem !== null;
  const showTokensError = (touched.tokens || submitted) && tokensProblem !== null;
  const showBudgetError = (touched.budget || submitted) && budgetProblem !== null;

  const submit = async () => {
    if (busy) return;
    setSubmitted(true);
    setTouched({ rpm: true, tpm: true, tokens: true, budget: true });
    const firstInvalidLabel = rpmProblem
      ? "requests per minute"
      : tpmProblem
        ? "tokens per minute"
        : tokensProblem
          ? "token budget"
          : budgetProblem
            ? "spend budget"
            : null;
    if (firstInvalidLabel) {
      formRef.current?.querySelector<HTMLInputElement>(`[aria-label="${firstInvalidLabel}"]`)?.focus();
      return;
    }

    setBusy(true);
    setFailure(null);
    try {
      let key = minted;
      if (!key) {
        key = await api.post<NewKey>("/api/keys", {
          name: name.trim() || null,
          team_id: team === "" ? null : team,
        });
        setMinted(key);
        void client.invalidateQueries({ queryKey: qk.keys });
        void client.invalidateQueries({ queryKey: qk.system });
      }
      const patch: UpdateKey = {
        ...(minted ? { name: name.trim() || null, team_id: team === "" ? null : team } : {}),
        scope,
        rpm: rpm.trim() === "" ? 0 : Number(rpm.trim()),
        tpm: tpm.trim() === "" ? 0 : Number(tpm.trim()),
        budget_tokens: tokens.trim() === "" ? 0 : Number(tokens.trim()),
        budget_window: reset,
        allowed_models: models.length > 0 ? models : null,
        // Guardrails are a separate policy document; a fresh key has none, so the toggle
        // is applied with the same PATCH as the limits.
        guardrails: redact ? { blocked: [], scan_output: false, redact_pii: true } : null,
        budget_micros: "micros" in budgetParsed ? budgetParsed.micros : 0,
      };
      await api.patch<KeyResult>(`/api/keys/${key.id}`, patch);
      setCreated(key);
    } catch (e) {
      setFailure(e);
    } finally {
      setBusy(false);
    }
  };

  const finish = () => {
    if (created) onCreated(created);
    else if (minted) onCreated(minted);
    else onClose();
  };

  if (created) {
    return (
      <Modal
        title="Key created"
        open
        onClose={finish}
        description="Copy it now — the secret is stored hashed and won't be shown again."
        footer={
          <>
            <Link
              to="/quickstart"
              onClick={finish}
              className="mr-auto text-small font-medium text-muted hover:text-fg hover:underline"
            >
              Quick start with this key →
            </Link>
            <Button variant="primary" onClick={finish}>
              Done
            </Button>
          </>
        }
      >
        <div className="flex flex-col gap-3">
          <CopyChip value={created.key} label="API key" className="w-full" />
          <p className="text-small text-muted">
            Clients send it as{" "}
            <code className="font-mono text-caption">Authorization: Bearer …</code> against the
            endpoint on the Keys page.
          </p>
        </div>
      </Modal>
    );
  }

  return (
    <Modal
      title="Create API key"
      open
      onClose={minted ? finish : onClose}
      description="Limits are optional; empty means unlimited. The key is minted only when you create it."
      footer={
        minted ? (
          <>
            <Button onClick={finish} disabled={busy}>
              Close without limits
            </Button>
            <Button variant="primary" onClick={submit} disabled={busy}>
              {busy ? "Saving limits…" : "Retry saving limits"}
            </Button>
          </>
        ) : (
          <>
            <Button onClick={onClose} disabled={busy}>
              Cancel
            </Button>
            <Button variant="primary" onClick={submit} disabled={busy}>
              {busy ? "Creating…" : "Create key"}
            </Button>
          </>
        )
      }
    >
      <div ref={formRef} className="flex flex-col gap-4">
        {minted ? (
          <div
            role="status"
            className="flex flex-col gap-2 rounded-[var(--radius-card)] border border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] px-4 py-3 text-small text-[var(--color-warn-ink)]"
          >
            <span className="font-semibold">Key created, but limits failed to save</span>
            <span>
              Copy your API key now — the plaintext is only shown once. Retrying applies your
              settings to this key without creating a duplicate.
            </span>
            <CopyChip value={minted.key} label="API key" className="w-full bg-surface" />
          </div>
        ) : null}
        <Field label="Name" hint="How this key appears in logs and budgets">
          <Input
            value={name}
            onChange={setName}
            placeholder="production-app"
            ariaLabel="key name"
            disabled={busy}
          />
        </Field>
        <Field label="Scope" hint="Admin keys manage the gateway and cannot spend tokens">
          <Segmented
            options={[
              { id: "inference", label: "Inference" },
              { id: "admin", label: "Admin" },
            ]}
            value={scope}
            onChange={(v) => {
              if (!busy) setScope(v as KeyScope);
            }}
            ariaLabel="key scope"
          />
        </Field>
        <Field
          label="Team"
          hint="Instance-wide unless assigned; a team owns the key's spend report"
        >
          <Select value={team} onChange={setTeam} ariaLabel="key team" disabled={busy}>
            <option value="">Instance-wide</option>
            {(teams.data ?? []).map((team) => (
              <option key={team.id} value={team.id}>
                {team.name}
              </option>
            ))}
          </Select>
        </Field>
        <SectionLabel>Limits (optional)</SectionLabel>
        <div className="grid grid-cols-2 gap-3">
          <div className="flex min-w-0 flex-col gap-1">
            <Field label="Requests / min" hint="Empty = unlimited">
              <Input
                type="number"
                value={rpm}
                onChange={setRpm}
                onBlur={() => setTouched((prev) => ({ ...prev, rpm: true }))}
                ariaLabel="requests per minute"
                className="tnum"
                disabled={busy}
              />
            </Field>
            {showRpmError ? (
              <span role="alert" className="text-caption text-neg">
                {rpmProblem}
              </span>
            ) : null}
          </div>
          <div className="flex min-w-0 flex-col gap-1">
            <Field label="Tokens / min" hint="Empty = unlimited">
              <Input
                type="number"
                value={tpm}
                onChange={setTpm}
                onBlur={() => setTouched((prev) => ({ ...prev, tpm: true }))}
                ariaLabel="tokens per minute"
                className="tnum"
                disabled={busy}
              />
            </Field>
            {showTpmError ? (
              <span role="alert" className="text-caption text-neg">
                {tpmProblem}
              </span>
            ) : null}
          </div>
          <div className="flex min-w-0 flex-col gap-1">
            <Field label="Token budget" hint="Empty = unlimited">
              <Input
                type="number"
                value={tokens}
                onChange={setTokens}
                onBlur={() => setTouched((prev) => ({ ...prev, tokens: true }))}
                ariaLabel="token budget"
                className="tnum"
                disabled={busy}
              />
            </Field>
            {showTokensError ? (
              <span role="alert" className="text-caption text-neg">
                {tokensProblem}
              </span>
            ) : null}
          </div>
          <div className="flex min-w-0 flex-col gap-1">
            <Field label="Spend budget (USD)" hint="Empty = unlimited">
              <Input
                value={budget}
                onChange={setBudget}
                onBlur={() => setTouched((prev) => ({ ...prev, budget: true }))}
                placeholder="5.00"
                ariaLabel="spend budget"
                className="tnum"
                disabled={busy}
              />
            </Field>
            {showBudgetError ? (
              <span role="alert" className="text-caption text-neg">
                {budgetProblem}
              </span>
            ) : null}
          </div>
        </div>
        <Field label="Budget window" hint="When the counters reset">
          <Select
            value={reset}
            onChange={(v) => setReset(v as BudgetWindow)}
            ariaLabel="budget window"
            disabled={busy}
          >
            <option value="total">Total (never)</option>
            <option value="daily">Daily</option>
            <option value="weekly">Weekly</option>
            <option value="monthly">Monthly</option>
          </Select>
        </Field>
        <SectionLabel>Access</SectionLabel>
        <ModelPicker selected={models} onChange={setModels} disabled={busy} />

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
              Masks email addresses, phone numbers, card numbers and credential-shaped strings
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
