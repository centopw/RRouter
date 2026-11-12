import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { ArrowUp, MagicWand, SlidersHorizontal, Square } from "@phosphor-icons/react";
import { useSearchParams } from "react-router-dom";
import { ModelSelect } from "../components/model-select";
import {
  Button,
  CopyChip,
  ErrorNote,
  Field,
  Input,
  Modal,
  Segmented,
  Select,
  cx,
  statusText,
} from "../components/ui";
import { ms } from "../lib/format";
import { useCreateTemplate, useModels, usePlaygroundKey, useSystem, useTemplates } from "../lib/queries";
import { PLAYGROUND_HEADERS, rewritePrompt } from "../lib/api";
import { Heading } from "../components/atoms";

/**
 * A live conversation against the gateway, so an operator can prove a model answers
 * before pointing a client at it. One request per send: the same `/v1` endpoints a
 * client uses, the same auth header, the same SSE frames. Nothing is simulated.
 */

const DIALECTS = [
  { id: "chat", label: "Chat" },
  { id: "messages", label: "Messages" },
] as const;

type Dialect = (typeof DIALECTS)[number]["id"];

const ENDPOINTS: Record<Dialect, string> = {
  chat: "/v1/chat/completions",
  messages: "/v1/messages",
};

/** Anthropic requires `max_tokens`; the playground keeps one generous, fixed cap. */
const MAX_OUTPUT_TOKENS = 2048;

type TurnStatus = "streaming" | "done" | "stopped" | "error";

type Turn = {
  id: number;
  role: "user" | "assistant";
  /** The model that produced an assistant turn, frozen so later selects do not rewrite it. */
  model: string | null;
  text: string;
  status: TurnStatus;
  error: string | null;
  /** Verbatim bytes from the response, shown behind the Raw toggle. */
  raw: string;
  ttft: number | null;
  latency: number | null;
  promptTokens: number | null;
  completionTokens: number | null;
};

export default function Playground() {
  const models = useModels();
  const system = useSystem();

  const [dialect, setDialect] = useState<Dialect>("chat");
  const [model, setModel] = useState("");
  const [systemPrompt, setSystemPrompt] = useState("");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [temp, setTemp] = useState("1");
  const [draft, setDraft] = useState("");
  const [turns, setTurns] = useState<Turn[]>([]);
  // The gateway's own key for this page: minted on first use, never pasted by hand.
  const key = usePlaygroundKey();
  const apiKey = key.data?.key ?? "";
  // The enhancer streams a rewrite into the composer; `undoDraft` is what it replaced.
  const [enhancing, setEnhancing] = useState(false);
  const [enhanceError, setEnhanceError] = useState<string | null>(null);
  const [undoDraft, setUndoDraft] = useState<string | null>(null);

  const templates = useTemplates();
  const createTemplate = useCreateTemplate();
  // The picker is an insert menu, not a live selection: it resets to its placeholder.
  const [templatePick, setTemplatePick] = useState("");
  const [saveTemplateOpen, setSaveTemplateOpen] = useState(false);
  const [saveTemplateName, setSaveTemplateName] = useState("");
  const [searchParams, setSearchParams] = useSearchParams();
  const templateParam = searchParams.get("template");

  const nextId = useRef(0);
  const abortRef = useRef<AbortController | null>(null);
  const enhanceAbortRef = useRef<AbortController | null>(null);
  const streaming = turns.some((turn) => turn.status === "streaming");
  const needsKey = system.data?.require_api_key === true;
  // Sending without a key is a 401 when enforcement is on, so the composer waits for ours.
  const keyMissing = needsKey && apiKey.trim() === "";

  // First usable model is the useful default: the page should send on first open.
  useEffect(() => {
    if (model !== "" || !models.data?.length) return;
    // Prefer an id the provider itself listed: catalog rows can name a model the
    // account cannot actually call (a first click should not 404).
    const usable = models.data.filter((m) => m.enabled && m.available);
    const pick =
      usable.find((m) => m.source === "upstream") ?? usable[0] ?? models.data[0];
    if (pick) setModel(pick.ref);
  }, [model, models.data]);

  // Escape stops a stream even when focus left the composer.
  useEffect(() => {
    if (!streaming && !enhancing) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        abortRef.current?.abort();
        enhanceAbortRef.current?.abort();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [streaming, enhancing]);

  // A route change must not leave an SSE connection running in the background.
  useEffect(
    () => () => {
      abortRef.current?.abort();
      enhanceAbortRef.current?.abort();
    },
    [],
  );

  // "Open in Playground" on the Templates page hands the body over as a query param.
  // Drop the param once the list resolves: a later reload must not overwrite an edit.
  useEffect(() => {
    if (!templateParam || !templates.data) return;
    const row = templates.data.find((template) => template.id === templateParam);
    if (row) {
      setSystemPrompt(row.body);
      setSettingsOpen(true);
    }
    const next = new URLSearchParams(searchParams);
    next.delete("template");
    setSearchParams(next, { replace: true });
  }, [templateParam, templates.data, searchParams, setSearchParams]);

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
      send();
    } else if (event.key === "Escape" && abortRef.current) {
      event.preventDefault();
      abortRef.current.abort();
    }
  };

  const send = () => {
    const text = draft.trim();
    if (!text || model === "" || streaming) return;
    if (needsKey && apiKey.trim() === "") return;

    const controller = new AbortController();
    abortRef.current = controller;

    const userId = nextId.current++;
    const assistantId = nextId.current++;
    const startedAt = performance.now();
    const endpoint = ENDPOINTS[dialect];
    const temperature = parseTemperature(temp);
    const systemText = systemPrompt.trim();
    const key = apiKey.trim();

    // Completed turns only: an aborted or failed reply is not context the model wrote.
    const history = turns
      .filter((turn) => turn.role === "user" || turn.status === "done")
      .filter((turn) => turn.text.trim() !== "")
      .map((turn) => ({ role: turn.role, content: turn.text }));
    const messages = [...history, { role: "user" as const, content: text }];

    const wire: Record<string, unknown> = { model, stream: true, temperature };
    if (dialect === "chat") {
      wire.messages = systemText ? [{ role: "system", content: systemText }, ...messages] : messages;
    } else {
      wire.max_tokens = MAX_OUTPUT_TOKENS;
      wire.messages = messages;
      if (systemText) wire.system = systemText;
    }

    // The gateway accepts this key only with the playground marker (and an admin session,
    // which the browser already carries), so every call here identifies itself.
    const headers: Record<string, string> = {
      "content-type": "application/json",
      ...PLAYGROUND_HEADERS,
    };
    if (dialect === "messages") headers["anthropic-version"] = "2023-06-01";
    if (key) {
      if (dialect === "messages") headers["x-api-key"] = key;
      else headers.authorization = `Bearer ${key}`;
    }

    setTurns((prev) => [
      ...prev,
      {
        id: userId,
        role: "user",
        model: null,
        text,
        status: "done",
        error: null,
        raw: "",
        ttft: null,
        latency: null,
        promptTokens: null,
        completionTokens: null,
      },
      {
        id: assistantId,
        role: "assistant",
        model,
        text: "",
        status: "streaming",
        error: null,
        raw: "",
        ttft: null,
        latency: null,
        promptTokens: null,
        completionTokens: null,
      },
    ]);
    setDraft("");

    let ttft: number | null = null;
    let raw = "";
    let promptTokens: number | null = null;
    let completionTokens: number | null = null;
    let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;

    const update = (patch: Partial<Turn>) =>
      setTurns((prev) =>
        prev.map((turn) => (turn.id === assistantId ? { ...turn, ...patch } : turn)),
      );

    const append = (piece: string) => {
      if (!piece) return;
      if (ttft === null) ttft = Math.round(performance.now() - startedAt);
      const firstTokenAt = ttft;
      setTurns((prev) =>
        prev.map((turn) =>
          turn.id === assistantId ? { ...turn, text: turn.text + piece, ttft: firstTokenAt } : turn,
        ),
      );
    };

    /** One SSE frame → a delta. Returns "stop" on the dialect's terminal frame. */
    const applyFrame = (frame: string): "stop" | null => {
      const parsed = parseSseFrame(frame);
      if (!parsed) return null;
      const payload = parsed.data.trim();
      if (payload === "[DONE]") return "stop";
      if (!payload) return null;
      let json: unknown;
      try {
        json = JSON.parse(payload);
      } catch {
        // A non-JSON data line (some gates emit plain text) stays in `raw` only.
        return null;
      }
      const delta = dialect === "chat" ? chatDelta(json) : messagesDelta(json, parsed.event);
      if (!delta) return null;
      if (delta.error) throw new Error(delta.error);
      if (delta.text) append(delta.text);
      if (delta.prompt !== undefined) promptTokens = delta.prompt;
      if (delta.completion !== undefined) completionTokens = delta.completion;
      return delta.done ? "stop" : null;
    };

    void (async () => {
      try {
        const response = await fetch(endpoint, {
          method: "POST",
          headers,
          body: JSON.stringify(wire),
          signal: controller.signal,
        });
        if (!response.ok) {
          throw new Error(httpErrorMessage(await response.text(), response.status));
        }
        if (!response.body) throw new Error("The gateway returned no response body.");

        reader = response.body.getReader();
        const decoder = new TextDecoder();
        let buffer = "";
        let finished = false;

        while (!finished) {
          const { done, value } = await reader.read();
          if (done) break;
          const chunk = decoder.decode(value, { stream: true });
          raw += chunk;
          // Concatenate before normalising so a CRLF split across chunks still matches.
          buffer = `${buffer}${chunk}`.replace(/\r\n/g, "\n");
          let cut = buffer.indexOf("\n\n");
          while (cut !== -1) {
            const frame = buffer.slice(0, cut);
            buffer = buffer.slice(cut + 2);
            if (applyFrame(frame) === "stop") {
              finished = true;
              break;
            }
            cut = buffer.indexOf("\n\n");
          }
        }
        // Some upstreams close without the blank-line terminator on the last frame.
        if (!finished && buffer.trim() !== "") applyFrame(buffer);

        update({
          status: "done",
          latency: Math.round(performance.now() - startedAt),
          raw,
          promptTokens,
          completionTokens,
        });
      } catch (error) {
        const aborted = controller.signal.aborted;
        update({
          status: aborted ? "stopped" : "error",
          error: aborted ? null : error instanceof Error ? error.message : String(error),
          latency: Math.round(performance.now() - startedAt),
          raw,
          promptTokens,
          completionTokens,
        });
      } finally {
        if (reader) await reader.cancel().catch(() => undefined);
        if (abortRef.current === controller) abortRef.current = null;
      }
    })();
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
          onText: (soFar) => setDraft(soFar),
        });
        if (rewrite.trim() === "") setEnhanceError("The model returned an empty rewrite.");
      } catch (error) {
        // An abort is a deliberate stop, not a failure worth a message.
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
    setDraft(undoDraft);
    setUndoDraft(null);
  };

  const modelHint = models.isLoading
    ? "Loading the registry…"
    : models.isError
      ? "The model registry could not be loaded."
      : (models.data?.length ?? 0) === 0
        ? "No models are registered yet."
        : undefined;

  const modelLabel = models.data?.find((m) => m.ref === model)?.display ?? model;

  return (
    <div className="mx-auto flex min-h-[calc(100dvh-7rem)] w-full max-w-[880px] flex-col gap-4 pb-2">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <Heading level={1} className="text-subtitle font-semibold leading-tight tracking-[-0.02em] text-fg">
            Playground
          </Heading>
          <p className="mt-0.5 text-small text-muted">
            A live conversation through the gateway. Same endpoint, headers and SSE frames a client sees.
          </p>
        </div>
        <div className="flex items-center gap-2">
          <span className="hidden font-mono text-caption text-faint min-[700px]:inline">
            {ENDPOINTS[dialect]}
          </span>
          <Button onClick={() => setSettingsOpen(true)}>
            <SlidersHorizontal size={14} aria-hidden />
            Parameters
          </Button>
        </div>
      </header>

      {models.isError ? <ErrorNote error={models.error} onRetry={() => void models.refetch()} /> : null}
      {key.isError ? (
        <ErrorNote error={key.error} onRetry={() => void key.refetch()} />
      ) : keyMissing && !key.isLoading ? (
        <div className="rounded-[8px] border border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] px-3 py-2 text-small text-[var(--color-warn-ink)]">
          This gateway requires a key and the playground's own key was not issued.
        </div>
      ) : null}

      <div className="flex min-h-[280px] flex-1 flex-col gap-5 rounded-[12px] border border-line bg-[var(--color-bg)] px-4 py-5 min-[700px]:px-6">
        {turns.length === 0 ? (
          <EmptyPlayground
            model={modelLabel}
            onPick={(prompt) => setDraft(prompt)}
          />
        ) : (
          turns.map((turn) => <TurnCard key={turn.id} turn={turn} />)
        )}
      </div>

      {enhanceError ? (
        <p role="alert" className="px-2 pb-1 text-caption text-neg">
          {enhanceError}
        </p>
      ) : null}

      <div className="z-10 px-1 pb-1 pt-1">
        <div className="rounded-[16px] border border-line-strong bg-surface shadow-[0_1px_2px_rgba(16,18,20,0.05)] focus-within:border-[var(--color-accent-text)]">
          <textarea
            value={draft}
            onChange={(event) => {
              setDraft(event.target.value);
              setUndoDraft(null);
            }}
            onKeyDown={onComposerKeyDown}
            rows={2}
            aria-label="message"
            placeholder={model ? `Message ${modelLabel}…` : "Choose a model in Parameters to start…"}
            style={{ outline: "none" }}
            className="block max-h-[220px] w-full resize-none rounded-t-[16px] bg-transparent px-4 pt-3.5 text-lead leading-relaxed text-fg placeholder:text-faint focus:outline-none focus-visible:outline-none"
          />
          <div className="flex flex-wrap items-center gap-2 px-3 pb-2.5 pt-1">
            <div className="min-w-[160px] max-w-[240px] flex-1">
              <ModelSelect value={model} onChange={setModel} ariaLabel="model" plain />
            </div>
            <span className="hidden text-caption text-faint min-[560px]:inline">
              {temp === "1" ? "temperature 1" : `temperature ${temp}`}
            </span>
            <span className="ml-auto hidden items-center gap-1 text-caption text-faint min-[820px]:flex">
              <kbd className={kbdClass}>⌘↵</kbd> send · <kbd className={kbdClass}>Esc</kbd> stop
            </span>
            {enhancing ? (
              <button
                type="button"
                onClick={() => enhanceAbortRef.current?.abort()}
                aria-label="stop enhancing"
                title="Stop the rewrite"
                className="flex h-8 items-center gap-1.5 rounded-full border border-line-strong bg-surface px-2.5 text-small text-fg hover:bg-surface-3"
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
                className="flex h-8 items-center gap-1.5 rounded-full border border-line bg-surface-2 px-2.5 text-small text-muted transition-colors hover:border-line-strong hover:text-fg disabled:cursor-not-allowed disabled:opacity-40"
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
                onClick={() => abortRef.current?.abort()}
                aria-label="stop generating"
                title="Stop"
                className="flex h-9 w-9 items-center justify-center rounded-full border border-line-strong bg-surface text-fg hover:bg-surface-3"
              >
                <Square size={13} weight="fill" aria-hidden />
              </button>
            ) : (
              <button
                type="button"
                onClick={send}
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

      <Modal
        title="Parameters"
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        width="lg"
        description="Request shape, credentials and instructions for this conversation."
        footer={<Button onClick={() => setSettingsOpen(false)}>done</Button>}
      >
        <div className="flex flex-col gap-5">
          <div className="flex flex-wrap items-end gap-3">
            <div className="min-w-[220px] flex-1">
              <Field label="Model" hint={modelHint}>
                <ModelSelect value={model} onChange={setModel} ariaLabel="model" />
              </Field>
            </div>
            <div className="w-[150px]">
              <Field label="Temperature" hint={dialect === "messages" ? "Anthropic accepts 0–1" : undefined}>
                <input
                  type="number"
                  min={0}
                  max={2}
                  step={0.1}
                  value={temp}
                  aria-label="temperature"
                  onChange={(event) => setTemp(event.target.value)}
                  onBlur={() => setTemp(String(parseTemperature(temp)))}
                  className={inputClass}
                />
              </Field>
            </div>
          </div>

          <Field label="Endpoint" hint="The body is built for this dialect; the gateway translates the reply back.">
            <div className="flex items-center gap-3">
              <Segmented options={DIALECTS} value={dialect} onChange={setDialect} ariaLabel="endpoint dialect" />
              <code className="truncate font-mono text-caption text-muted">{ENDPOINTS[dialect]}</code>
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
                {key.isError ? "the key could not be issued" : "minting the key…"}
              </span>
            )}
          </Field>

          <div className="border-t border-line pt-4">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <Field label="System prompt" hint="Optional instructions sent before the conversation.">
                <textarea
                  id="playground-system-prompt"
                  value={systemPrompt}
                  onChange={(event) => setSystemPrompt(event.target.value)}
                  rows={4}
                  aria-label="system prompt"
                  placeholder="You are a concise assistant. Answer in the language of the question."
                  className={textareaClass}
                />
              </Field>
            </div>
            <div className="mt-3 flex flex-wrap items-center gap-2">
              <Select
                value={templatePick}
                onChange={(id) => {
                  setTemplatePick("");
                  const row = templates.data?.find((template) => template.id === id);
                  if (!row) return;
                  setSystemPrompt(row.body);
                }}
                ariaLabel="load a saved template into the system prompt"
                disabled={templates.isLoading || !templates.data?.length}
                className="min-w-[200px] flex-1"
              >
                <option value="">
                  {templates.isError
                    ? "templates unavailable"
                    : templates.isLoading
                      ? "loading templates…"
                      : templates.data?.length
                        ? "insert template…"
                        : "no templates"}
                </option>
                {templates.data?.map((template) => (
                  <option key={template.id} value={template.id}>
                    {template.name}
                  </option>
                ))}
              </Select>
              <Button
                onClick={() => setSaveTemplateOpen(true)}
                disabled={systemPrompt.trim() === ""}
                title={systemPrompt.trim() === "" ? "Write a system prompt first" : "Save this prompt as a template"}
              >
                Save as template
              </Button>
            </div>
          </div>
        </div>
      </Modal>

      {saveTemplateOpen ? (
        <Modal
          title="Save as template"
          open
          onClose={() => setSaveTemplateOpen(false)}
          footer={
            <>
              <Button onClick={() => setSaveTemplateOpen(false)}>cancel</Button>
              <Button
                variant="primary"
                disabled={saveTemplateName.trim() === "" || createTemplate.isPending}
                onClick={() =>
                  createTemplate.mutate(
                    { name: saveTemplateName.trim(), body: systemPrompt },
                    {
                      onSuccess: () => {
                        setSaveTemplateOpen(false);
                        setSaveTemplateName("");
                      },
                    },
                  )
                }
              >
                {createTemplate.isPending ? "saving…" : "save template"}
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
    </div>
  );
}

function TurnCard({ turn }: { turn: Turn }) {
  const [rawOpen, setRawOpen] = useState(false);
  const assistant = turn.role === "assistant";

  if (!assistant) {
    return (
      <article className="ml-auto max-w-[85%] rounded-[14px] bg-surface-3 px-4 py-2.5">
        <p className="whitespace-pre-wrap break-words text-body leading-relaxed text-fg">
          {turn.text}
        </p>
      </article>
    );
  }

  return (
    <article className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-2">
        <span aria-hidden className="h-2 w-2 rounded-full bg-[var(--color-accent)]" />
        <span className="font-mono text-caption text-muted">{turn.model ?? "assistant"}</span>
        <span className={cx("text-caption", turn.status === "error" ? "text-neg" : "text-faint")}>
          {STATUS_LABEL[turn.status]}
        </span>
        {turn.latency !== null ? (
          <span className="ml-auto font-mono text-caption text-faint tnum">
            {turn.ttft !== null ? `first token ${ms(turn.ttft)} · ` : ""}
            {ms(turn.latency)}
            {turn.promptTokens !== null || turn.completionTokens !== null
              ? ` · ${turn.promptTokens ?? "—"} in / ${turn.completionTokens ?? "—"} out`
              : ""}
          </span>
        ) : null}
      </div>

      <div
        aria-live={turn.status === "streaming" ? "polite" : undefined}
        aria-busy={turn.status === "streaming" ? true : undefined}
        className="text-lead leading-7 text-fg"
      >
        {turn.text ? (
          <p className="whitespace-pre-wrap break-words">
            {turn.text}
            {turn.status === "streaming" ? (
              <span
                aria-hidden
                className="ml-0.5 inline-block h-4 w-0.5 translate-y-0.5 bg-accent motion-safe:animate-pulse"
              />
            ) : null}
          </p>
        ) : turn.status !== "error" ? (
          <p className="text-body text-faint">
            {turn.status === "streaming"
              ? "Waiting for the first token…"
              : turn.status === "stopped"
                ? "Stopped before any content arrived."
                : "The provider returned no text content."}
          </p>
        ) : null}
        {turn.error ? (
          <div className="mt-2">
            <ErrorNote error={turn.error} />
          </div>
        ) : null}
      </div>

      {turn.status !== "streaming" ? (
        <div>
          <button
            type="button"
            onClick={() => setRawOpen((open) => !open)}
            className="text-caption text-faint underline-offset-2 hover:text-fg hover:underline"
          >
            {rawOpen ? "hide raw frames" : "raw frames"}
          </button>
          {rawOpen ? (
            <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap break-all rounded-[8px] border border-line bg-surface-2 p-3 font-mono text-caption leading-relaxed text-muted">
              {turn.raw || "No frames were received."}
            </pre>
          ) : null}
        </div>
      ) : null}
    </article>
  );
}

/** The calm first screen: say what this place does, then offer real starting prompts. */
function EmptyPlayground({ model, onPick }: { model: string; onPick: (prompt: string) => void }) {
  const prompts = [
    "Explain what this gateway does in two sentences.",
    "Write a SQL query that joins users to their latest order.",
    "Summarise the risks of sending customer data to three providers.",
    "Draft a one-paragraph changelog for a retry policy.",
  ];
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-6 pb-8 pt-6 text-center">
      <div>
        <Heading level={2} className="text-title font-semibold tracking-[-0.02em] text-fg">
          What do you want to test?
        </Heading>
        <p className="mx-auto mt-2 max-w-[46ch] text-body leading-relaxed text-muted">
          {model
            ? `Messages go to ${model} through the real gateway. Nothing is simulated.`
            : "Pick a model in Parameters, then send a message to see the real reply stream."}
        </p>
      </div>
      <div className="grid w-full max-w-[560px] gap-2 min-[640px]:grid-cols-2">
        {prompts.map((prompt) => (
          <button
            key={prompt}
            type="button"
            onClick={() => onPick(prompt)}
            className="rounded-[10px] border border-line bg-surface px-3.5 py-2.5 text-left text-small leading-relaxed text-ink-soft transition-colors hover:border-line-strong hover:bg-surface-2"
          >
            {prompt}
          </button>
        ))}
      </div>
      <p className="text-caption text-faint">
        <kbd className={kbdClass}>⌘↵</kbd> send · <kbd className={kbdClass}>Esc</kbd> stop a stream
      </p>
    </div>
  );
}

// ---------------------------------------------------------------- styling

const inputClass =
  "h-8 w-full min-w-0 rounded-lg border border-line bg-surface-2 px-2.5 text-body text-fg placeholder:text-faint focus:border-[var(--color-accent)]/60 focus:outline-none";

const textareaClass =
  "w-full resize-y rounded-lg border border-line bg-surface-2 px-2.5 py-2 text-body text-fg placeholder:text-faint focus:border-[var(--color-accent)]/60 focus:outline-none";

const kbdClass =
  "rounded border border-line bg-surface-2 px-1 py-0.5 font-sans text-caption text-muted";

// ---------------------------------------------------------------- streaming

type Json = Record<string, unknown>;

type FrameDelta = {
  text?: string;
  prompt?: number;
  completion?: number;
  done?: boolean;
  error?: string;
};

type SseFrame = { event: string | null; data: string };

/**
 * One SSE frame: `event:` names (Anthropic), any number of `data:` lines joined with a
 * newline per the SSE spec, comments and blank lines ignored.
 */
function parseSseFrame(frame: string): SseFrame | null {
  let event: string | null = null;
  const data: string[] = [];
  for (const line of frame.split("\n")) {
    if (line === "" || line.startsWith(":")) continue;
    if (line.startsWith("event:")) {
      event = line.slice(6).trim() || null;
    } else if (line.startsWith("data:")) {
      const rest = line.slice(5);
      data.push(rest.startsWith(" ") ? rest.slice(1) : rest);
    }
  }
  if (data.length === 0) return null;
  return { event, data: data.join("\n") };
}

/** OpenAI chat: content lives in `choices[0].delta.content`; usage in the last chunk. */
function chatDelta(json: unknown): FrameDelta | null {
  const record = asRecord(json);
  if (!record) return null;
  const delta: FrameDelta = {};

  const error = asRecord(record.error);
  if (error) delta.error = str(error.message) ?? "the gateway reported a stream error";

  const choices = Array.isArray(record.choices) ? record.choices : [];
  const choice = asRecord(choices[0]);
  const piece = choice ? asRecord(choice.delta) : null;
  const content = piece ? str(piece.content) : null;
  if (content) delta.text = content;

  const usage = asRecord(record.usage);
  if (usage) {
    const prompt = num(usage.prompt_tokens);
    const completion = num(usage.completion_tokens);
    if (prompt !== null) delta.prompt = prompt;
    if (completion !== null) delta.completion = completion;
  }
  return delta;
}

/** Anthropic messages: text arrives as `content_block_delta` text deltas. */
function messagesDelta(json: unknown, event: string | null): FrameDelta | null {
  const record = asRecord(json);
  if (!record) return null;
  const type = str(record.type) ?? event;
  const delta: FrameDelta = {};

  switch (type) {
    case "message_start": {
      const message = asRecord(record.message);
      applyAnthropicUsage(message ? asRecord(message.usage) : null, delta);
      break;
    }
    case "content_block_delta": {
      const block = asRecord(record.delta);
      if (block && str(block.type) === "text_delta") {
        const text = str(block.text);
        if (text) delta.text = text;
      }
      break;
    }
    case "message_delta":
      applyAnthropicUsage(asRecord(record.usage), delta);
      break;
    case "message_stop":
      delta.done = true;
      break;
    case "error":
      delta.error =
        str(asRecord(record.error)?.message) ??
        str(record.message) ??
        "the gateway reported a stream error";
      break;
    default:
      break;
  }
  return delta;
}

/**
 * Gateway-encoded streams zero-fill usage (the `message_start` placeholder is always
 * 0/0), so only a positive count is evidence the provider actually reported one.
 */
function applyAnthropicUsage(usage: Json | null, delta: FrameDelta) {
  if (!usage) return;
  const prompt = num(usage.input_tokens);
  const completion = num(usage.output_tokens);
  if (prompt !== null && prompt > 0) delta.prompt = prompt;
  if (completion !== null && completion > 0) delta.completion = completion;
}

function asRecord(value: unknown): Json | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Json)
    : null;
}

function str(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function num(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/** Errors are inline text, never `alert`: keep the status and the server's message. */
function httpErrorMessage(body: string, status: number): string {
  let parsed: unknown;
  try {
    parsed = JSON.parse(body);
  } catch {
    parsed = null;
  }
  const record = asRecord(parsed);
  if (record) {
    const error = asRecord(record.error);
    const message = (error ? str(error.message) : null) ?? str(record.message);
    if (message) return `${status} ${statusText(status)} · ${message}`;
  }
  const text = body.trim();
  if (!text) return `${status} ${statusText(status)}`;
  const short = text.length > 400 ? `${text.slice(0, 400)}…` : text;
  return `${status} ${statusText(status)} · ${short}`;
}

function parseTemperature(value: string): number {
  const parsed = Number.parseFloat(value);
  if (!Number.isFinite(parsed)) return 1;
  return Math.min(2, Math.max(0, parsed));
}

const STATUS_LABEL: Record<TurnStatus, string> = {
  streaming: "streaming",
  done: "complete",
  stopped: "stopped",
  error: "failed",
};
