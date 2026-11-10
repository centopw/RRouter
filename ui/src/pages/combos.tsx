import { useMemo, useState } from "react";
import { PencilSimple, Trash } from "@phosphor-icons/react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import {
  Badge,
  Button,
  Card,
  CardFooter,
  CardHeader,
  ConfirmDialog,
  ErrorNote,
  Field,
  Input,
  PageHeader,
  Select,
  cx,
} from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { ModelChain } from "../components/model-select";
import { IconButton } from "../components/atoms";
import { qk, useCombos, useModels } from "../lib/queries";
import { api, type Combo, type ComboStrategy } from "../lib/api";

/** The three strategies, in the order the form and the docs read them. */
const STRATEGIES: Array<{
  id: ComboStrategy;
  label: string;
  badge: string;
  body: string;
  /** How the delete confirmation names the thing that stops existing. */
  noun: string;
}> = [
  {
    id: "fallback",
    label: "Fallback",
    badge: "fallback",
    body: "Try the models in order; the next one takes over when the previous fails.",
    noun: "fallback chain",
  },
  {
    id: "round_robin",
    label: "Round robin",
    badge: "round robin",
    body: "Rotate which model starts each request; the rest stay the fallback order.",
    noun: "round-robin chain",
  },
  {
    id: "smart",
    label: "Smart routing",
    badge: "smart",
    body: "A router model picks the first candidate; the rest stay the fallback order.",
    noun: "smart chain",
  },
];

function strategyOf(combo: Combo): ComboStrategy {
  return combo.strategy ?? "fallback";
}

/**
 * What is wrong with the form, if anything. The same rules the API enforces, checked here
 * so the operator sees the problem next to the field rather than as a 400.
 */
function formProblem(
  name: string,
  models: string[],
  strategy: ComboStrategy,
  routerModel: string,
): string | null {
  const trimmed = name.trim();
  if (trimmed === "") return "Name the combo before saving.";
  if (models.filter(Boolean).length === 0) return "Add at least one model to the chain.";
  if (models.includes(trimmed)) return "A combo cannot list itself as a candidate.";
  if (strategy === "smart" && routerModel === "") return "Smart routing needs a router model.";
  if (strategy === "smart" && routerModel === trimmed) {
    return "A combo cannot use itself as its router.";
  }
  return null;
}

export default function Combos() {
  const combos = useCombos();
  const models = useModels();
  const client = useQueryClient();
  const [name, setName] = useState("");
  const [chain, setChain] = useState<string[]>([""]);
  const [strategy, setStrategy] = useState<ComboStrategy>("fallback");
  const [routerModel, setRouterModel] = useState("");
  const [pending, setPending] = useState<Combo | null>(null);
  const [editing, setEditing] = useState(false);

  const invalidate = () => {
    void client.invalidateQueries({ queryKey: qk.combos });
    void client.invalidateQueries({ queryKey: qk.models });
    void client.invalidateQueries({ queryKey: qk.system });
  };

  const reset = () => {
    setName("");
    setChain([""]);
    setStrategy("fallback");
    setRouterModel("");
    setEditing(false);
  };

  const save = useMutation({
    mutationFn: () =>
      api.post<Combo>("/api/combos", {
        name: name.trim(),
        models: chain.filter(Boolean),
        strategy,
        // Only a smart combo carries a router; the API treats the field as absent otherwise.
        ...(strategy === "smart" ? { router_model: routerModel } : {}),
      }),
    onSuccess: () => {
      reset();
      invalidate();
    },
  });

  const remove = useMutation({
    mutationFn: (combo: string) =>
      api.del<{ deleted: boolean }>(`/api/combos?name=${encodeURIComponent(combo)}`),
    onSuccess: invalidate,
  });

  const routers = useMemo(
    () => [...(models.data ?? [])].sort((a, b) => a.ref.localeCompare(b.ref)),
    [models.data],
  );

  const chosen = STRATEGIES.find((entry) => entry.id === strategy) ?? STRATEGIES[0]!;
  const pendingStrategy = pending ? strategyOf(pending) : "fallback";
  const pendingNoun =
    STRATEGIES.find((entry) => entry.id === pendingStrategy)?.noun ?? "fallback chain";

  const problem = formProblem(name, chain, strategy, routerModel);

  const columns: Array<Column<Combo>> = [
    {
      key: "name",
      title: "name",
      render: (r) => {
        const kind = strategyOf(r);
        const entry = STRATEGIES.find((candidate) => candidate.id === kind);
        return (
          <span className="flex min-w-0 flex-col gap-1">
            <span className="block truncate font-mono text-small" title={r.name}>
              {r.name}
            </span>
            {/* Below sm the strategy and candidates columns are gone; the row keeps both. */}
            <span className="flex flex-wrap items-center gap-1 sm:hidden">
              <Badge tone={kind === "fallback" ? "neutral" : "accent"}>{entry?.badge ?? "fallback"}</Badge>
              {kind === "smart" && r.router_model ? (
                <span className="truncate font-mono text-caption text-faint">{r.router_model}</span>
              ) : null}
            </span>
            <span className="flex flex-wrap items-center gap-1 sm:hidden">
              {r.models.map((m, i) => (
                <span key={`${m}-${i}`} className="flex items-center gap-1">
                  {i > 0 ? <span className="text-faint">→</span> : null}
                  <span className="rounded border border-line-strong bg-surface-2 px-1.5 py-0.5 font-mono text-caption text-muted">
                    {m}
                  </span>
                </span>
              ))}
            </span>
          </span>
        );
      },
    },
    {
      key: "strategy",
      title: "strategy",
      className: "hidden sm:table-cell",
      render: (r) => {
        const kind = strategyOf(r);
        const entry = STRATEGIES.find((candidate) => candidate.id === kind);
        return (
          <span className="flex min-w-0 flex-col gap-0.5">
            <span>
              <Badge tone={kind === "fallback" ? "neutral" : "accent"} title={entry?.body}>
                {entry?.badge ?? "fallback"}
              </Badge>
            </span>
            {kind === "smart" ? (
              <span
                className="truncate font-mono text-caption text-faint"
                title={`router model: ${r.router_model ?? "missing"}`}
              >
                {r.router_model ?? "router: missing"}
              </span>
            ) : null}
          </span>
        );
      },
    },
    {
      key: "models",
      title: "candidates",
      className: "hidden sm:table-cell",
      render: (r) => (
        <span className="flex flex-wrap items-center gap-1.5">
          {r.models.map((m, i) => (
            <span key={`${m}-${i}`} className="flex items-center gap-1.5">
              {i > 0 ? <span className="text-faint">→</span> : null}
              <span
                className={cx(
                  "rounded border border-line-strong bg-surface-2 px-1.5 py-0.5 font-mono text-caption text-muted",
                  i === 0 && strategyOf(r) !== "fallback" && "border-accent-text/40 text-fg",
                )}
                title={i === 0 && strategyOf(r) === "round_robin" ? "the starting model rotates" : undefined}
              >
                {m}
              </span>
            </span>
          ))}
        </span>
      ),
    },
    {
      key: "actions",
      title: "",
      align: "right",
      width: "5rem",
      render: (r) => (
        <span className="flex justify-end gap-0.5">
          <IconButton
            label={`Edit combo ${r.name}`}
            onClick={() => {
              setName(r.name);
              setChain(r.models.length > 0 ? r.models : [""]);
              setStrategy(strategyOf(r));
              setRouterModel(r.router_model ?? "");
              setEditing(true);
            }}
          >
            <PencilSimple size={15} aria-hidden />
          </IconButton>
          <IconButton
            label={`Delete combo ${r.name}`}
            onClick={() => setPending(r)}
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
      <PageHeader
        title="Combos"
        subtitle="A named candidate chain. The strategy decides which model goes first; the rest stay the fallback order."
      />

      {combos.isError ? (
        <div className="mb-4">
          <ErrorNote error={combos.error} onRetry={() => void combos.refetch()} />
        </div>
      ) : null}

      <Card className="mb-3">
        <form
          onSubmit={(event) => {
            event.preventDefault();
            if (problem === null) save.mutate();
          }}
        >
          <div className="flex min-w-0 flex-col gap-4 px-5 py-4">
            <div className="w-full sm:w-[280px]">
              <Field label="combo name" hint="the name clients send">
                <Input value={name} onChange={setName} placeholder="smart" ariaLabel="combo name" />
              </Field>
            </div>

            <fieldset className="min-w-0">
              <legend className="mb-2 text-caption font-medium uppercase tracking-[0.08em] text-faint">
                strategy
              </legend>
              <div className="grid gap-2 sm:grid-cols-3">
                {STRATEGIES.map((entry) => {
                  const active = strategy === entry.id;
                  return (
                    <label
                      key={entry.id}
                      className={cx(
                        "flex min-w-0 cursor-pointer items-start gap-2 rounded-[8px] border px-3 py-2.5 transition-colors",
                        active
                          ? "border-[var(--color-accent-text)] bg-[var(--color-selected)]"
                          : "border-line bg-surface hover:border-line-strong",
                      )}
                    >
                      <input
                        type="radio"
                        name="combo-strategy"
                        value={entry.id}
                        checked={active}
                        onChange={() => setStrategy(entry.id)}
                        className="mt-0.5 h-3.5 w-3.5 shrink-0 accent-[var(--color-accent-text)]"
                      />
                      <span className="min-w-0">
                        <span className="block text-body font-medium text-fg">{entry.label}</span>
                        <span className="mt-0.5 block text-caption leading-relaxed text-faint">
                          {entry.body}
                        </span>
                      </span>
                    </label>
                  );
                })}
              </div>
            </fieldset>

            {strategy === "smart" ? (
              <div className="flex min-w-0 flex-col gap-1.5 sm:max-w-[420px]">
                <Field label="router model" hint="the model that picks the first candidate">
                  <Select
                    ariaLabel="router model"
                    value={routerModel}
                    onChange={setRouterModel}
                  >
                    <option value="" disabled>
                      choose a model…
                    </option>
                    {routers.map((model) => (
                      <option key={model.ref} value={model.ref}>
                        {model.display ?? model.id} · {model.provider}
                      </option>
                    ))}
                  </Select>
                </Field>
                <p className="text-caption leading-relaxed text-[var(--color-warn)]">
                  Costs one extra model call per request (latency plus tokens). If the router
                  fails or returns nothing usable, the first candidate in the chain is used.
                </p>
              </div>
            ) : null}

            <Field label="candidates" hint={`tried ${chosen.id === "fallback" ? "top to bottom" : "after the chosen starting model"}`}>
              <ModelChain models={chain} onChange={setChain} />
            </Field>
          </div>

          {problem && (name.trim() !== "" || chain.some(Boolean)) ? (
            <div className="px-5 pb-3">
              <p role="alert" className="text-small text-neg">
                {problem}
              </p>
            </div>
          ) : null}
          {save.isError ? (
            <div className="px-5 pb-3">
              <ErrorNote error={save.error} />
            </div>
          ) : null}
          <CardFooter>
            {editing ? (
              <Button variant="quiet" onClick={reset}>
                cancel
              </Button>
            ) : null}
            <Button
              type="submit"
              variant="primary"
              disabled={problem !== null || save.isPending}
            >
              {editing ? "update combo" : "save combo"}
            </Button>
          </CardFooter>
        </form>
      </Card>

      <ConfirmDialog
        open={pending !== null}
        title="Delete combo"
        message={
          <>
            The {pendingNoun} <span className="font-mono">{pending?.name}</span> stops existing.
            Requests naming it fail until they use another model.
          </>
        }
        confirmLabel="Delete combo"
        busy={remove.isPending}
        error={remove.error}
        onConfirm={() => {
          if (pending) {
            remove.mutate(pending.name, { onSuccess: () => setPending(null) });
          }
        }}
        onClose={() => {
          if (!remove.isPending) {
            remove.reset();
            setPending(null);
          }
        }}
      />

      <Card>
        <CardHeader title="Saved combos" />
        <DataTable
          columns={columns}
          rows={combos.data}
          rowKey={(r) => r.name}
          loading={combos.isLoading}
          tableLayout="fixed"
          empty="No combos yet. A combo is a name your clients can send in place of a model."
        />
      </Card>
    </>
  );
}
