import { useEffect, useMemo, useRef, useState } from "react";
import {
  AlertBanner,
  Badge,
  Button,
  Card,
  CardFooter,
  CardHeader,
  Checkbox,
  ConfirmDialog,
  CopyIconButton,
  Dot,
  ErrorNote,
  Field,
  FilterChip,
  Input,
  PageHeader,
  Select,
  Textarea,
  Toggle,
  cx,
} from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { ago, when } from "../lib/format";
import {
  FREE_DOCUMENT_EXAMPLE,
  FREE_PREFERS,
  attemptsProblem,
  documentText,
  freeRefHints,
  priceLabel,
  routingOf,
  rpmProblem,
  sourceLabel,
  validateFreeDocument,
  withRouting,
  type FreePrefer,
  type FreeRouting,
} from "../lib/free-mode-document";
import {
  freeModeProblems,
  providersOf,
  refSample,
  type FreeAccount,
  type FreeDiscoverResult,
  type FreeImportResult,
  type FreeModel,
} from "../lib/free-mode";
import {
  useDiscoverFreeMode,
  useFreeMode,
  useImportFreeMode,
  useTeardownFreeMode,
} from "../lib/queries";

/** `when()` for a tooltip, `ago()` on the face: a timestamp nobody has to convert by hand. */
function stamp(ts: number | null): string {
  return ts === null || ts === 0 ? "" : when(ts);
}

function relative(ts: number | null): string {
  return ts === null || ts === 0 ? "never" : ago(ts);
}

/** An API count on the page; `—` when the answer did not carry it. */
function count(value: number | null): string {
  return value === null ? "—" : String(value);
}

/** Every problem as its own row, in the gateway's `path: message` spelling. */
function ProblemList({ title, problems }: { title: string; problems: string[] }) {
  if (problems.length === 0) return null;
  return (
    <div
      role="alert"
      className="rounded-[var(--radius-surface)] border border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-3 py-2 text-body text-[var(--color-neg-ink)]"
    >
      <p className="text-small font-medium">{title}</p>
      <ul className="mt-1 list-disc space-y-0.5 pl-5">
        {problems.map((problem, index) => (
          <li key={`${index}-${problem}`} className="break-words font-mono text-caption leading-relaxed">
            {problem}
          </li>
        ))}
      </ul>
    </div>
  );
}

/**
 * Free mode: one document declares free-tier accounts and the egress proxy each one must
 * use. Import creates or updates those accounts; Discover records which of their models are
 * free; the reserved ref `free` then spreads requests across the resulting pairs.
 *
 * The page is a draft editor over a server-owned document: nothing here is saved until
 * Import answers, and every count on the page comes from `GET /api/free-mode`.
 */
export default function FreeMode() {
  const free = useFreeMode();
  const importDoc = useImportFreeMode();
  const discover = useDiscoverFreeMode();
  const teardown = useTeardownFreeMode();

  const stored = free.data;
  const accounts = useMemo(() => stored?.accounts ?? [], [stored]);
  const models = useMemo(() => stored?.models ?? [], [stored]);
  const freeModels = useMemo(() => models.filter((model) => model.free), [models]);
  const providerOptions = useMemo(() => providersOf(accounts), [accounts]);

  const [draft, setDraft] = useState("");
  const [touched, setTouched] = useState(false);
  const [showProblems, setShowProblems] = useState(false);
  const [checkNote, setCheckNote] = useState<string | null>(null);
  const [fileError, setFileError] = useState<string | null>(null);
  const [fileName, setFileName] = useState<string | null>(null);
  const [dragging, setDragging] = useState(false);
  const [imported, setImported] = useState<FreeImportResult | null>(null);
  const [discovered, setDiscovered] = useState<FreeDiscoverResult | null>(null);
  const [picked, setPicked] = useState<string[]>([]);
  const [teardownOpen, setTeardownOpen] = useState(false);
  const [disableOnTeardown, setDisableOnTeardown] = useState(true);
  const [rpmText, setRpmText] = useState("");
  const [attemptsText, setAttemptsText] = useState("");

  /** The text last written into the editor from the server, so a refetch can re-seed it. */
  const seeded = useRef<string | null>(null);
  const fileInput = useRef<HTMLInputElement | null>(null);

  const check = useMemo(() => validateFreeDocument(draft), [draft]);
  const knobs = useMemo(() => routingOf(check.document, stored?.routing ?? null), [check.document, stored]);
  const storedKnobs = useMemo(() => routingOf(null, stored?.routing ?? null), [stored]);
  const providerProblems = useMemo(
    () => check.problems.map((problem) => `${problem.path}: ${problem.message}`),
    [check.problems],
  );
  const actionProblems = useMemo(() => {
    const error = importDoc.error ?? discover.error;
    return error ? freeModeProblems(error) : [];
  }, [importDoc.error, discover.error]);

  // The server owns the document: re-seed the editor whenever it answers something new,
  // unless the operator has typed since the last seed.
  useEffect(() => {
    if (touched) return;
    const next = stored?.document ? documentText(stored.document) : "";
    if (seeded.current === next) return;
    seeded.current = next;
    setDraft(next);
  }, [stored?.document, touched]);

  // The knobs mirror the draft's `routing` block. Re-seed only when that block changes, so
  // a value the operator is still typing is never overwritten by its own commit.
  const knobKey = JSON.stringify(knobs);
  useEffect(() => {
    setRpmText(knobs.per_account_rpm === null ? "" : String(knobs.per_account_rpm));
    setAttemptsText(String(knobs.max_attempts));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [knobKey]);

  const edit = (text: string) => {
    setDraft(text);
    setTouched(true);
    setFileError(null);
    setFileName(null);
    setCheckNote(null);
    setImported(null);
  };

  const applyRouting = (patch: Partial<FreeRouting>) => {
    if (!check.document) return;
    edit(documentText(withRouting(check.document, { ...knobs, ...patch })));
  };

  /** Picking is document state: the id list is `routing.models`, applied by Import. */
  const toggleModel = (id: string) => {
    const next = knobs.models.includes(id)
      ? knobs.models.filter((entry) => entry !== id)
      : [...knobs.models, id];
    applyRouting({ models: next });
  };

  const sample = useMemo(() => refSample(models, knobs.models), [models, knobs.models]);
  const refHints = useMemo(() => freeRefHints(sample), [sample]);

  const revert = () => {
    seeded.current = null;
    setTouched(false);
    setFileName(null);
    setFileError(null);
    setShowProblems(false);
    setCheckNote(null);
    setImported(null);
  };

  const runValidate = async () => {
    setShowProblems(true);
    setCheckNote(null);
    if (!check.document || check.problems.length > 0) return;
    const result = await free.refetch();
    if (result.data === undefined) return;
    const held = result.data.accounts.length;
    const providerCount = check.providers === 1 ? "1 provider" : `${check.providers} providers`;
    const accountCount = check.accounts === 1 ? "1 account" : `${check.accounts} accounts`;
    const heldCount = held === 1 ? "1 account" : `${held} accounts`;
    setCheckNote(
      `Parses as JSON: ${providerCount}, ${accountCount}. The gateway holds ${heldCount} from the last import.`,
    );
  };

  const runImport = () => {
    setImported(null);
    setDiscovered(null);
    setCheckNote(null);
    importDoc.reset();
    if (!check.document || check.problems.length > 0) {
      setShowProblems(true);
      return;
    }
    importDoc.mutate(check.document, {
      onSuccess: (result) => {
        setImported(result);
        setShowProblems(false);
        setTouched(false);
        // A provider the new document no longer declares must not stay in the run filter.
        setPicked([]);
      },
    });
  };

  const runDiscover = () => {
    setDiscovered(null);
    discover.reset();
    discover.mutate(picked, { onSuccess: (result) => setDiscovered(result) });
  };

  const runTeardown = () => {
    teardown.mutate(disableOnTeardown, {
      onSuccess: () => {
        setTeardownOpen(false);
        setImported(null);
        setDiscovered(null);
        seeded.current = null;
        setTouched(false);
      },
    });
  };

  const readFile = async (file: File | undefined) => {
    if (!file) return;
    setFileError(null);
    if (!/\.json$/i.test(file.name) && file.type !== "application/json") {
      setFileError(`${file.name} is not a .json file.`);
      return;
    }
    try {
      const text = await file.text();
      edit(text);
      setFileName(file.name);
      setShowProblems(true);
    } catch (error) {
      setFileError(
        `Could not read ${file.name}: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  };

  const togglePicked = (provider: string) =>
    setPicked((current) =>
      current.includes(provider)
        ? current.filter((entry) => entry !== provider)
        : [...current, provider],
    );

  const draftDirty = touched && draft !== (seeded.current ?? "");
  const nothingImported =
    stored !== undefined &&
    stored.document === null &&
    accounts.length === 0 &&
    models.length === 0;
  const canImport = check.document !== null && check.problems.length === 0 && !importDoc.isPending;

  const accountColumns: Array<Column<FreeAccount>> = [
    {
      key: "provider",
      title: "Provider",
      width: "9rem",
      render: (row) => (
        <span className="truncate text-small text-fg" title={row.provider}>
          {row.provider}
        </span>
      ),
    },
    {
      key: "label",
      title: "Label",
      width: "11rem",
      render: (row) =>
        row.label ? (
          <span className="truncate font-mono text-small text-fg">{row.label}</span>
        ) : (
          <span className="text-small text-faint">unnamed</span>
        ),
    },
    {
      key: "proxy",
      title: "Proxy",
      render: (row) =>
        row.proxy ? (
          <span
            className="truncate font-mono text-caption text-muted"
            title={`${row.proxy} — masked by the gateway`}
          >
            {row.proxy}
          </span>
        ) : (
          <span className="text-small text-faint">direct</span>
        ),
      cellTitle: (row) => row.proxy ?? "direct",
    },
    {
      key: "rpm",
      title: "RPM",
      align: "right",
      width: "5.5rem",
      render: (row) =>
        row.free_rpm === null ? (
          <span className="text-caption text-faint">no cap</span>
        ) : (
          <span className="tnum text-small">{row.free_rpm}</span>
        ),
      cellTitle: (row) => (row.free_rpm === null ? "no cap" : String(row.free_rpm)),
    },
    {
      key: "active",
      title: "State",
      width: "7.5rem",
      render: (row) => (
        <span className="flex items-center gap-1.5 text-small">
          {/* Active is the resting state, not an achievement: the muted dot, like the drawer. */}
          <Dot tone="muted" />
          <span className={row.is_active ? "text-muted" : "text-faint"}>
            {row.is_active ? "active" : "disabled"}
          </span>
        </span>
      ),
      cellTitle: (row) => (row.is_active ? "active" : "disabled"),
    },
    {
      key: "last_error",
      title: "Last error",
      render: (row) =>
        row.last_error ? (
          <span className="truncate text-caption text-[var(--color-neg-ink)]">{row.last_error}</span>
        ) : (
          <span className="text-small text-faint">—</span>
        ),
      cellTitle: (row) => row.last_error ?? "—",
    },
  ];

  const modelRows = useMemo(
    () =>
      [...models].sort(
        (a, b) =>
          Number(b.free) - Number(a.free) ||
          a.provider.localeCompare(b.provider) ||
          a.model.localeCompare(b.model),
      ),
    [models],
  );

  const modelColumns: Array<Column<FreeModel>> = [
    {
      key: "picked",
      title: "Use",
      width: "4.5rem",
      render: (row) => (
        <span
          title={
            row.free
              ? knobs.models.includes(row.model)
                ? "picked: bare free is narrowed to the picked ids"
                : "add this id to routing.models"
              : "not free: only free models can be picked"
          }
        >
          <Checkbox
            checked={knobs.models.includes(row.model)}
            onChange={() => toggleModel(row.model)}
            ariaLabel={`use ${row.model} for free`}
            disabled={!check.document || !row.free}
          />
        </span>
      ),
      cellTitle: (row) => (knobs.models.includes(row.model) ? "picked" : "not picked"),
    },
    {
      key: "provider",
      title: "Provider",
      width: "9rem",
      render: (row) => (
        <span className="truncate text-small text-fg" title={row.provider}>
          {row.provider}
        </span>
      ),
    },
    {
      key: "model",
      title: "Model",
      render: (row) => (
        <span className="truncate font-mono text-small text-fg" title={`${row.provider}/${row.model}`}>
          {row.model}
        </span>
      ),
    },
    {
      key: "free",
      title: "Free",
      width: "7rem",
      render: (row) =>
        row.free ? (
          <Badge tone="pos">free</Badge>
        ) : (
          <Badge title="listed by the account, but neither the id nor the registry prices it at zero">
            not free
          </Badge>
        ),
      cellTitle: (row) => (row.free ? "free" : "not free"),
    },
    {
      key: "source",
      title: "Source",
      width: "8.5rem",
      render: (row) => <Badge title={`source: ${row.source}`}>{sourceLabel(row.source)}</Badge>,
      cellTitle: (row) => row.source,
    },
    {
      key: "price",
      title: "Price /M",
      align: "right",
      width: "6.5rem",
      render: (row) => (
        <span className={cx("tnum text-small", row.price === null ? "text-faint" : "text-fg")}>
          {priceLabel(row.price)}
        </span>
      ),
      cellTitle: (row) => priceLabel(row.price),
    },
    {
      key: "discovered_at",
      title: "Discovered",
      width: "8rem",
      render: (row) => (
        <span className="text-caption text-muted" title={stamp(row.discovered_at)}>
          {relative(row.discovered_at)}
        </span>
      ),
      cellTitle: (row) => relative(row.discovered_at),
    },
  ];

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        title="Free mode"
        subtitle="Import free-tier accounts and the egress proxy each one uses. The gateway discovers which of their models are free and spreads the reserved ref free across those account and proxy pairs."
      />

      {free.isError ? (
        <ErrorNote error={free.error} onRetry={() => void free.refetch()} />
      ) : null}

      {nothingImported ? (
        <AlertBanner
          tone="info"
          title="Nothing imported yet"
          className="mb-0"
          actions={
            <Button variant="secondary" onClick={() => fileInput.current?.click()}>
              Choose .json
            </Button>
          }
        >
          <p>
            Paste a document below or drop a .json file. Import validates the whole document
            before it writes anything; Discover then records which models are actually free.
          </p>
        </AlertBanner>
      ) : null}

      {stored ? (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-[var(--radius-card)] border border-line bg-surface-2 px-4 py-2.5 text-small">
          <span className="flex items-center gap-1.5">
            <Dot tone={storedKnobs.enabled ? "pos" : "muted"} />
            <span className={storedKnobs.enabled ? "text-muted" : "text-faint"}>
              {storedKnobs.enabled ? "free routing enabled" : "free routing paused"}
            </span>
          </span>
          <span aria-hidden className="text-faint">
            ·
          </span>
          <span className="text-muted tnum">
            {accounts.length} account{accounts.length === 1 ? "" : "s"}
          </span>
          <span aria-hidden className="text-faint">
            ·
          </span>
          <span className="text-muted tnum">
            {freeModels.length} free of {models.length} discovered models
          </span>
          <span className="ml-auto text-caption text-faint" title={stamp(stored.updated_at)}>
            {stored.updated_at === null || stored.updated_at === 0
              ? "no document stored"
              : `document saved ${ago(stored.updated_at)}`}
          </span>
        </div>
      ) : null}

      <Card>
        <CardHeader
          title="Document"
          action={
            <div className="flex items-center gap-2">
              {draftDirty ? <span className="text-caption text-[var(--color-warn)]">unsaved</span> : null}
              <Button variant="secondary" onClick={() => void runValidate()}>
                Validate
              </Button>
            </div>
          }
        />
        <div className="flex flex-col gap-3 px-5 pb-4">
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="secondary" onClick={() => fileInput.current?.click()}>
              Choose .json
            </Button>
            <Button
              variant="quiet"
              onClick={() => {
                edit(FREE_DOCUMENT_EXAMPLE);
                setShowProblems(true);
              }}
            >
              Load example
            </Button>
            <input
              ref={fileInput}
              type="file"
              accept=".json,application/json"
              className="sr-only"
              aria-label="import a free-mode document"
              onChange={(event) => {
                void readFile(event.target.files?.[0]);
                event.target.value = "";
              }}
            />
            {fileName ? (
              <span className="text-caption text-faint">from {fileName}</span>
            ) : (
              <span className="ml-auto text-caption text-faint">paste JSON or drop a .json file</span>
            )}
          </div>

          <div
            onDragOver={(event) => {
              event.preventDefault();
              setDragging(true);
            }}
            onDragLeave={() => setDragging(false)}
            onDrop={(event) => {
              event.preventDefault();
              setDragging(false);
              void readFile(event.dataTransfer.files?.[0]);
            }}
            className={cx(
              "rounded-[var(--radius-input)] border-2 border-dashed transition-colors",
              dragging ? "border-[var(--color-accent)] p-0" : "border-transparent",
            )}
          >
            <Textarea
              value={draft}
              onChange={edit}
              rows={16}
              mono
              ariaLabel="free-mode document"
              placeholder={'{\n  "version": 1,\n  "providers": []\n}'}
              disabled={importDoc.isPending}
              className="min-h-[260px] text-small leading-relaxed"
            />
          </div>
          {dragging ? (
            <p className="text-caption text-[var(--color-accent-text)]">drop the .json here</p>
          ) : null}

          {fileError ? <p className="text-small text-[var(--color-neg-ink)]">{fileError}</p> : null}

          {showProblems ? (
            <ProblemList
              title={
                providerProblems.length === 1
                  ? "1 problem to fix"
                  : `${providerProblems.length} problems to fix`
              }
              problems={providerProblems}
            />
          ) : null}

          <ProblemList
            title={
              actionProblems.length === 1
                ? "The gateway refused this request"
                : `The gateway named ${actionProblems.length} problems`
            }
            problems={actionProblems}
          />

          {checkNote ? (
            <p role="status" className="text-small text-muted">
              {checkNote}
            </p>
          ) : null}

          {imported ? (
            <p role="status" className="text-small text-[var(--color-pos-ink)]">
              Import: providers {count(imported.providers)} · accounts created{" "}
              {count(imported.accounts.created)} · accounts updated{" "}
              {count(imported.accounts.updated)} · switched off {count(imported.disabled)}.
            </p>
          ) : null}

          <p className="text-caption leading-relaxed text-faint">
            The gateway validates the whole document and names every problem at once; nothing is
            written unless every one of them is fixed. Secrets stay encrypted — an import answer
            never repeats a key.
          </p>
        </div>
        <CardFooter>
          <Button variant="quiet" onClick={revert} disabled={!draftDirty || importDoc.isPending}>
            Revert to stored document
          </Button>
          <Button variant="primary" loading={importDoc.isPending} disabled={!canImport} onClick={runImport}>
            {importDoc.isPending ? "Importing…" : "Import"}
          </Button>
        </CardFooter>
      </Card>

      <Card>
        <CardHeader
          title="Routing"
          action={<span className="text-caption text-faint">part of the document; Import applies it</span>}
        />
        <div className="flex flex-col gap-4 px-5 pb-4">
          <Field
            label="Enabled"
            hint="Off keeps the accounts and the discovered models, but the reserved ref is not routed."
          >
            <div className="flex items-center gap-2">
              <Toggle
                checked={knobs.enabled}
                onChange={(next) => applyRouting({ enabled: next })}
                label="free routing enabled"
                disabled={!check.document}
              />
              <span className="text-small text-muted">{knobs.enabled ? "on" : "off"}</span>
            </div>
          </Field>

          <div className="grid gap-4 sm:grid-cols-2">
            <Field
              label="Requests per account"
              hint="Proactive cap per minute; empty means no local cap. Falls back to this when an account names no rpm."
              error={rpmProblem(rpmText) ?? undefined}
            >
              <Input
                type="number"
                value={rpmText}
                onChange={(value) => {
                  setRpmText(value);
                  if (rpmProblem(value) === null) {
                    applyRouting({
                      per_account_rpm: value.trim() === "" ? null : Number(value.trim()),
                    });
                  }
                }}
                placeholder="no cap"
                ariaLabel="free-mode requests per account per minute"
                className="tnum"
                disabled={!check.document}
              />
            </Field>
            <Field
              label="Max attempts"
              hint="How many free-mode candidates one request may try before it fails."
              error={attemptsProblem(attemptsText) ?? undefined}
            >
              <Input
                type="number"
                value={attemptsText}
                onChange={(value) => {
                  setAttemptsText(value);
                  if (attemptsProblem(value) === null) {
                    applyRouting({ max_attempts: Number(value.trim()) });
                  }
                }}
                ariaLabel="free-mode maximum attempts"
                className="tnum"
                disabled={!check.document}
              />
            </Field>
          </div>

          <Field
            label="Prefer"
            hint={FREE_PREFERS.find((entry) => entry.id === knobs.prefer)?.hint}
          >
            <Select
              value={knobs.prefer}
              onChange={(value) => applyRouting({ prefer: value as FreePrefer })}
              ariaLabel="free-mode routing preference"
              disabled={!check.document}
            >
              {FREE_PREFERS.map((entry) => (
                <option key={entry.id} value={entry.id}>
                  {entry.label}
                </option>
              ))}
            </Select>
          </Field>

          <p className="text-caption leading-relaxed text-muted">
            Stored: {storedKnobs.enabled ? "enabled" : "paused"} ·{" "}
            {storedKnobs.per_account_rpm === null
              ? "no per-account cap"
              : `${storedKnobs.per_account_rpm} rpm per account`}{" "}
            · {storedKnobs.max_attempts} attempts · {storedKnobs.prefer} ·{" "}
            {storedKnobs.models.length === 0
              ? "every free model"
              : `${storedKnobs.models.length} picked model${storedKnobs.models.length === 1 ? "" : "s"}`}
            .
            {check.document === null
              ? " Import a document to edit the knobs."
              : draftDirty
                ? " This draft differs — Import to apply it."
                : ""}
          </p>
        </div>
      </Card>

      <Card>
        <CardHeader
          title="Accounts"
          action={
            <div className="flex items-center gap-2">
              <span className="text-caption text-faint tnum">
                {accounts.length} total
              </span>
              <Button
                variant="secondary"
                loading={discover.isPending}
                disabled={accounts.length === 0 || discover.isPending}
                onClick={runDiscover}
                title={
                  accounts.length === 0
                    ? "Import a document first: discovery visits the imported accounts."
                    : undefined
                }
              >
                {discover.isPending ? "Discovering…" : "Discover models"}
              </Button>
            </div>
          }
        />

        {providerOptions.length > 0 ? (
          <div className="flex flex-wrap items-center gap-1.5 px-5 pb-3">
            <span className="text-caption text-faint">Limit the run to</span>
            {providerOptions.map((provider) => (
              <FilterChip
                key={provider}
                on={picked.includes(provider)}
                onClick={() => togglePicked(provider)}
                ariaLabel={`discover ${provider} only`}
              >
                {provider}
              </FilterChip>
            ))}
            {picked.length > 0 ? (
              <button
                type="button"
                onClick={() => setPicked([])}
                className="text-caption text-muted underline underline-offset-2 hover:text-fg"
              >
                every provider
              </button>
            ) : null}
          </div>
        ) : null}

        {discovered ? (
          <div
            role="status"
            className="mx-5 mb-3 rounded-[8px] border border-line bg-surface-2 px-3 py-2 text-small"
          >
            <p className="text-fg">
              Discovery finished
              {stored && stored.discovered_at !== null && stored.discovered_at > 0
                ? ` · ${when(stored.discovered_at)}`
                : ""}
              {"."}
            </p>
            {discovered.providers.length > 0 ? (
              <ul className="mt-1 space-y-0.5 text-caption text-muted">
                {discovered.providers.map((row) => (
                  <li key={row.provider} className="tnum">
                    <span className="font-mono text-fg">{row.provider}</span>
                    {row.free !== null ? ` · ${row.free} free` : ""}
                    {row.models !== null ? ` of ${row.models} models` : ""}
                    {row.accounts !== null
                      ? ` · ${row.accounts} account${row.accounts === 1 ? "" : "s"}`
                      : ""}
                  </li>
                ))}
              </ul>
            ) : null}
            {discovered.errors.length > 0 ? (
              <ul className="mt-1 space-y-0.5 text-caption text-[var(--color-neg-ink)]">
                {discovered.errors.map((row, index) => (
                  <li key={`${row.provider}-${row.label ?? ""}-${index}`} className="break-words">
                    <span className="font-mono">
                      {row.provider}
                      {row.label ? ` · ${row.label}` : ""}
                    </span>
                    : {row.error}
                  </li>
                ))}
              </ul>
            ) : null}
          </div>
        ) : null}

        <DataTable
          columns={accountColumns}
          rows={free.isLoading ? undefined : accounts}
          rowKey={(row) => row.connection_id || `${row.provider}-${row.label ?? ""}`}
          loading={free.isLoading}
          scrollLabel="free-mode accounts"
          tableLayout="fixed"
          empty="No free-mode accounts. Import a document to create them."
        />
      </Card>

      <Card>
        <CardHeader
          title="Discovered models"
          action={
            <span className="text-caption text-faint tnum">
              {freeModels.length} free of {models.length}
            </span>
          }
        />
        <div className="flex flex-col gap-2 px-5 pb-3">
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <p className="min-w-0 flex-1 text-small text-muted">
              {knobs.models.length === 0
                ? "No models picked: bare free uses every discovered free model."
                : `free is narrowed to ${knobs.models.length} picked model${knobs.models.length === 1 ? "" : "s"}; everything else discovery found is left out.`}
            </p>
            {knobs.models.length > 0 && check.document ? (
              <Button variant="quiet" onClick={() => applyRouting({ models: [] })}>
                Use every model
              </Button>
            ) : null}
          </div>
          {knobs.models.length > 0 ? (
            <ul className="flex flex-wrap gap-1.5" aria-label="picked model ids">
              {knobs.models.map((id) => (
                <li
                  key={id}
                  className="inline-flex max-w-full min-w-0 items-center gap-1 rounded-full border border-line bg-surface-2 py-0.5 pl-2.5 pr-1"
                >
                  <span className="truncate font-mono text-caption text-muted" title={id}>
                    {id}
                  </span>
                  <button
                    type="button"
                    onClick={() => toggleModel(id)}
                    disabled={!check.document}
                    aria-label={`stop using ${id}`}
                    title={`stop using ${id}`}
                    className="inline-flex h-5 w-5 shrink-0 items-center justify-center rounded-full text-faint hover:bg-surface-3 hover:text-fg disabled:opacity-40"
                  >
                    <span aria-hidden>×</span>
                  </button>
                </li>
              ))}
            </ul>
          ) : null}
          {check.document === null ? (
            <p className="text-caption text-faint">
              The editor holds no valid document, so picking is disabled — fix it above first.
            </p>
          ) : null}
        </div>
        <DataTable
          columns={modelColumns}
          rows={free.isLoading ? undefined : modelRows}
          rowKey={(row) => `${row.provider}/${row.model}`}
          loading={free.isLoading}
          scrollLabel="discovered free-mode models"
          tableLayout="fixed"
          empty="Nothing discovered yet. Run Discover once the accounts are imported."
        />
      </Card>

      <Card>
        <CardHeader
          title="Call it like this"
          action={
            <span className="text-caption text-faint">as the model in /v1/chat/completions</span>
          }
        />
        <div className="px-5 pb-4">
          <p className="mb-1 text-small text-muted">
            The reserved ref free routes across every free-mode account and proxy, so a client
            sends one of these where it would otherwise name a model.
          </p>
          {sample === null ? (
            <p className="mb-1 text-caption text-faint">
              Nothing discovered yet: the placeholder lines below show the shape, not a model this
              gateway has.
            </p>
          ) : null}
          <ul className="flex flex-col">
            {refHints.map((entry) => (
              <li
                key={entry.ref}
                className="flex flex-wrap items-center gap-x-2 gap-y-1 border-b border-line py-2 last:border-b-0"
              >
                <span className="min-w-0 break-all font-mono text-small text-fg">{entry.ref}</span>
                <span className="min-w-0 flex-1 text-caption text-faint">{entry.hint}</span>
                <CopyIconButton
                  value={entry.ref}
                  label={`Copy the ${entry.ref} ref`}
                  className="ml-auto"
                />
              </li>
            ))}
          </ul>
        </div>
      </Card>

      <Card>
        <CardHeader title="Tear down" />
        <div className="flex flex-col gap-3 px-5 pb-4">
          <p className="text-small leading-relaxed text-muted">
            Clearing removes the imported document, the routing knobs and every discovered model,
            so requests naming <span className="font-mono text-fg">free</span> fail until a
            document is imported again. The account rows and their encrypted credentials stay —
            nothing is deleted.
          </p>
          <div>
            <Button
              variant="danger"
              disabled={
                teardown.isPending ||
                stored === undefined ||
                (stored.document === null && accounts.length === 0 && models.length === 0)
              }
              onClick={() => {
                teardown.reset();
                setTeardownOpen(true);
              }}
            >
              Tear down free mode
            </Button>
          </div>
        </div>
      </Card>

      <ConfirmDialog
        open={teardownOpen}
        title="Tear down free mode"
        message={
          <div className="flex flex-col gap-2">
            <p>
              The imported document, its routing knobs and every discovered model are cleared.
              Requests naming <span className="font-mono">free</span> fail until something is
              imported again.
            </p>
            <p>
              {accounts.length === 0
                ? "No accounts are imported right now."
                : `${accounts.length} imported account${accounts.length === 1 ? "" : "s"} keep their stored credentials; none are deleted.`}
            </p>
            <div className="mt-1">
              <Checkbox
                checked={disableOnTeardown}
                onChange={setDisableOnTeardown}
                label="Also switch the free-mode accounts off"
              />
            </div>
          </div>
        }
        confirmLabel="Tear down"
        busy={teardown.isPending}
        error={teardown.error}
        onConfirm={runTeardown}
        onClose={() => {
          if (!teardown.isPending) {
            teardown.reset();
            setTeardownOpen(false);
          }
        }}
      />
    </div>
  );
}
