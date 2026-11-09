import { useEffect, useState } from "react";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  Field,
  Input,
  Modal,
  PageHeader,
  Select,
  cx,
} from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { when } from "../lib/format";
import {
  useCreateEvalCase,
  useCreateEvalSuite,
  useDeleteEvalCase,
  useDeleteEvalSuite,
  useEvalRun,
  useEvalSuite,
  useEvalSuites,
  useIsAdmin,
  useModels,
  useStartEvalRun,
  useUpdateEvalSuite,
} from "../lib/queries";
import type { EvalCase, EvalResult, EvalRun, EvalSuite } from "../lib/api";

/**
 * Evals: a suite is a list of prompts and the text a good answer must contain; a run pins
 * a suite to one model and executes every case through the gateway's own `/v1` path.
 *
 * A run is asynchronous. Starting one returns immediately and the results table polls the
 * run until it completes, so a slow model never blocks this page.
 */
export default function Evals() {
  const isAdmin = useIsAdmin();
  const suites = useEvalSuites();
  const [selected, setSelected] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [renaming, setRenaming] = useState<EvalSuite | null>(null);
  const [deleting, setDeleting] = useState<EvalSuite | null>(null);
  const remove = useDeleteEvalSuite();

  const rows = suites.data ?? [];
  const current = rows.find((suite) => suite.id === selected) ?? rows[0] ?? null;

  const columns: Array<Column<EvalSuite>> = [
    {
      key: "name",
      title: "Suite",
      render: (suite) => (
        <button
          type="button"
          onClick={() => setSelected(suite.id)}
          className={cx(
            "text-left font-medium hover:underline",
            current?.id === suite.id ? "text-accent-text" : "text-fg",
          )}
        >
          {suite.name}
        </button>
      ),
    },
    {
      key: "description",
      title: "Description",
      render: (suite) => (
        <span className="text-muted">
          {suite.description ?? <span className="text-faint">—</span>}
        </span>
      ),
    },
    {
      key: "cases",
      title: "Cases",
      align: "right",
      width: "6rem",
      render: (suite) => <span className="text-muted tnum">{suite.cases}</span>,
    },
    {
      key: "updated",
      title: "Updated",
      width: "9rem",
      render: (suite) => (
        <span className="whitespace-nowrap text-muted">{when(suite.updated_at)}</span>
      ),
    },
    ...(isAdmin
      ? [
          {
            key: "actions",
            title: "Actions",
            srOnlyTitle: true,
            align: "right" as const,
            render: (suite: EvalSuite) => (
              <span className="flex justify-end gap-1.5">
                <Button variant="quiet" onClick={() => setRenaming(suite)}>
                  rename
                </Button>
                <Button variant="danger" onClick={() => setDeleting(suite)}>
                  delete
                </Button>
              </span>
            ),
          },
        ]
      : []),
  ];

  return (
    <>
      <ConfirmDialog
        open={deleting !== null}
        title="Delete eval suite"
        message={
          <>
            Delete <span className="font-medium">{deleting?.name}</span>, its cases and every
            run recorded for it? This cannot be undone.
          </>
        }
        confirmLabel="Delete suite"
        busy={remove.isPending}
        onConfirm={() => {
          if (deleting) remove.mutate(deleting.id);
          setDeleting(null);
        }}
        onClose={() => setDeleting(null)}
      />

      {creating ? <SuiteModal onClose={() => setCreating(false)} /> : null}
      {renaming ? (
        <SuiteModal suite={renaming} onClose={() => setRenaming(null)} />
      ) : null}

      <PageHeader
        title="Evals"
        subtitle={
          isAdmin
            ? "Prompt suites you run against a model. Every case goes through the gateway, so routing, limits and usage all apply."
            : "Prompt suites and their run history."
        }
        actions={
          isAdmin ? (
            <Button variant="primary" onClick={() => setCreating(true)}>
              New suite
            </Button>
          ) : undefined
        }
      />

      {suites.isError ? (
        <div className="mb-4">
          <ErrorNote error={suites.error} onRetry={() => void suites.refetch()} />
        </div>
      ) : null}

      <Card className="mb-6">
        <CardHeader
          title="Suites"
          action={
            <span className="text-caption text-faint tnum">
              {rows.length} {rows.length === 1 ? "suite" : "suites"}
            </span>
          }
        />
        {rows.length === 0 && !suites.isLoading ? (
          <EmptyState>
            {isAdmin
              ? "No suites yet. Create one, add a few prompts, then run it against a model."
              : "No eval suites yet."}
          </EmptyState>
        ) : (
          <DataTable
            columns={columns}
            rows={rows}
            rowKey={(suite) => suite.id}
            loading={suites.isLoading}
            scrollLabel="Eval suites"
          />
        )}
      </Card>

      {current ? <SuitePanel key={current.id} suite={current} isAdmin={isAdmin} /> : null}
    </>
  );
}

function SuitePanel({ suite, isAdmin }: { suite: EvalSuite; isAdmin: boolean }) {
  const detail = useEvalSuite(suite.id);
  const models = useModels();
  const start = useStartEvalRun();
  const createCase = useCreateEvalCase();
  const deleteCase = useDeleteEvalCase();

  const [model, setModel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [runId, setRunId] = useState<string | null>(null);
  const [input, setInput] = useState("");
  const [expected, setExpected] = useState("");
  const [problem, setProblem] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);

  const run = useEvalRun(runId);
  const cases = detail.data?.cases ?? [];
  const runs = detail.data?.runs ?? [];

  // The newest run is the useful default once the suite has history.
  useEffect(() => {
    if (runId === null && runs.length > 0) setRunId(runs[0]?.id ?? null);
    // Only on first load of this panel: a later refresh must not steal the selection.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [detail.data?.runs.length]);

  useEffect(() => {
    if (model !== "" || !models.data?.length) return;
    const usable = models.data.filter((entry) => entry.enabled && entry.available);
    const first = usable[0] ?? models.data[0];
    if (first) setModel(first.ref);
  }, [model, models.data]);

  const startRun = () => {
    if (model === "") {
      setProblem("Choose a model to run against.");
      return;
    }
    setProblem(null);
    setFailure(null);
    start.mutate(
      { suiteId: suite.id, model, apiKey: apiKey.trim() || undefined },
      {
        onSuccess: (body) => setRunId(body.data.id),
        onError: (error) => setFailure(error),
      },
    );
  };

  const addCase = () => {
    if (input.trim() === "") {
      setProblem("A case needs an input prompt.");
      return;
    }
    setProblem(null);
    setFailure(null);
    createCase.mutate(
      {
        suiteId: suite.id,
        input: input.trim(),
        expected: expected.trim() === "" ? undefined : expected.trim(),
      },
      {
        onSuccess: () => {
          setInput("");
          setExpected("");
        },
        onError: (error) => setFailure(error),
      },
    );
  };

  const caseColumns: Array<Column<EvalCase>> = [
    {
      key: "input",
      title: "Input",
      render: (row) => (
        <span className="block max-w-[420px] truncate font-mono text-small text-fg" title={row.input}>
          {row.input}
        </span>
      ),
    },
    {
      key: "expected",
      title: "Expected",
      render: (row) =>
        row.expected ? (
          <span className="block max-w-[260px] truncate text-small text-muted" title={row.expected}>
            {row.expected}
          </span>
        ) : (
          <span className="text-faint" title="no expectation: the output is recorded, not judged">
            unscored
          </span>
        ),
    },
    ...(isAdmin
      ? [
          {
            key: "actions",
            title: "Actions",
            srOnlyTitle: true,
            align: "right" as const,
            render: (row: EvalCase) => (
              <Button
                variant="quiet"
                disabled={deleteCase.isPending}
                onClick={() => deleteCase.mutate({ suiteId: suite.id, caseId: row.id })}
              >
                remove
              </Button>
            ),
          },
        ]
      : []),
  ];

  const runColumns: Array<Column<EvalRun>> = [
    {
      key: "when",
      title: "Started",
      width: "10rem",
      render: (row) => <span className="whitespace-nowrap text-muted">{when(row.created_at)}</span>,
    },
    { key: "model", title: "Model", render: (row) => <span className="font-mono text-small">{row.model}</span> },
    {
      key: "status",
      title: "Status",
      width: "8rem",
      render: (row) => (
        <span className="flex items-center gap-2">
          <Badge tone={runTone(row.status)}>{row.status}</Badge>
          {row.status === "pending" || row.status === "running" ? (
            <span className="text-caption text-faint tnum">
              {row.progress.done}/{row.progress.total}
            </span>
          ) : null}
        </span>
      ),
    },
    {
      key: "summary",
      title: "Result",
      render: (row) => <RunSummary run={row} />,
    },
    {
      key: "actions",
      title: "",
      align: "right",
      width: "5rem",
      render: (row) => (
        <Button
          variant="quiet"
          onClick={() => setRunId(row.id)}
          ariaLabel={`View results of the run started ${when(row.created_at)}`}
        >
          {runId === row.id ? "viewing" : "view"}
        </Button>
      ),
    },
  ];

  const results = run.data?.results ?? [];
  const activeRun = run.data?.data ?? null;
  const resultColumns: Array<Column<EvalResult>> = [
    {
      key: "case",
      title: "Case",
      render: (row) => (
        <span className="block max-w-[320px] truncate font-mono text-small text-fg" title={row.case_input}>
          {row.case_input}
        </span>
      ),
    },
    {
      key: "verdict",
      title: "Verdict",
      width: "7rem",
      render: (row) =>
        row.error ? (
          <Badge tone="neg">error</Badge>
        ) : row.passed === null ? (
          <Badge tone="neutral">recorded</Badge>
        ) : row.passed ? (
          <Badge tone="pos">pass</Badge>
        ) : (
          <Badge tone="warn">fail</Badge>
        ),
    },
    {
      key: "latency",
      title: "Latency",
      align: "right",
      width: "6rem",
      render: (row) =>
        row.latency_ms === null ? (
          <span className="text-faint">—</span>
        ) : (
          <span className="text-muted tnum">{row.latency_ms} ms</span>
        ),
    },
    {
      key: "detail",
      title: "Detail",
      render: (row) => (
        <span
          className={cx("block max-w-[420px] truncate text-small", row.error ? "text-neg" : "text-muted")}
          title={row.error ?? row.output ?? ""}
        >
          {row.error ?? row.output ?? "—"}
        </span>
      ),
    },
  ];

  return (
    <div className="flex flex-col gap-6">
      {failure ? <ErrorNote error={failure} /> : null}

      <Card>
        <CardHeader
          title={`${suite.name} · run`}
          action={
            <span className="text-caption text-faint tnum">
              {cases.length} {cases.length === 1 ? "case" : "cases"}
            </span>
          }
        />
        <div className="flex flex-wrap items-end gap-3 px-6 pb-5">
          <div className="min-w-[220px] flex-1">
            <Field label="model" hint="a model this gateway can route">
              <Select value={model} onChange={setModel} ariaLabel="eval model">
                {(models.data ?? []).length === 0 ? <option value="">no models available</option> : null}
                {(models.data ?? []).map((entry) => (
                  <option key={entry.ref} value={entry.ref}>
                    {entry.ref}
                    {entry.enabled ? "" : " (disabled)"}
                  </option>
                ))}
              </Select>
            </Field>
          </div>
          {isAdmin ? (
            <>
              <div className="min-w-[200px] flex-1">
                <Field
                  label="gateway key (optional)"
                  hint="needed when this gateway requires API keys; used for this run only, never stored"
                >
                  <Input
                    value={apiKey}
                    onChange={setApiKey}
                    type="password"
                    placeholder="sk-rr-…"
                    ariaLabel="eval gateway key"
                  />
                </Field>
              </div>
              <Button
                variant="primary"
                onClick={startRun}
                disabled={start.isPending || cases.length === 0 || model === ""}
              >
                {start.isPending ? "starting…" : "Run suite"}
              </Button>
            </>
          ) : null}
        </div>
        {problem ? (
          <p role="alert" className="px-6 pb-4 text-caption text-neg">
            {problem}
          </p>
        ) : null}
        {cases.length === 0 ? (
          <p className="px-6 pb-5 text-small text-muted">
            This suite has no cases yet.
          </p>
        ) : null}
        <DataTable
          columns={caseColumns}
          rows={cases}
          rowKey={(row) => row.id}
          loading={detail.isLoading}
          scrollLabel={`Cases in ${suite.name}`}
          empty="No cases yet."
        />
        {isAdmin ? (
          <div className="flex flex-wrap items-end gap-3 border-t border-line px-6 py-4">
            <div className="min-w-[240px] flex-1">
              <Field label="input" hint="the prompt sent to the model">
                <Input
                  value={input}
                  onChange={setInput}
                  placeholder="What is the capital of France?"
                  ariaLabel="case input"
                />
              </Field>
            </div>
            <div className="min-w-[200px] flex-1">
              <Field label="expected (optional)" hint="the answer must contain this text; empty = record only">
                <Input
                  value={expected}
                  onChange={setExpected}
                  placeholder="Paris"
                  ariaLabel="case expected text"
                />
              </Field>
            </div>
            <Button variant="ghost" onClick={addCase} disabled={createCase.isPending}>
              {createCase.isPending ? "adding…" : "add case"}
            </Button>
          </div>
        ) : null}
      </Card>

      <Card>
        <CardHeader
          title="Runs"
          action={
            <span className="text-caption text-faint tnum">
              {runs.length === 0 ? "none yet" : `${runs.length} recent`}
            </span>
          }
        />
        <DataTable
          columns={runColumns}
          rows={runs}
          rowKey={(row) => row.id}
          loading={detail.isLoading}
          scrollLabel={`Runs of ${suite.name}`}
          empty="No runs yet. Pick a model and run the suite."
        />
      </Card>

      {runId ? (
        <Card>
          <CardHeader
            title="Results"
            action={
              activeRun ? (
                <span className="text-caption text-faint tnum">
                  {activeRun.status === "pending" || activeRun.status === "running"
                    ? `${activeRun.progress.done}/${activeRun.progress.total} cases`
                    : `${results.length} results`}
                </span>
              ) : null
            }
          />
          {run.isError ? (
            <div className="px-6 pb-5">
              <ErrorNote error={run.error} onRetry={() => void run.refetch()} />
            </div>
          ) : (
            <>
              {activeRun && (activeRun.status === "pending" || activeRun.status === "running") ? (
                <div className="px-6 pb-4">
                  <div className="h-1.5 w-full overflow-hidden rounded-full bg-surface-3">
                    <div
                      className="h-full rounded-full bg-accent transition-[width] duration-300"
                      style={{
                        width: `${
                          activeRun.progress.total === 0
                            ? 0
                            : Math.round((activeRun.progress.done / activeRun.progress.total) * 100)
                        }%`,
                      }}
                    />
                  </div>
                  <p className="mt-2 text-caption text-faint" aria-live="polite">
                    {activeRun.status === "pending"
                      ? "Queued: another run is using the gateway's eval slots."
                      : `Running ${activeRun.progress.done} of ${activeRun.progress.total} cases…`}
                  </p>
                </div>
              ) : null}
              {activeRun?.summary?.error ? (
                <p className="px-6 pb-4 text-small text-neg">{activeRun.summary.error}</p>
              ) : null}
              <DataTable
                columns={resultColumns}
                rows={results}
                rowKey={(row) => row.id}
                loading={run.isLoading}
                scrollLabel="Case results"
                empty={
                  activeRun && activeRun.status !== "completed"
                    ? "Results appear as cases finish."
                    : "This run has no results."
                }
              />
            </>
          )}
        </Card>
      ) : null}
    </div>
  );
}

function runTone(status: EvalRun["status"]): "pos" | "warn" | "neg" | "neutral" {
  switch (status) {
    case "completed":
      return "pos";
    case "running":
      return "warn";
    case "failed":
      return "neg";
    default:
      return "neutral";
  }
}

function RunSummary({ run }: { run: EvalRun }) {
  const summary = run.summary;
  if (!summary) {
    return <span className="text-faint">{run.status === "failed" ? "failed" : "—"}</span>;
  }
  return (
    <span className="whitespace-nowrap text-small">
      <span className="text-pos tnum">{summary.passed} pass</span>
      <span className="text-faint"> · </span>
      <span className={summary.failed > 0 ? "text-warn tnum" : "text-faint tnum"}>
        {summary.failed} fail
      </span>
      {summary.errors > 0 ? (
        <>
          <span className="text-faint"> · </span>
          <span className="text-neg tnum">{summary.errors} error</span>
        </>
      ) : null}
      <span className="text-faint"> · </span>
      <span className="text-muted tnum">{(summary.duration_ms / 1000).toFixed(1)}s</span>
    </span>
  );
}

/** Create or rename one suite. */
function SuiteModal({ suite, onClose }: { suite?: EvalSuite; onClose: () => void }) {
  const create = useCreateEvalSuite();
  const update = useUpdateEvalSuite();
  const [name, setName] = useState(suite?.name ?? "");
  const [description, setDescription] = useState(suite?.description ?? "");
  const [problem, setProblem] = useState<string | null>(null);
  const busy = create.isPending || update.isPending;

  const submit = () => {
    const trimmed = name.trim();
    if (trimmed === "") {
      setProblem("A suite name is required.");
      return;
    }
    setProblem(null);
    if (suite) {
      update.mutate(
        { id: suite.id, patch: { name: trimmed, description: description.trim() } },
        { onSuccess: onClose },
      );
    } else {
      create.mutate(
        { name: trimmed, ...(description.trim() ? { description: description.trim() } : {}) },
        { onSuccess: onClose },
      );
    }
  };

  return (
    <Modal
      title={suite ? "Rename suite" : "New eval suite"}
      open
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={busy}>
            {busy ? "saving…" : suite ? "rename" : "create suite"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <Field label="name" hint="unique across this gateway">
          <Input value={name} onChange={setName} placeholder="support smoke" ariaLabel="suite name" />
        </Field>
        <Field label="description" hint="optional">
          <Input
            value={description}
            onChange={setDescription}
            placeholder="short prompts the support bot must answer"
            ariaLabel="suite description"
          />
        </Field>
        {problem ? (
          <span role="alert" className="text-caption text-neg">
            {problem}
          </span>
        ) : null}
        {create.isError ? <ErrorNote error={create.error} /> : null}
        {update.isError ? <ErrorNote error={update.error} /> : null}
      </div>
    </Modal>
  );
}
