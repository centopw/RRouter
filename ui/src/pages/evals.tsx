import { useEffect, useState } from "react";
import { CaretDown } from "@phosphor-icons/react";
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
 * Evals: a suite is context plus a list of prompts and, per case, the criterion a good
 * answer must meet. A run pins the suite to one model (and optionally a judge model) and
 * executes every case through the gateway's own `/v1` path.
 *
 * Context is first-class: the suite's system prompt is prepended to every case, so a
 * context-free model is not asked to invent support policy. With a judge, `expected` is a
 * semantic criterion; without one, the output must contain it literally.
 *
 * A run is asynchronous. Starting one returns immediately and the results list polls the
 * run until it completes, so a slow model never blocks this page.
 */
export default function Evals() {
  const isAdmin = useIsAdmin();
  const suites = useEvalSuites();
  const [selected, setSelected] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [editing, setEditing] = useState<EvalSuite | null>(null);
  const [deleting, setDeleting] = useState<EvalSuite | null>(null);
  const remove = useDeleteEvalSuite();

  const rows = suites.data ?? [];
  const current = rows.find((suite) => suite.id === selected) ?? rows[0] ?? null;

  const columns: Array<Column<EvalSuite>> = [
    {
      key: "name",
      title: "Suite",
      render: (suite) => (
        <span className="flex min-w-0 flex-wrap items-center gap-2">
          <button
            type="button"
            onClick={() => setSelected(suite.id)}
            className={cx(
              "min-w-0 truncate text-left font-medium hover:underline",
              current?.id === suite.id ? "text-accent-text" : "text-fg",
            )}
          >
            {suite.name}
          </button>
          {suite.system_prompt?.trim() ? (
            <Badge title={suite.system_prompt.trim().slice(0, 160)}>context</Badge>
          ) : null}
        </span>
      ),
    },
    {
      key: "description",
      title: "Description",
      className: "hidden md:table-cell",
      render: (suite) => (
        <span className="block max-w-[28rem] truncate text-muted" title={suite.description ?? ""}>
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
      className: "hidden lg:table-cell",
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
                <Button variant="quiet" onClick={() => setEditing(suite)}>
                  edit
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
            Delete <span className="font-medium">{deleting?.name}</span>, its context, cases
            and every run recorded for it? This cannot be undone.
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
      {editing ? <SuiteModal suite={editing} onClose={() => setEditing(null)} /> : null}

      <PageHeader
        title="Evals"
        subtitle={
          isAdmin
            ? "Suites with shared context, a model under test, and an optional judge. Every case goes through the gateway, so routing, limits and usage all apply."
            : "Prompt suites, their context and run history."
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
              ? "No suites yet. Create one, write the shared context, add a few cases, then run against a model."
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

      {current ? <SuiteWorkspace key={current.id} suite={current} isAdmin={isAdmin} /> : null}
    </>
  );
}

function SuiteWorkspace({ suite, isAdmin }: { suite: EvalSuite; isAdmin: boolean }) {
  const detail = useEvalSuite(suite.id);
  const models = useModels();
  const start = useStartEvalRun();
  const createCase = useCreateEvalCase();
  const deleteCase = useDeleteEvalCase();
  const updateSuite = useUpdateEvalSuite();

  // The server document wins once it loads; the local draft is only while editing.
  const savedContext = detail.data?.data.system_prompt ?? suite.system_prompt ?? "";
  const [contextDraft, setContextDraft] = useState<string | null>(null);
  const contextValue = contextDraft ?? savedContext;
  const contextDirty = contextValue.trim() !== savedContext.trim();

  const [model, setModel] = useState("");
  const [judge, setJudge] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [runId, setRunId] = useState<string | null>(null);
  const [input, setInput] = useState("");
  const [expected, setExpected] = useState("");
  const [problem, setProblem] = useState<string | null>(null);
  const [contextProblem, setContextProblem] = useState<string | null>(null);
  const [failure, setFailure] = useState<unknown>(null);

  const run = useEvalRun(runId);
  const cases = detail.data?.cases ?? [];
  const runs = detail.data?.runs ?? [];

  // Connected/available models first; a disabled row stays visible but sorts last.
  const orderedModels = [...(models.data ?? [])].sort((a, b) => {
    const usable = Number(b.enabled && b.available) - Number(a.enabled && a.available);
    return usable !== 0 ? usable : a.ref.localeCompare(b.ref);
  });

  // The newest run is the useful default once the suite has history.
  useEffect(() => {
    if (runId === null && runs.length > 0) setRunId(runs[0]?.id ?? null);
    // Only on first load of this panel: a later refresh must not steal the selection.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [detail.data?.runs.length]);

  useEffect(() => {
    if (model !== "" || orderedModels.length === 0) return;
    const first = orderedModels.find((entry) => entry.enabled && entry.available) ?? orderedModels[0];
    if (first) setModel(first.ref);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [model, models.data]);

  const saveContext = () => {
    setContextProblem(null);
    updateSuite.mutate(
      {
        id: suite.id,
        patch: { system_prompt: contextValue.trim() === "" ? null : contextValue.trim() },
      },
      {
        onSuccess: () => setContextDraft(null),
        onError: () => setContextProblem("Context was not saved."),
      },
    );
  };

  const startRun = () => {
    if (model === "") {
      setProblem("Choose a model to run against.");
      return;
    }
    setProblem(null);
    setFailure(null);
    start.mutate(
      {
        suiteId: suite.id,
        model,
        judgeModel: judge === "" ? undefined : judge,
        apiKey: apiKey.trim() || undefined,
      },
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
        <span className="block max-w-[26rem] truncate font-mono text-small text-fg" title={row.input}>
          {row.input}
        </span>
      ),
    },
    {
      key: "expected",
      title: "Expected criterion",
      render: (row) =>
        row.expected ? (
          <span
            className="block max-w-[20rem] truncate text-small text-muted"
            title={row.expected}
          >
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
    {
      key: "model",
      title: "Model",
      render: (row) => <span className="block max-w-[18rem] truncate font-mono text-small">{row.model}</span>,
    },
    {
      key: "judge",
      title: "Judge",
      width: "12rem",
      render: (row) =>
        row.judge_model ? (
          <span className="block max-w-[11rem] truncate font-mono text-small" title={row.judge_model}>
            {row.judge_model}
          </span>
        ) : (
          <span className="text-faint">literal expected</span>
        ),
    },
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
  // Results carry the case input/output/verdict; the criterion lives on the case row.
  const expectedByCase = new Map(cases.map((entry) => [entry.id, entry.expected]));

  return (
    <div className="flex min-w-0 flex-col gap-6">
      {failure ? <ErrorNote error={failure} /> : null}

      <div className="grid min-w-0 gap-6 xl:grid-cols-[minmax(0,26rem)_minmax(0,1fr)]">
        <div className="flex min-w-0 flex-col gap-6">
          <Card>
            <CardHeader
              title={suite.name}
              action={
                contextValue.trim() ? (
                  <Badge tone="accent">context set</Badge>
                ) : (
                  <Badge>no context</Badge>
                )
              }
            />
            <div className="flex flex-col gap-3 px-5 pb-5">
              {suite.description ? (
                <p className="text-small text-muted">{suite.description}</p>
              ) : null}
              <Field
                label="System prompt / context"
                hint="Prepended to every case as a system message. Empty means no system message."
              >
                <textarea
                  value={contextValue}
                  readOnly={!isAdmin}
                  onChange={(event) => setContextDraft(event.target.value)}
                  rows={7}
                  aria-label="suite system prompt"
                  placeholder="You are the Acme support assistant. Use the policy below. If the answer is not covered, say so and offer a human handoff."
                  className={textareaClass}
                />
              </Field>
              {isAdmin ? (
                <div className="flex flex-wrap items-center gap-2">
                  <Button
                    variant="primary"
                    onClick={saveContext}
                    disabled={!contextDirty || updateSuite.isPending}
                  >
                    {updateSuite.isPending ? "saving…" : "Save context"}
                  </Button>
                  {contextDraft !== null && contextDirty ? (
                    <Button variant="quiet" onClick={() => setContextDraft(null)}>
                      discard
                    </Button>
                  ) : null}
                  {!contextDirty && contextDraft === null ? (
                    <span className="text-caption text-faint">saved</span>
                  ) : null}
                </div>
              ) : null}
              {contextProblem ? (
                <p role="alert" className="text-caption text-neg">
                  {contextProblem}
                </p>
              ) : null}
              {updateSuite.isError ? <ErrorNote error={updateSuite.error} /> : null}
            </div>
          </Card>

          <Card>
            <CardHeader title="Run settings" />
            <div className="flex flex-col gap-4 px-5 pb-5">
              <Field label="model under test" hint="a connected model this gateway can route">
                <Select value={model} onChange={setModel} ariaLabel="eval model">
                  {orderedModels.length === 0 ? <option value="">no models available</option> : null}
                  {orderedModels.map((entry) => (
                    <option key={entry.ref} value={entry.ref}>
                      {entry.ref}
                      {entry.enabled && entry.available ? "" : " (unavailable)"}
                    </option>
                  ))}
                </Select>
              </Field>
              <Field
                label="judge model (optional)"
                hint="With a judge, each expected criterion is evaluated semantically. Without one, the answer must contain it literally."
              >
                <Select value={judge} onChange={setJudge} ariaLabel="eval judge model">
                  <option value="">no judge — literal expected</option>
                  {orderedModels.map((entry) => (
                    <option key={entry.ref} value={entry.ref}>
                      {entry.ref}
                      {entry.enabled && entry.available ? "" : " (unavailable)"}
                    </option>
                  ))}
                </Select>
              </Field>
              {isAdmin ? (
                <Field
                  label="gateway key (optional)"
                  hint="Only for a gateway that requires API keys; used for this run, never stored."
                >
                  <Input
                    value={apiKey}
                    onChange={setApiKey}
                    type="password"
                    placeholder="sk-rr-…"
                    ariaLabel="eval gateway key"
                  />
                </Field>
              ) : null}
              <p className="text-caption text-muted">
                Expected is a criterion, not a magic string. A judge scores whether the answer
                meets it; without a judge the output must contain it literally.
              </p>
              {isAdmin ? (
                <Button
                  variant="primary"
                  onClick={startRun}
                  disabled={start.isPending || cases.length === 0 || model === ""}
                >
                  {start.isPending ? "starting…" : "Run suite"}
                </Button>
              ) : null}
              {problem ? (
                <p role="alert" className="text-caption text-neg">
                  {problem}
                </p>
              ) : null}
              {start.isError ? <ErrorNote error={start.error} /> : null}
            </div>
          </Card>
        </div>

        <div className="flex min-w-0 flex-col gap-6">
          <Card>
            <CardHeader
              title="Cases"
              action={
                <span className="text-caption text-faint tnum">
                  {cases.length} {cases.length === 1 ? "case" : "cases"}
                </span>
              }
            />
            {cases.length === 0 ? (
              <p className="px-5 pb-5 text-small text-muted">
                This suite has no cases yet. Add the prompts a good answer should handle.
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
              <div className="flex flex-col gap-3 border-t border-line px-5 py-4">
                <div className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] sm:items-end">
                  <Field label="input" hint="the prompt sent after the suite context">
                    <Input
                      value={input}
                      onChange={setInput}
                      placeholder="What is the capital of France?"
                      ariaLabel="case input"
                    />
                  </Field>
                  <Field
                    label="expected (optional)"
                    hint="a criterion, or literal text when no judge is used"
                  >
                    <Input
                      value={expected}
                      onChange={setExpected}
                      placeholder="Paris"
                      ariaLabel="case expected criterion"
                    />
                  </Field>
                  <Button variant="ghost" onClick={addCase} disabled={createCase.isPending}>
                    {createCase.isPending ? "adding…" : "add case"}
                  </Button>
                </div>
                {createCase.isError ? <ErrorNote error={createCase.error} /> : null}
              </div>
            ) : null}
          </Card>

          <Card>
            <CardHeader
              title="Results"
              action={
                activeRun ? (
                  <span className="flex items-center gap-2 text-caption text-faint tnum">
                    {activeRun.judge_model ? (
                      <Badge title={activeRun.judge_model}>judge</Badge>
                    ) : (
                      <Badge>literal</Badge>
                    )}
                    {activeRun.status === "pending" || activeRun.status === "running"
                      ? `${activeRun.progress.done}/${activeRun.progress.total} cases`
                      : `${results.length} results`}
                  </span>
                ) : null
              }
            />
            {runId === null ? (
              <EmptyState>Run the suite to see per-case verdicts, output and timing.</EmptyState>
            ) : run.isError ? (
              <div className="px-5 pb-5">
                <ErrorNote error={run.error} onRetry={() => void run.refetch()} />
              </div>
            ) : (
              <>
                {activeRun && (activeRun.status === "pending" || activeRun.status === "running") ? (
                  <div className="px-5 pb-4">
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
                  <p className="px-5 pb-4 text-small text-neg">{activeRun.summary.error}</p>
                ) : null}
                {results.length === 0 ? (
                  <EmptyState>
                    {activeRun && activeRun.status !== "completed"
                      ? "Results appear as cases finish."
                      : "This run has no results."}
                  </EmptyState>
                ) : (
                  <ul className="divide-y divide-line border-t border-line">
                    {results.map((result) => (
                      <ResultRow
                        key={result.id}
                        result={result}
                        expected={expectedByCase.get(result.case_id) ?? null}
                      />
                    ))}
                  </ul>
                )}
              </>
            )}
          </Card>
        </div>
      </div>

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
    </div>
  );
}

/** One result as an expandable row: verdict, timing, expected criterion, output, error. */
function ResultRow({ result, expected }: { result: EvalResult; expected: string | null }) {
  const verdict = result.error
    ? { tone: "neg" as const, label: "error" }
    : result.passed === null
      ? { tone: "neutral" as const, label: "recorded" }
      : result.passed
        ? { tone: "pos" as const, label: "pass" }
        : { tone: "warn" as const, label: "fail" };
  return (
    <li>
      <details className="group">
        <summary className="flex cursor-pointer list-none items-start gap-3 px-5 py-3 hover:bg-surface-2 focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent-text)]/30 [&::-webkit-details-marker]:hidden">
          <span className="mt-0.5 shrink-0">
            <Badge tone={verdict.tone}>{verdict.label}</Badge>
          </span>
          <span className="min-w-0 flex-1">
            <span className="block truncate font-mono text-small text-fg" title={result.case_input}>
              {result.case_input}
            </span>
            <span className="mt-0.5 block text-caption text-faint">
              {result.case_name ? `${result.case_name} · ` : ""}
              {result.latency_ms === null ? "no latency" : `${result.latency_ms} ms`}
            </span>
          </span>
          <CaretDown
            size={14}
            aria-hidden
            className="mt-1 shrink-0 text-faint transition-transform group-open:rotate-180"
          />
        </summary>
        <div className="flex flex-col gap-3 px-5 pb-4">
          <div>
            <p className="text-caption font-medium text-muted">Expected criterion</p>
            <p className="mt-1 whitespace-pre-wrap break-words text-small text-fg">
              {expected ?? "unscored"}
            </p>
          </div>
          <div>
            <p className="text-caption font-medium text-muted">Model output</p>
            {result.output ? (
              <pre className="mt-1 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md border border-line bg-surface-2 px-3 py-2 text-small text-fg">
                {result.output}
              </pre>
            ) : (
              <p className="mt-1 text-small text-faint">no output recorded</p>
            )}
          </div>
          {result.error ? (
            <div>
              <p className="text-caption font-medium text-neg">Error</p>
              <p className="mt-1 whitespace-pre-wrap break-words text-small text-neg">
                {result.error}
              </p>
            </div>
          ) : null}
        </div>
      </details>
    </li>
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

/** Create or edit one suite: name, description and the shared context. */
function SuiteModal({ suite, onClose }: { suite?: EvalSuite; onClose: () => void }) {
  const create = useCreateEvalSuite();
  const update = useUpdateEvalSuite();
  const [name, setName] = useState(suite?.name ?? "");
  const [description, setDescription] = useState(suite?.description ?? "");
  const [systemPrompt, setSystemPrompt] = useState(suite?.system_prompt ?? "");
  const [problem, setProblem] = useState<string | null>(null);
  const busy = create.isPending || update.isPending;

  const submit = () => {
    const trimmed = name.trim();
    if (trimmed === "") {
      setProblem("A suite name is required.");
      return;
    }
    setProblem(null);
    const prompt = systemPrompt.trim() === "" ? null : systemPrompt.trim();
    if (suite) {
      update.mutate(
        {
          id: suite.id,
          patch: {
            name: trimmed,
            description: description.trim(),
            system_prompt: prompt,
          },
        },
        { onSuccess: onClose },
      );
    } else {
      create.mutate(
        {
          name: trimmed,
          ...(description.trim() ? { description: description.trim() } : {}),
          ...(prompt ? { system_prompt: prompt } : {}),
        },
        { onSuccess: onClose },
      );
    }
  };

  return (
    <Modal
      title={suite ? "Edit suite" : "New eval suite"}
      open
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={busy}>
            {busy ? "saving…" : suite ? "save" : "create suite"}
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
        <Field
          label="system prompt / context"
          hint="Prepended to every case. Empty means no system message."
        >
          <textarea
            value={systemPrompt}
            onChange={(event) => setSystemPrompt(event.target.value)}
            rows={6}
            aria-label="suite system prompt"
            placeholder="You are the Acme support assistant. Use the policy below…"
            className={textareaClass}
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

const textareaClass =
  "w-full resize-y rounded-[var(--radius-input)] border border-line-strong bg-surface px-2.5 py-2 text-body text-fg placeholder:text-faint hover:border-[var(--color-fg)]/40 focus:border-[var(--color-accent-text)] focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent-text)]/25 read-only:bg-surface-2 read-only:text-muted";
