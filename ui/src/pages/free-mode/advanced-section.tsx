import { useEffect, useMemo, useRef, useState } from "react";
import { DataTable, type Column } from "../../components/table";
import {
  Button,
  Checkbox,
  ConfirmDialog,
  Disclosure,
  Dot,
  FilterChip,
  Textarea,
  cx,
} from "../../components/ui";
import { when } from "../../lib/format";
import {
  FREE_DOCUMENT_EXAMPLE,
  documentText,
  validateFreeDocument,
} from "../../lib/free-mode-document";
import {
  freeModeProblems,
  providersOf,
  type FreeAccount,
  type FreeDiscoverResult,
  type FreeImportResult,
  type FreeModeState,
} from "../../lib/free-mode";
import {
  useDiscoverFreeMode,
  useImportFreeMode,
  useTeardownFreeMode,
} from "../../lib/queries";

function count(value: number | null): string {
  return value === null ? "—" : String(value);
}

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
          <li
            key={`${index}-${problem}`}
            className="break-words font-mono text-caption leading-relaxed"
          >
            {problem}
          </li>
        ))}
      </ul>
    </div>
  );
}

const ACCOUNT_COLUMNS: Array<Column<FreeAccount>> = [
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
    width: "7rem",
    render: (row) => (
      <span className="flex items-center gap-1.5 text-small">
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

/**
 * Advanced power-user disclosure: JSON document import for free-tier accounts with
 * per-account egress proxies, imported accounts list, and Clear import.
 */
export function AdvancedSection({
  stored,
  onRefetch,
}: {
  stored: FreeModeState | undefined;
  onRefetch: () => Promise<FreeModeState | undefined>;
}) {
  const importDoc = useImportFreeMode();
  const discover = useDiscoverFreeMode();
  const teardown = useTeardownFreeMode();

  const accounts = useMemo(() => stored?.accounts ?? [], [stored]);
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
  const [clearOpen, setClearOpen] = useState(false);
  const [disableOnClear, setDisableOnClear] = useState(true);

  const seeded = useRef<string | null>(null);
  const fileInput = useRef<HTMLInputElement | null>(null);

  const check = useMemo(() => validateFreeDocument(draft), [draft]);
  const validationProblems = useMemo(
    () => check.problems.map((problem) => `${problem.path}: ${problem.message}`),
    [check.problems],
  );
  const actionProblems = useMemo(() => {
    const error = importDoc.error ?? discover.error;
    return error ? freeModeProblems(error) : [];
  }, [importDoc.error, discover.error]);

  useEffect(() => {
    if (touched) return;
    const next = stored?.document ? documentText(stored.document) : "";
    if (seeded.current === next) return;
    seeded.current = next;
    setDraft(next);
  }, [stored?.document, touched]);

  const edit = (text: string) => {
    setDraft(text);
    setTouched(true);
    setFileError(null);
    setFileName(null);
    setCheckNote(null);
    setImported(null);
  };

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
    const next = await onRefetch();
    const held = next?.accounts.length ?? accounts.length;
    const providerCount = check.providers === 1 ? "1 provider" : `${check.providers} providers`;
    const accountCount = check.accounts === 1 ? "1 account" : `${check.accounts} accounts`;
    const heldCount = held === 1 ? "1 account" : `${held} accounts`;
    setCheckNote(
      `Valid JSON: ${providerCount}, ${accountCount}. The gateway currently holds ${heldCount} from imported documents.`,
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
        setPicked([]);
      },
    });
  };

  const runDiscover = () => {
    setDiscovered(null);
    discover.reset();
    discover.mutate(picked, { onSuccess: (result) => setDiscovered(result) });
  };

  const runClearImport = () => {
    teardown.mutate(disableOnClear, {
      onSuccess: () => {
        setClearOpen(false);
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
  const canImport = check.document !== null && check.problems.length === 0 && !importDoc.isPending;
  const hasImport = stored !== undefined && (stored.document !== null || accounts.length > 0);

  return (
    <>
      <Disclosure
        title="Advanced: import free-tier accounts and proxies (JSON)"
        contentClassName="flex flex-col gap-5 px-5 py-4"
      >
        <div className="flex flex-col gap-3">
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="secondary" size="sm" onClick={() => fileInput.current?.click()}>
              Choose .json
            </Button>
            <Button
              variant="quiet"
              size="sm"
              onClick={() => {
                edit(FREE_DOCUMENT_EXAMPLE);
                setShowProblems(true);
              }}
            >
              Load example
            </Button>
            <Button variant="quiet" size="sm" onClick={() => void runValidate()}>
              Validate
            </Button>
            {draftDirty ? (
              <span className="text-caption text-[var(--color-warn-ink)]">unsaved</span>
            ) : null}
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
              <span className="ml-auto text-caption text-faint">
                paste JSON or drop a .json file
              </span>
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
              rows={12}
              mono
              ariaLabel="free-mode document"
              placeholder={'{\n  "version": 1,\n  "providers": []\n}'}
              disabled={importDoc.isPending}
              className="min-h-[200px] text-small leading-relaxed"
            />
          </div>

          {dragging ? (
            <p className="text-caption text-[var(--color-accent-text)]">drop the .json here</p>
          ) : null}

          {fileError ? <p className="text-small text-[var(--color-neg-ink)]">{fileError}</p> : null}

          {showProblems ? (
            <ProblemList
              title={
                validationProblems.length === 1
                  ? "1 problem to fix"
                  : `${validationProblems.length} problems to fix`
              }
              problems={validationProblems}
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

          <div className="flex flex-wrap items-center justify-between gap-2 pt-1">
            <p className="text-caption text-faint">
              Optional. Use this only to manage pools of free-tier accounts with per-account egress
              proxies. Secrets stay encrypted on the server.
            </p>
            <div className="flex items-center gap-2">
              <Button
                variant="quiet"
                size="sm"
                onClick={revert}
                disabled={!draftDirty || importDoc.isPending}
              >
                Revert
              </Button>
              <Button
                variant="primary"
                size="sm"
                loading={importDoc.isPending}
                disabled={!canImport}
                onClick={runImport}
              >
                {importDoc.isPending ? "Importing…" : "Import"}
              </Button>
            </div>
          </div>
        </div>

        {accounts.length > 0 ? (
          <div className="flex flex-col gap-3 border-t border-rule pt-4">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className="text-small font-medium text-fg tnum">
                Imported accounts ({accounts.length})
              </p>
              <Button
                variant="secondary"
                size="sm"
                loading={discover.isPending}
                disabled={discover.isPending}
                onClick={runDiscover}
              >
                {discover.isPending ? "Discovering…" : "Discover from accounts"}
              </Button>
            </div>

            {providerOptions.length > 1 ? (
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="text-caption text-faint">Limit to</span>
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
                className="rounded-[var(--radius-surface)] border border-line bg-surface-2 px-3 py-2 text-small"
              >
                <p className="text-fg">
                  Discovery finished
                  {stored && stored.discovered_at !== null && stored.discovered_at > 0
                    ? ` · ${when(stored.discovered_at)}`
                    : ""}
                  .
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
                      <li
                        key={`${row.provider}-${row.label ?? ""}-${index}`}
                        className="break-words"
                      >
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
              columns={ACCOUNT_COLUMNS}
              rows={accounts}
              rowKey={(row) => row.connection_id || `${row.provider}-${row.label ?? ""}`}
              scrollLabel="imported free-mode accounts"
              tableLayout="fixed"
              empty="No imported accounts."
            />
          </div>
        ) : null}

        {hasImport ? (
          <div className="flex flex-wrap items-center justify-between gap-3 border-t border-rule pt-4">
            <p className="text-caption text-muted">
              Clear the imported JSON document and account-discovered rows. Public catalog
              discovery stays intact; stored credentials are not deleted.
            </p>
            <Button
              variant="danger"
              size="sm"
              disabled={teardown.isPending}
              onClick={() => {
                teardown.reset();
                setClearOpen(true);
              }}
            >
              Clear import
            </Button>
          </div>
        ) : null}
      </Disclosure>

      <ConfirmDialog
        open={clearOpen}
        title="Clear imported document"
        message={
          <div className="flex flex-col gap-2">
            <p>
              Removes the imported document and account-discovered rows. Auto-discovered public
              catalog models stay available.
            </p>
            <p>
              {accounts.length === 0
                ? "No imported accounts right now."
                : `${accounts.length} imported account${accounts.length === 1 ? "" : "s"} keep their stored credentials; none are deleted.`}
            </p>
            <div className="mt-1">
              <Checkbox
                checked={disableOnClear}
                onChange={setDisableOnClear}
                label="Also switch the imported accounts off"
              />
            </div>
          </div>
        }
        confirmLabel="Clear import"
        busy={teardown.isPending}
        error={teardown.error}
        onConfirm={runClearImport}
        onClose={() => {
          if (!teardown.isPending) {
            teardown.reset();
            setClearOpen(false);
          }
        }}
      />
    </>
  );
}
