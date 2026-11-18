import { useEffect, useMemo, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { PencilSimple, Plus, Trash } from "@phosphor-icons/react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  ConfirmDialog,
  ErrorNote,
  Field,
  Input,
  Modal,
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
  const [params, setParams] = useSearchParams();
  const formRef = useRef<HTMLFormElement>(null);
  const [drawerOpen, setDrawerOpen] = useState(false);
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
    setDrawerOpen(false);
  };

  const openCreate = () => {
    setName("");
    setChain([""]);
    setStrategy("fallback");
    setRouterModel("");
    setEditing(false);
    save.reset();
    setDrawerOpen(true);
  };

  const openEdit = (combo: Combo) => {
    setName(combo.name);
    setChain(combo.models.length > 0 ? combo.models : [""]);
    setStrategy(strategyOf(combo));
    setRouterModel(combo.router_model ?? "");
    setEditing(true);
    save.reset();
    setDrawerOpen(true);
  };

  const editParam = params.get("edit");
  useEffect(() => {
    if (!editParam || !combos.data) return;
    const match = combos.data.find((item) => item.name === editParam);
    if (match) openEdit(match);
    const next = new URLSearchParams(params);
    next.delete("edit");
    setParams(next, { replace: true });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editParam, combos.data]);

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
  const comboList = combos.data ?? [];

  const columns: Array<Column<Combo>> = [
    {
      key: "name",
      title: "Name",
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
              <span className="text-caption text-faint tnum">
                {r.models.length} candidate{r.models.length === 1 ? "" : "s"}
              </span>
              {kind === "smart" && r.router_model ? (
                <span className="truncate font-mono text-caption text-faint">{r.router_model}</span>
              ) : null}
            </span>
            <span className="flex flex-wrap items-center gap-1 sm:hidden">
              {r.models.map((m, i) => (
                <span key={`${m}-${i}`} className="flex items-center gap-1">
                  {i > 0 ? <span className="text-faint">→</span> : null}
                  <span className="rounded border border-line-strong bg-surface-2 px-1.5 py-0.5 font-mono text-caption text-muted">
                    <span className="mr-1 text-faint tnum">{i + 1}.</span>
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
      title: "Strategy",
      className: "hidden sm:table-cell",
      render: (r) => {
        const kind = strategyOf(r);
        const entry = STRATEGIES.find((candidate) => candidate.id === kind);
        return (
          <span className="flex min-w-0 flex-col gap-0.5">
            <span className="flex items-center gap-1.5">
              <Badge tone={kind === "fallback" ? "neutral" : "accent"} title={entry?.body}>
                {entry?.badge ?? "fallback"}
              </Badge>
              <span className="text-caption text-faint tnum">
                {r.models.length} step{r.models.length === 1 ? "" : "s"}
              </span>
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
      title: "Candidates",
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
                <span className="mr-1 text-faint tnum">{i + 1}.</span>
                {m}
              </span>
            </span>
          ))}
        </span>
      ),
    },
    {
      key: "actions",
      title: "Actions",
      srOnlyTitle: true,
      align: "right",
      width: "14rem",
      render: (r) => (
        <span className="flex items-center justify-end gap-1">
          <Link
            to={`/playground/chat?model=${encodeURIComponent(r.name)}`}
            className="inline-flex h-8 shrink-0 items-center whitespace-nowrap rounded-full border border-line bg-surface px-2.5 text-caption font-medium text-fg transition-colors hover:border-line-strong hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
          >
            Test in Playground →
          </Link>
          <IconButton label={`Edit combo ${r.name}`} onClick={() => openEdit(r)}>
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
        actions={
          <Button variant="primary" onClick={openCreate}>
            <Plus size={15} aria-hidden />
            Create combo
          </Button>
        }
      />

      {combos.isError ? (
        <div className="mb-4">
          <ErrorNote error={combos.error} onRetry={() => void combos.refetch()} />
        </div>
      ) : null}

      <Modal
        open={drawerOpen}
        onClose={() => {
          if (!save.isPending) reset();
        }}
        title={editing ? `Edit combo · ${name}` : "Create combo"}
        description="Name a candidate chain and choose how requests select the starting model."
        width="lg"
        footer={
          <>
            <Button variant="quiet" onClick={reset} disabled={save.isPending}>
              Cancel
            </Button>
            <Button
              variant="primary"
              onClick={() => formRef.current?.requestSubmit()}
              disabled={problem !== null || save.isPending}
            >
              {save.isPending ? "Saving…" : editing ? "Update combo" : "Save combo"}
            </Button>
          </>
        }
      >
        <form
          ref={formRef}
          className="flex min-w-0 flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (problem === null && !save.isPending) save.mutate();
          }}
        >
          <Field label="Combo name" hint="The name clients send">
            <Input
              value={name}
              onChange={setName}
              placeholder="smart"
              ariaLabel="combo name"
              autoFocus
              disabled={save.isPending}
            />
          </Field>

          <fieldset className="min-w-0">
            <legend className="mb-2 text-caption font-medium uppercase tracking-[0.08em] text-faint">
              Strategy
            </legend>
            <div className="grid gap-2">
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
            <div className="flex min-w-0 flex-col gap-1.5">
              <Field label="Router model" hint="The model that picks the first candidate">
                <Select
                  ariaLabel="router model"
                  value={routerModel}
                  onChange={setRouterModel}
                >
                  <option value="" disabled>
                    Choose a model…
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

          <Field
            label="Candidates"
            hint={`Tried ${chosen.id === "fallback" ? "top to bottom" : "after the chosen starting model"}`}
          >
            <ModelChain models={chain} onChange={setChain} />
          </Field>

          {problem && (name.trim() !== "" || chain.some(Boolean)) ? (
            <p role="alert" className="text-small text-neg">
              {problem}
            </p>
          ) : null}
          {save.isError ? <ErrorNote error={save.error} /> : null}
        </form>
      </Modal>

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
        <CardHeader
          title="Saved combos"
          action={
            <span className="text-caption text-faint tnum">
              {comboList.length} total
            </span>
          }
        />
        <DataTable
          columns={columns}
          rows={combos.data}
          rowKey={(r) => r.name}
          loading={combos.isLoading}
          tableLayout="fixed"
          stickyRight="actions"
          empty={
            <div className="flex flex-col items-center gap-2">
              <span>No combos yet. A combo is a name your clients can send in place of a model.</span>
              <Button variant="primary" onClick={openCreate}>
                <Plus size={15} aria-hidden />
                Create combo
              </Button>
            </div>
          }
        />
      </Card>
    </>
  );
}
