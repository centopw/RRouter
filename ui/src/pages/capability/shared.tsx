import { useEffect, useId, useMemo, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api";
import {
  qk,
  useCapabilities,
  useModels,
  type Capabilities,
  type CapabilityName,
  type CapabilityProvider,
} from "../../lib/queries";
import {
  AlertBanner,
  Badge,
  Button,
  Card,
  CardFooter,
  CardHeader,
  ErrorNote,
  Field,
  PageHeader,
  Text,
  control,
  cx,
} from "../../components/ui";
import { DataTable, type Column } from "../../components/table";
import { GATEWAY_SETTINGS_KEY } from "../settings/gateway-settings";

const MAX_DEFAULT_LENGTH = 200;

/** Stable empty list so the memo below does not re-run on every render. */
const NO_PROVIDERS: CapabilityProvider[] = [];

/** Where a `/v1` call finds its model, in the order the gateway tries it. */
const FALLBACK_ORDER = "request model/provider → stored default → 400";

export type CapabilityPageProps = {
  capability: CapabilityName;
  /** Page title, e.g. "Search". */
  title: string;
  /** The `/v1` endpoint this page's default backs, e.g. `POST /v1/search`. */
  endpoint: string;
  /**
   * How the registry established these providers: `serving` when the capability is
   * explicit, `eligible` when only the wire dialect matches and the upstream still has
   * to expose the endpoint itself.
   */
  providerWording?: "serving" | "eligible";
  /** Explains the eligibility test; only rendered with `providerWording="eligible"`. */
  eligibilityNote?: string;
};

/**
 * One layout for the three capability defaults: a single stored ref, a free-text input
 * with suggestions, and the read-only providers that can serve it.
 */
export function CapabilityPage({
  capability,
  title,
  endpoint,
  providerWording = "serving",
  eligibilityNote,
}: CapabilityPageProps) {
  const providerHeading =
    providerWording === "eligible"
      ? `Providers eligible for ${title}`
      : `Providers serving ${title}`;
  const caps = useCapabilities();
  const client = useQueryClient();
  // Search suggests refs from the capability listing alone; the other two join it with
  // the model registry, so that read stays idle on the Search page.
  const models = useModels(capability !== "search");

  const providers = caps.data?.providers[capability] ?? NO_PROVIDERS;
  const stored = caps.data?.defaults[capability] ?? null;

  const [draft, setDraft] = useState("");
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState<null | "save" | "clear">(null);
  const [error, setError] = useState<unknown>(null);
  const [invalid, setInvalid] = useState<string | null>(null);
  const listId = useId();
  const busy = saving !== null;

  // A refetch never rewrites what the operator is typing; it only refreshes the field
  // while the draft is still the stored value.
  useEffect(() => {
    if (!dirty) setDraft(stored ?? "");
  }, [stored, dirty]);

  const suggestions = useMemo(() => {
    const refs = new Set<string>();
    if (capability === "search") {
      for (const provider of providers) {
        refs.add(provider.provider);
        if (provider.default_model) {
          refs.add(`${provider.provider}/${provider.default_model}`);
        }
      }
    } else {
      const connected = new Set(
        providers.filter((provider) => provider.connected).map((provider) => provider.provider),
      );
      for (const model of models.data ?? []) {
        if (connected.has(model.provider)) refs.add(`${model.provider}/${model.id}`);
      }
    }
    return [...refs].sort();
  }, [capability, providers, models.data]);

  /** PUT the one-key block; the server deep-merges it into the stored settings. */
  const write = async (mode: "save" | "clear", value: string | null) => {
    setSaving(mode);
    setError(null);
    try {
      await api.put("/api/settings", { capability_defaults: { [capability]: value } });
      client.setQueryData<Capabilities>(qk.capabilities, (old) =>
        old ? { ...old, defaults: { ...old.defaults, [capability]: value } } : old,
      );
      setDraft(value ?? "");
      setDirty(false);
      setInvalid(null);
      void client.invalidateQueries({ queryKey: qk.capabilities });
      void client.invalidateQueries({ queryKey: GATEWAY_SETTINGS_KEY });
    } catch (e) {
      // The field still holds the draft, so a failed save changes nothing upstream.
      setError(e);
    } finally {
      setSaving(null);
    }
  };

  const submit = () => {
    const next = draft.trim();
    if (next.length === 0) {
      setInvalid("Enter a provider or provider/model.");
      return;
    }
    if (next.length > MAX_DEFAULT_LENGTH) {
      setInvalid(`Keep the default to ${MAX_DEFAULT_LENGTH} characters or fewer.`);
      return;
    }
    setInvalid(null);
    void write("save", next);
  };

  const columns: Array<Column<CapabilityProvider>> = [
    {
      key: "provider",
      title: "Provider",
      width: "38%",
      render: (row) => (
        <span className="flex min-w-0 items-baseline gap-2">
          <span className="min-w-0 truncate">{row.display}</span>
          <span className="min-w-0 truncate font-mono text-caption text-faint">{row.provider}</span>
        </span>
      ),
      cellTitle: (row) => `${row.display} (${row.provider})`,
    },
    {
      key: "default_model",
      title: "Default model",
      render: (row) =>
        row.default_model ? (
          <span className="font-mono text-small">{row.default_model}</span>
        ) : (
          <span className="text-faint">—</span>
        ),
      cellTitle: (row) => row.default_model ?? "—",
    },
    {
      key: "accounts",
      title: "Accounts",
      align: "right",
      width: "7rem",
      render: (row) => <span className="tnum">{row.accounts}</span>,
    },
    {
      key: "connected",
      title: "State",
      width: "9.5rem",
      render: (row) => (
        <Badge tone={row.connected ? "pos" : "neutral"}>
          {row.connected ? "Connected" : "Not connected"}
        </Badge>
      ),
    },
  ];

  return (
    <>
      <PageHeader
        title={title}
        subtitle={`The stored default backs ${endpoint}. It applies only when the client omits a model and never overrides one.`}
      />

      {caps.isError ? (
        <div className="mb-4">
          <ErrorNote
            error={caps.error}
            title="Could not load capability defaults"
            onRetry={() => void caps.refetch()}
          />
        </div>
      ) : null}
      {error ? (
        <AlertBanner
          tone="neg"
          title="Could not save the default"
          onDismiss={() => setError(null)}
        >
          {error instanceof Error ? error.message : String(error)}
        </AlertBanner>
      ) : null}

      <Card>
        <CardHeader title="Default" />
        <div className="flex flex-col gap-3 px-5 pb-4">
          <Field
            label="Default provider or model"
            hint={`Leave unset to require the client to send one. Up to ${MAX_DEFAULT_LENGTH} characters.`}
            error={invalid ?? undefined}
          >
            <input
              list={listId}
              value={draft}
              disabled={busy || caps.isPending}
              placeholder={capability === "search" ? "provider or provider/model" : "provider/model"}
              autoComplete="off"
              spellCheck={false}
              className={cx(control, "font-mono text-small")}
              onChange={(event) => {
                setDraft(event.target.value);
                setDirty(true);
                setInvalid(null);
              }}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  submit();
                }
              }}
            />
          </Field>
          <datalist id={listId}>
            {suggestions.map((ref) => (
              <option key={ref} value={ref} />
            ))}
          </datalist>
          <Text as="p" variant="caption" tone="muted">
            Current default:{" "}
            {stored ? (
              <span className="font-mono">{stored}</span>
            ) : (
              <span>none — a client must send provider/model</span>
            )}
          </Text>
        </div>
        <CardFooter>
          <Button
            variant="ghost"
            className="text-muted"
            disabled={busy || (stored === null && draft.trim() === "")}
            loading={saving === "clear"}
            onClick={() => void write("clear", null)}
          >
            Clear
          </Button>
          <Button
            variant="primary"
            disabled={busy || caps.isPending}
            loading={saving === "save"}
            onClick={submit}
          >
            Save
          </Button>
        </CardFooter>
      </Card>

      <Card className="mt-4">
        <CardHeader title={providerHeading} />
        {eligibilityNote ? (
          <Text as="p" variant="caption" tone="muted" className="px-5 pb-3">
            {eligibilityNote}
          </Text>
        ) : null}
        <DataTable
          columns={columns}
          rows={caps.data ? providers : undefined}
          rowKey={(row) => row.provider}
          loading={caps.isPending}
          tableLayout="fixed"
          caption={providerHeading}
          scrollLabel={providerHeading}
          empty={
            providerWording === "eligible" ? (
              <span>
                No compiled provider is eligible for this capability. A client must still pin{" "}
                <span className="font-mono">provider/model</span> on every request.
              </span>
            ) : (
              <span>
                No compiled provider serves this capability. A client must still pin{" "}
                <span className="font-mono">provider/model</span> on every request.
              </span>
            )
          }
        />
      </Card>

      <Text as="p" variant="caption" tone="faint" className="mt-3">
        {endpoint} resolves a call in this order: {FALLBACK_ORDER}.
      </Text>
    </>
  );
}
