import { useState } from "react";
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
import { Checkbox } from "./atoms";
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
import { useTeams } from "../lib/queries";

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

  const finish = () => {
    if (created) onCreated(created);
    else onClose();
  };

  if (created) {
    return (
      <Modal
        title="Key created"
        open
        onClose={finish}
        description="This is the only time the plaintext is shown. Store it before closing."
        footer={
          <Button variant="primary" onClick={finish}>
            done
          </Button>
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
          <Input
            value={name}
            onChange={setName}
            placeholder="production-app"
            ariaLabel="key name"
          />
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
        <Field
          label="team"
          hint="instance-wide unless assigned; a team owns the key's spend report"
        >
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
