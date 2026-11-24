import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import * as Popover from "@radix-ui/react-popover";
import {
  ArrowUp,
  ClockCounterClockwise,
  MagicWand,
  SlidersHorizontal,
  Square,
} from "@phosphor-icons/react";
import { EffortPicker } from "../../components/effort-picker";
import { ModelSelect } from "../../components/model-select";
import {
  Badge,
  Button,
  CopyChip,
  ErrorNote,
  Field,
  Input,
  Modal,
  Segmented,
  Select,
  Textarea,
} from "../../components/ui";
import { playgroundHeaders, rewritePrompt, type Template, type TemplateVersion } from "../../lib/api";
import { ago, when } from "../../lib/format";
import { useCreateTemplate, useTemplateVersions, useTemplates } from "../../lib/queries";
import type { Turn } from "./turn-card";

export const DIALECTS = [
  { id: "chat", label: "Chat" },
  { id: "messages", label: "Messages" },
] as const;

export type Dialect = (typeof DIALECTS)[number]["id"];

export const ENDPOINTS: Record<Dialect, string> = {
  chat: "/v1/chat/completions",
  messages: "/v1/messages",
};

export const MAX_OUTPUT_TOKENS = 2048;

export function parseTemperature(value: string, max = 2): number {
  const parsed = Number.parseFloat(value);
  if (!Number.isFinite(parsed)) return Math.min(1, max);
  return Math.min(max, Math.max(0, parsed));
}

export function describeTemperature(
  selected: { provider: string } | undefined,
  dialect: Dialect,
  reasoningOn: boolean,
): { max: number; supported: boolean; hint?: string } {
  const claude = selected?.provider === "claude" || selected?.provider === "anthropic";
  const max = claude || dialect === "messages" ? 1 : 2;
  const supported =
    selected !== undefined && selected.provider !== "codex" && !(claude && reasoningOn);
  const hint = !selected
    ? "Choose a model first."
    : selected.provider === "codex"
      ? "Codex rejects sampling controls; the gateway omits temperature."
      : claude && reasoningOn
        ? "Claude omits temperature while thinking is on."
        : dialect === "messages"
          ? "Messages accepts 0–1."
          : undefined;
  return { max, supported, hint };
}

export function buildWireRequest({
  dialect,
  model,
  text,
  turns,
  systemText,
  temperature,
  effort,
  authKey,
}: {
  dialect: Dialect;
  model: string;
  text: string;
  turns: Turn[];
  systemText: string;
  temperature: number | undefined;
  effort: string;
  authKey: string;
}) {
  const history = turns
    .filter((turn) => turn.role === "user" || turn.status === "done")
    .filter((turn) => turn.text.trim() !== "")
    .map((turn) => ({ role: turn.role, content: turn.text }));
  const messages = [...history, { role: "user" as const, content: text }];

  const wire: Record<string, unknown> = { model, stream: true };
  if (temperature !== undefined) wire.temperature = temperature;
  if (effort !== "") {
    if (dialect === "chat") wire.reasoning_effort = effort;
    else if (effort === "none") wire.thinking = { type: "disabled" };
    else wire.output_config = { effort };
  }
  if (dialect === "chat") {
    wire.messages = systemText ? [{ role: "system", content: systemText }, ...messages] : messages;
  } else {
    wire.max_tokens = MAX_OUTPUT_TOKENS;
    wire.messages = messages;
    if (systemText) wire.system = systemText;
  }

  const headers: Record<string, string> = {
    "content-type": "application/json",
    ...playgroundHeaders(ENDPOINTS[dialect]),
  };
  if (dialect === "messages") headers["anthropic-version"] = "2023-06-01";
  if (authKey) {
    if (dialect === "messages") headers["x-api-key"] = authKey;
    else headers.authorization = `Bearer ${authKey}`;
  }
  return { wire, headers };
}

const INPUT_CLASS =
  "h-8 w-full min-w-0 rounded-[var(--radius-input)] border border-[var(--color-line-strong)] bg-surface px-2.5 text-body text-fg placeholder:text-faint focus:border-[var(--color-focus-ring)] focus:outline-none";

const KBD_CLASS =
  "rounded border border-line bg-surface-2 px-1 py-0.5 font-sans text-caption text-muted";

export type ComposerProps = {
  draft: string;
  onDraftChange: (value: string) => void;
  model: string;
  onModelChange: (value: string) => void;
  modelLabel: string;
  modelHint?: string;
  effort: string;
  onEffortChange: (value: string) => void;
  levels: string[];
  mandatory: boolean;
  dialect: Dialect;
  onDialectChange: (value: Dialect) => void;
  temp: string;
  onTempChange: (value: string) => void;
  temperatureMax: number;
  temperatureSupported: boolean;
  temperatureHint?: string;
  systemPrompt: string;
  onSystemPromptChange: (value: string) => void;
  activeTemplateId: string | null;
  onActiveTemplateIdChange: (id: string | null) => void;
  apiKey: string;
  keyError: boolean;
  needsKey: boolean;
  streaming: boolean;
  settingsOpen: boolean;
  onSettingsOpenChange: (open: boolean) => void;
  onSend: () => void;
  onAbortStream: () => void;
};

export function Composer({
  draft,
  onDraftChange,
  model,
  onModelChange,
  modelLabel,
  modelHint,
  effort,
  onEffortChange,
  levels,
  mandatory,
  dialect,
  onDialectChange,
  temp,
  onTempChange,
  temperatureMax,
  temperatureSupported,
  temperatureHint,
  systemPrompt,
  onSystemPromptChange,
  activeTemplateId,
  onActiveTemplateIdChange,
  apiKey,
  keyError,
  needsKey,
  streaming,
  settingsOpen,
  onSettingsOpenChange,
  onSend,
  onAbortStream,
}: ComposerProps) {
  const templates = useTemplates();
  const createTemplate = useCreateTemplate();

  const [enhancing, setEnhancing] = useState(false);
  const [enhanceError, setEnhanceError] = useState<string | null>(null);
  const [undoDraft, setUndoDraft] = useState<string | null>(null);
  const [templatePick, setTemplatePick] = useState("");
  const [saveTemplateOpen, setSaveTemplateOpen] = useState(false);
  const [saveTemplateName, setSaveTemplateName] = useState("");
  const [historyOpen, setHistoryOpen] = useState(false);
  const [historyTemplateId, setHistoryTemplateId] = useState<string | null>(null);

  const enhanceAbortRef = useRef<AbortController | null>(null);
  const keyMissing = needsKey && apiKey.trim() === "";
  const hasCustomParams =
    systemPrompt.trim() !== "" || dialect !== "chat" || parseTemperature(temp, temperatureMax) !== 1;

  useEffect(() => {
    if (!enhancing) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") enhanceAbortRef.current?.abort();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [enhancing]);

  useEffect(
    () => () => {
      enhanceAbortRef.current?.abort();
    },
    [],
  );

  const canSend = draft.trim() !== "" && model !== "" && !streaming && !enhancing && !keyMissing;
  const sendBlocker = keyMissing
    ? "The playground key is not ready yet"
    : model === ""
      ? "Choose a model first"
      : draft.trim() === ""
        ? "Type a message first"
        : null;
  const enhanceBlocker =
    apiKey === ""
      ? "The playground key is not ready yet"
      : model === ""
        ? "Choose a model first"
        : draft.trim() === ""
          ? "Type a draft first"
          : null;
  const canEnhance = enhanceBlocker === null && !streaming && !enhancing;

  const onComposerKeyDown = (event: ReactKeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      if (canSend) onSend();
    } else if (event.key === "Escape") {
      if (streaming) {
        event.preventDefault();
        onAbortStream();
      } else if (enhancing) {
        event.preventDefault();
        enhanceAbortRef.current?.abort();
      }
    }
  };

  /**
   * Kilo-style enhance: the same selected model rewrites the draft on the playground key,
   * streamed into the composer so the operator watches it happen. The replaced text is
   * kept for one undo; an abort keeps whatever streamed.
   */
  const enhance = () => {
    const text = draft.trim();
    if (!text || model === "" || apiKey === "" || streaming || enhancing) return;
    const controller = new AbortController();
    enhanceAbortRef.current = controller;
    setUndoDraft(draft);
    setEnhanceError(null);
    setEnhancing(true);
    void (async () => {
      try {
        const rewrite = await rewritePrompt({
          draft: text,
          model,
          key: apiKey,
          signal: controller.signal,
          onText: (soFar) => onDraftChange(soFar),
        });
        if (rewrite.trim() === "") setEnhanceError("The model returned an empty rewrite.");
      } catch (error) {
        if (!controller.signal.aborted) {
          setEnhanceError(error instanceof Error ? error.message : String(error));
        }
      } finally {
        if (enhanceAbortRef.current === controller) enhanceAbortRef.current = null;
        setEnhancing(false);
      }
    })();
  };

  const undoEnhance = () => {
    if (undoDraft === null) return;
    onDraftChange(undoDraft);
    setUndoDraft(null);
  };

  const openHistory = () => {
    const fallbackId = activeTemplateId ?? templates.data?.[0]?.id ?? null;
    if (!fallbackId) return;
    setHistoryTemplateId(fallbackId);
    onSettingsOpenChange(false);
    setHistoryOpen(true);
  };

  const templateList = templates.data ?? [];

  return (
    <>
      {enhanceError ? (
        <p role="alert" className="px-2 pb-1 text-caption text-neg">
          {enhanceError}
        </p>
      ) : null}

      <div className="z-10 px-1 pb-1 pt-1">
        <div className="rounded-[16px] border border-[var(--color-line-strong)] bg-surface focus-within:border-[var(--color-focus-ring)]">
          <textarea
            value={draft}
            onChange={(event) => {
              onDraftChange(event.target.value);
              setUndoDraft(null);
            }}
            onKeyDown={onComposerKeyDown}
            rows={2}
            aria-label="message"
            placeholder={
              model ? `Message ${modelLabel}…` : "Choose a model in Parameters to start…"
            }
            style={{ outline: "none" }}
            className="block max-h-[220px] w-full resize-none rounded-t-[16px] bg-transparent px-4 pt-3.5 text-lead leading-relaxed text-fg placeholder:text-faint focus:outline-none focus-visible:outline-none"
          />
          <div className="flex flex-wrap items-center gap-2 px-3 pb-2.5 pt-1">
            <div className="min-w-[160px] max-w-[240px] flex-1">
              <ModelSelect
                value={model}
                onChange={onModelChange}
                ariaLabel="model"
                plain
                footer={
                  <EffortPicker
                    levels={levels}
                    value={effort}
                    onChange={onEffortChange}
                    mandatory={mandatory}
                  />
                }
              />
            </div>

            <Popover.Root open={settingsOpen} onOpenChange={onSettingsOpenChange}>
              <Popover.Trigger asChild>
                <Button size="sm" variant="quiet" ariaLabel="Parameters">
                  <SlidersHorizontal size={13} aria-hidden />
                  Parameters
                  {hasCustomParams ? (
                    <span aria-hidden className="h-1.5 w-1.5 rounded-full bg-[var(--color-accent-text)]" />
                  ) : null}
                </Button>
              </Popover.Trigger>
              <Popover.Portal>
                <Popover.Content
                  side="top"
                  align="start"
                  sideOffset={8}
                  collisionPadding={12}
                  onPointerDownOutside={(event) => {
                    if ((event.target as HTMLElement | null)?.closest("[role='listbox'], .popover-panel")) {
                      event.preventDefault();
                    }
                  }}
                  onInteractOutside={(event) => {
                    if ((event.target as HTMLElement | null)?.closest("[role='listbox'], .popover-panel")) {
                      event.preventDefault();
                    }
                  }}
                  className="popover-panel z-[var(--z-popover)] max-h-[min(76dvh,560px)] w-[min(500px,calc(100vw-24px))] overflow-y-auto rounded-[var(--radius-overlay)] border border-[var(--color-line-strong)] bg-surface p-4 shadow-[var(--shadow-popover)] focus:outline-none"
                >
                  <div className="mb-3 flex items-center justify-between gap-2 border-b border-line pb-2.5">
                    <div>
                      <h2 className="text-body font-semibold text-fg">Parameters</h2>
                      <p className="text-caption text-faint">
                        Request shape, credentials and instructions for this conversation.
                      </p>
                    </div>
                    <Button size="sm" variant="quiet" onClick={() => onSettingsOpenChange(false)}>
                      Done
                    </Button>
                  </div>

                  <div className="flex flex-col gap-4">
                    <div className="flex flex-wrap items-end gap-3">
                      <div className="min-w-[210px] flex-1">
                        <Field label="Model" hint={modelHint}>
                          <ModelSelect
                            value={model}
                            onChange={onModelChange}
                            ariaLabel="model in parameters"
                            footer={
                              <EffortPicker
                                levels={levels}
                                value={effort}
                                onChange={onEffortChange}
                                mandatory={mandatory}
                              />
                            }
                          />
                        </Field>
                      </div>
                      <div className="w-[136px]">
                        <Field label="Temperature" hint={temperatureHint}>
                          <input
                            type="number"
                            min={0}
                            max={temperatureMax}
                            step={0.1}
                            value={temp}
                            aria-label="temperature"
                            disabled={!temperatureSupported}
                            onChange={(event) => onTempChange(event.target.value)}
                            onBlur={() =>
                              onTempChange(String(parseTemperature(temp, temperatureMax)))
                            }
                            className={`${INPUT_CLASS} tnum disabled:cursor-not-allowed disabled:opacity-50`}
                          />
                        </Field>
                      </div>
                    </div>

                    <Field
                      label="Endpoint"
                      hint="The body is built for this dialect; the gateway translates the reply back."
                    >
                      <div className="flex flex-wrap items-center gap-3">
                        <Segmented
                          options={DIALECTS}
                          value={dialect}
                          onChange={onDialectChange}
                          ariaLabel="endpoint dialect"
                        />
                        <code className="truncate font-mono text-caption text-muted">
                          {ENDPOINTS[dialect]}
                        </code>
                      </div>
                    </Field>

                    <Field
                      label="Key"
                      hint="The gateway issues this playground its own key; its limits and permissions are edited on the API Keys page."
                    >
                      {apiKey ? (
                        <CopyChip value={apiKey} label="playground key" className="w-full" />
                      ) : (
                        <span className="text-small text-faint">
                          {keyError ? "the key could not be issued" : "minting the key…"}
                        </span>
                      )}
                    </Field>

                    <div className="border-t border-line pt-3">
                      <Field
                        label="System prompt"
                        hint="Optional instructions sent before the conversation."
                      >
                        <Textarea
                          id="playground-system-prompt"
                          value={systemPrompt}
                          onChange={(value) => {
                            onSystemPromptChange(value);
                          }}
                          rows={4}
                          ariaLabel="system prompt"
                          placeholder="You are a concise assistant. Answer in the language of the question."
                        />
                      </Field>

                      <div className="mt-2.5 flex flex-wrap items-center gap-2">
                        <Select
                          value={templatePick}
                          onChange={(id) => {
                            setTemplatePick("");
                            const row = templateList.find((template) => template.id === id);
                            if (!row) return;
                            onSystemPromptChange(row.body);
                            onActiveTemplateIdChange(row.id);
                          }}
                          ariaLabel="load a saved template into the system prompt"
                          disabled={templates.isLoading || templateList.length === 0}
                          className="min-w-[170px] flex-1"
                        >
                          <option value="">
                            {templates.isError
                              ? "templates unavailable"
                              : templates.isLoading
                                ? "loading templates…"
                                : templateList.length > 0
                                  ? "insert template…"
                                  : "no templates"}
                          </option>
                          {templateList.map((template) => (
                            <option key={template.id} value={template.id}>
                              {template.name}
                            </option>
                          ))}
                        </Select>
                        <Button
                          size="sm"
                          onClick={() => {
                            createTemplate.reset();
                            onSettingsOpenChange(false);
                            setSaveTemplateOpen(true);
                          }}
                          disabled={systemPrompt.trim() === ""}
                          title={
                            systemPrompt.trim() === ""
                              ? "Write a system prompt first"
                              : "Save this prompt as a template"
                          }
                        >
                          Save as template
                        </Button>
                        <Button
                          size="sm"
                          variant="quiet"
                          onClick={openHistory}
                          disabled={templateList.length === 0}
                          title={
                            templateList.length === 0
                              ? "No saved templates yet"
                              : "Inspect template revision history"
                          }
                        >
                          <ClockCounterClockwise size={13} aria-hidden />
                          History
                        </Button>
                      </div>
                    </div>
                  </div>
                </Popover.Content>
              </Popover.Portal>
            </Popover.Root>

            <span className="ml-auto hidden items-center gap-1 text-caption text-faint min-[820px]:flex">
              <kbd className={KBD_CLASS}>⌘↵</kbd> send · <kbd className={KBD_CLASS}>Esc</kbd> stop
            </span>

            {enhancing ? (
              <button
                type="button"
                onClick={() => enhanceAbortRef.current?.abort()}
                aria-label="stop enhancing"
                title="Stop the rewrite"
                className="flex h-8 items-center gap-1.5 rounded-full border border-[var(--color-line-strong)] bg-surface px-2.5 text-small text-fg hover:bg-surface-3"
              >
                <Square size={10} weight="fill" aria-hidden />
                stop
              </button>
            ) : (
              <button
                type="button"
                onClick={enhance}
                disabled={!canEnhance}
                aria-label="enhance prompt"
                title={enhanceBlocker ?? "Rewrite the draft with this model"}
                className="flex h-8 items-center gap-1.5 rounded-full border border-line bg-surface-2 px-2.5 text-small text-muted transition-colors hover:border-[var(--color-line-strong)] hover:text-fg disabled:cursor-not-allowed disabled:opacity-40"
              >
                <MagicWand size={14} aria-hidden />
                enhance
              </button>
            )}

            {undoDraft !== null && !enhancing ? (
              <button
                type="button"
                onClick={undoEnhance}
                aria-label="undo the rewrite"
                className="text-small text-muted underline underline-offset-2 hover:text-fg"
              >
                undo
              </button>
            ) : null}

            {streaming ? (
              <button
                type="button"
                onClick={onAbortStream}
                aria-label="stop generating"
                title="Stop"
                className="flex h-9 w-9 items-center justify-center rounded-full border border-[var(--color-line-strong)] bg-surface text-fg hover:bg-surface-3"
              >
                <Square size={13} weight="fill" aria-hidden />
              </button>
            ) : (
              <button
                type="button"
                onClick={onSend}
                disabled={!canSend}
                title={canSend ? "Send (⌘↵)" : (sendBlocker ?? undefined)}
                aria-label="send message"
                className="flex h-9 w-9 items-center justify-center rounded-full bg-accent text-[var(--color-accent-ink)] transition-colors hover:bg-fg disabled:cursor-not-allowed disabled:opacity-40"
              >
                <ArrowUp size={16} weight="bold" aria-hidden />
              </button>
            )}
          </div>
        </div>
      </div>

      {saveTemplateOpen ? (
        <Modal
          title="Save as template"
          variant="dialog"
          width="sm"
          open
          onClose={() => setSaveTemplateOpen(false)}
          footer={
            <>
              <Button onClick={() => setSaveTemplateOpen(false)}>Cancel</Button>
              <Button
                variant="primary"
                disabled={saveTemplateName.trim() === "" || createTemplate.isPending}
                onClick={() =>
                  createTemplate.mutate(
                    { name: saveTemplateName.trim(), body: systemPrompt },
                    {
                      onSuccess: (created) => {
                        onActiveTemplateIdChange(created.id);
                        setSaveTemplateOpen(false);
                        setSaveTemplateName("");
                      },
                    },
                  )
                }
              >
                {createTemplate.isPending ? "Saving…" : "Save template"}
              </Button>
            </>
          }
        >
          <div className="space-y-3">
            <Field label="name" hint="unique; shown in the Templates page and the picker">
              <Input
                value={saveTemplateName}
                onChange={setSaveTemplateName}
                placeholder="support-reply"
                ariaLabel="template name"
              />
            </Field>
            <p className="max-h-24 overflow-hidden whitespace-pre-wrap break-words font-mono text-caption leading-relaxed text-faint">
              {systemPrompt}
            </p>
            {createTemplate.isError ? <ErrorNote error={createTemplate.error} /> : null}
          </div>
        </Modal>
      ) : null}

      {historyOpen ? (
        <RevisionHistoryDrawer
          templates={templateList}
          selectedId={historyTemplateId}
          onSelectTemplate={setHistoryTemplateId}
          onClose={() => setHistoryOpen(false)}
          onRestore={(template, version) => {
            onSystemPromptChange(version.body);
            onActiveTemplateIdChange(template.id);
            setHistoryOpen(false);
            onSettingsOpenChange(true);
          }}
        />
      ) : null}
    </>
  );
}

function RevisionHistoryDrawer({
  templates,
  selectedId,
  onSelectTemplate,
  onClose,
  onRestore,
}: {
  templates: Template[];
  selectedId: string | null;
  onSelectTemplate: (id: string) => void;
  onClose: () => void;
  onRestore: (template: Template, version: TemplateVersion) => void;
}) {
  const active = templates.find((item) => item.id === selectedId) ?? templates[0] ?? null;
  const versions = useTemplateVersions(active?.id ?? null);

  return (
    <Modal
      open
      variant="drawer"
      width="md"
      title={active ? `History — ${active.name}` : "Revision history"}
      description="Newest first. Restoring copies that revision into the Playground system prompt."
      onClose={onClose}
      footer={<Button onClick={onClose}>Close</Button>}
    >
      <div className="space-y-4">
        {templates.length > 1 && active ? (
          <Field label="Template">
            <Select
              value={active.id}
              onChange={onSelectTemplate}
              ariaLabel="template for revision history"
            >
              {templates.map((template) => (
                <option key={template.id} value={template.id}>
                  {template.name}
                </option>
              ))}
            </Select>
          </Field>
        ) : null}

        {versions.isError ? (
          <ErrorNote error={versions.error} onRetry={() => void versions.refetch()} />
        ) : null}

        {!versions.data ? (
          <p className="py-6 text-center text-body text-faint">
            {versions.isLoading ? "Loading revisions…" : "—"}
          </p>
        ) : versions.data.length === 0 ? (
          <p className="py-6 text-center text-body text-faint">No revisions recorded.</p>
        ) : (
          <ol className="space-y-2">
            {versions.data.map((version) => (
              <li
                key={version.version}
                className="rounded-[var(--radius-surface)] border border-line bg-surface-2 px-3 py-2.5"
              >
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <span className="flex items-center gap-2">
                    <Badge tone={active && version.version === active.version ? "pos" : "neutral"}>
                      <span className="font-mono tnum">v{version.version}</span>
                    </Badge>
                    <span className="text-caption text-faint tnum" title={when(version.created_at)}>
                      {when(version.created_at)} · {ago(version.created_at)}
                    </span>
                    {active && version.version === active.version ? (
                      <span className="text-caption text-faint">current</span>
                    ) : null}
                  </span>
                  {active ? (
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => onRestore(active, version)}
                    >
                      Restore
                    </Button>
                  ) : null}
                </div>
                <p className="mt-1.5 max-h-28 overflow-hidden whitespace-pre-wrap break-words font-mono text-caption leading-relaxed text-muted">
                  {version.body}
                </p>
              </li>
            ))}
          </ol>
        )}
      </div>
    </Modal>
  );
}
