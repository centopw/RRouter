import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { ModelSelect } from "../components/model-select";
import {
  Button,
  Card,
  CardHeader,
  ErrorNote,
  Field,
  Input,
  Segmented,
  cx,
  statusText,
} from "../components/ui";
import { ms } from "../lib/format";
import { useModels, useSystem } from "../lib/queries";

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

/** Session-only: a playground key must not outlive the tab or leak into a backup. */
const KEY_STORAGE = "rrouter:playground:key";

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
  const [systemOpen, setSystemOpen] = useState(false);
  const [temp, setTemp] = useState("1");
  const [draft, setDraft] = useState("");
  const [turns, setTurns] = useState<Turn[]>([]);
  const [apiKey, setApiKey] = useState(() => {
    try {
      return sessionStorage.getItem(KEY_STORAGE) ?? "";
    } catch {
      return "";
    }
  });

  const nextId = useRef(0);
  const abortRef = useRef<AbortController | null>(null);
  const streaming = turns.some((turn) => turn.status === "streaming");
  const needsKey = system.data?.require_api_key === true;
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
    if (!streaming) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") abortRef.current?.abort();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [streaming]);

  // A route change must not leave an SSE connection running in the background.
  useEffect(() => () => abortRef.current?.abort(), []);

  const rememberKey = (value: string) => {
    setApiKey(value);
    try {
      if (value) sessionStorage.setItem(KEY_STORAGE, value);
      else sessionStorage.removeItem(KEY_STORAGE);
    } catch {
      // Storage can be blocked; the field still works for this mount.
    }
  };

  const canSend = draft.trim() !== "" && model !== "" && !streaming && !keyMissing;
  const sendBlocker = keyMissing
    ? "This gateway requires an API key"
    : model === ""
      ? "Choose a model first"
      : draft.trim() === ""
        ? "Type a message first"
        : null;

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

    const headers: Record<string, string> = { "content-type": "application/json" };
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

  const modelHint = models.isLoading
    ? "Loading the registry…"
    : models.isError
      ? "The model registry could not be loaded."
      : (models.data?.length ?? 0) === 0
        ? "No models are registered yet."
        : undefined;

  return (
    <div className="space-y-5">
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 className="text-[30px] font-semibold tracking-[-0.02em] text-fg">Playground</h1>
          <p className="mt-1 text-[13.5px] text-muted">
            Send a real request through the gateway and watch the model answer token by token.
          </p>
        </div>
        <Segmented
          options={DIALECTS}
          value={dialect}
          onChange={setDialect}
          ariaLabel="endpoint dialect"
        />
      </header>

      <Card>
        <CardHeader
          title="Request"
          action={<span className="font-mono text-[11px] text-faint">{ENDPOINTS[dialect]}</span>}
        />
        {models.isError ? (
          <div className="px-5 pb-4">
            <ErrorNote error={models.error} onRetry={() => void models.refetch()} />
          </div>
        ) : null}
        <div className="flex flex-wrap items-end gap-3 px-5 pb-4">
          <div className="min-w-[240px] flex-1">
            <Field label="Model" hint={modelHint}>
              <ModelSelect value={model} onChange={setModel} ariaLabel="model" />
            </Field>
          </div>
          {needsKey ? (
            <div className="min-w-[220px] flex-1">
              <Field label="API key" hint="Kept in this tab's session only.">
                <Input
                  type="password"
                  value={apiKey}
                  onChange={rememberKey}
                  placeholder="sk-…"
                  ariaLabel="gateway API key"
                />
              </Field>
            </div>
          ) : null}
          <div className="w-[132px]">
            <Field
              label="Temperature"
              hint={dialect === "messages" ? "Anthropic accepts 0–1" : undefined}
            >
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
        <div className="border-t border-line px-5 py-3">
          <button
            type="button"
            aria-expanded={systemOpen}
            aria-controls="playground-system-prompt"
            onClick={() => setSystemOpen((open) => !open)}
            className="flex items-center gap-2 text-[12px] font-medium text-muted transition-colors hover:text-fg"
          >
            <span aria-hidden className="text-faint">
              {systemOpen ? "▾" : "▸"}
            </span>
            System prompt
            {systemPrompt.trim() ? (
              <span className="rounded-full border border-line px-1.5 py-0.5 text-[10px] text-faint">
                set
              </span>
            ) : null}
          </button>
          {systemOpen ? (
            <textarea
              id="playground-system-prompt"
              value={systemPrompt}
              onChange={(event) => setSystemPrompt(event.target.value)}
              rows={3}
              aria-label="system prompt"
              placeholder="Optional: instructions the model receives before the conversation."
              className={cx(textareaClass, "mt-2")}
            />
          ) : null}
        </div>
      </Card>

      <section className="space-y-3" aria-label="Conversation">
        {turns.length === 0 ? (
          <EmptyPlayground />
        ) : (
          turns.map((turn) => <TurnCard key={turn.id} turn={turn} />)
        )}
      </section>

      <Card>
        <div className="p-4">
          <textarea
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={onComposerKeyDown}
            rows={3}
            aria-label="message"
            placeholder={model ? `Message ${model}…` : "Choose a model to start…"}
            className={textareaClass}
          />
          <div className="mt-3 flex flex-wrap items-center justify-between gap-2">
            <p className="text-[11px] text-faint">
              <kbd className={kbdClass}>⌘↵</kbd> or <kbd className={kbdClass}>Ctrl↵</kbd> to send ·{" "}
              <kbd className={kbdClass}>Esc</kbd> to stop
            </p>
            {streaming ? (
              <Button variant="danger" onClick={() => abortRef.current?.abort()}>
                Stop
              </Button>
            ) : (
              <Button
                variant="primary"
                onClick={send}
                disabled={!canSend}
                title={canSend ? undefined : (sendBlocker ?? undefined)}
              >
                Send
              </Button>
            )}
          </div>
        </div>
      </Card>
    </div>
  );
}

function TurnCard({ turn }: { turn: Turn }) {
  const [rawOpen, setRawOpen] = useState(false);
  const assistant = turn.role === "assistant";

  return (
    <Card as="article">
      <div className="flex flex-wrap items-center justify-between gap-2 px-5 pt-3.5 pb-1">
        <div className="flex min-w-0 items-center gap-2">
          <span className="text-[11px] font-medium uppercase tracking-[0.08em] text-faint">
            {assistant ? "Assistant" : "You"}
          </span>
          {turn.model ? (
            <span className="truncate font-mono text-[11px] text-faint">{turn.model}</span>
          ) : null}
        </div>
        {assistant ? (
          <span className={cx("text-[11px]", turn.status === "error" ? "text-neg" : "text-faint")}>
            {STATUS_LABEL[turn.status]}
          </span>
        ) : null}
      </div>

      <div
        aria-live={turn.status === "streaming" ? "polite" : undefined}
        aria-busy={turn.status === "streaming" ? true : undefined}
        className="px-5 pb-4 pt-1"
      >
        {turn.text ? (
          <p className="whitespace-pre-wrap break-words text-[13.5px] leading-relaxed text-fg">
            {turn.text}
            {turn.status === "streaming" ? (
              <span
                aria-hidden
                className="ml-0.5 inline-block h-3.5 w-0.5 translate-y-0.5 bg-accent motion-safe:animate-pulse"
              />
            ) : null}
          </p>
        ) : assistant && turn.status !== "error" ? (
          <p className="text-[13px] text-faint">
            {turn.status === "streaming"
              ? "Waiting for the first token…"
              : turn.status === "stopped"
                ? "Stopped before any content arrived."
                : "The provider returned no text content."}
          </p>
        ) : null}
        {turn.error ? (
          <div className="mt-3">
            <ErrorNote error={turn.error} />
          </div>
        ) : null}
      </div>

      {assistant ? (
        <>
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2 border-t border-line px-5 py-2 tnum">
            <span className="font-mono text-[11px] text-faint">
              TTFT{" "}
              <span className="text-muted">{turn.ttft === null ? "—" : ms(turn.ttft)}</span>
            </span>
            <span className="font-mono text-[11px] text-faint">
              total{" "}
              <span className="text-muted">
                {turn.latency === null ? "—" : ms(turn.latency)}
              </span>
            </span>
            {turn.promptTokens !== null || turn.completionTokens !== null ? (
              <span className="font-mono text-[11px] text-faint">
                tokens{" "}
                <span className="text-muted">
                  {turn.promptTokens ?? "—"} in / {turn.completionTokens ?? "—"} out
                </span>
              </span>
            ) : null}
            {turn.status !== "streaming" ? (
              <Button
                variant="quiet"
                className="ml-auto"
                onClick={() => setRawOpen((open) => !open)}
              >
                {rawOpen ? "Hide raw" : "Raw"}
              </Button>
            ) : null}
          </div>
          {rawOpen ? (
            <div className="border-t border-line px-5 py-3">
              <p className="mb-1.5 text-[11px] text-faint">Unparsed SSE frames</p>
              {turn.raw ? (
                <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-[var(--color-bg)] p-3 font-mono text-[11px] leading-relaxed text-faint">
                  {turn.raw}
                </pre>
              ) : (
                <p className="text-[12px] text-faint">No frames were received.</p>
              )}
            </div>
          ) : null}
        </>
      ) : null}
    </Card>
  );
}

function EmptyPlayground() {
  return (
    <Card className="px-5 py-10">
      <div className="mx-auto max-w-md text-center">
        <h2 className="text-[14px] font-medium text-fg">Nothing sent yet</h2>
        <p className="mt-1.5 text-[13px] leading-relaxed text-muted">
          Send a prompt to call a real model through this gateway and watch the reply stream in.
          Each answer reports time to first token, total latency, the tokens the provider returned,
          and the raw SSE frames behind it.
        </p>
        <p className="mt-3 font-mono text-[11px] text-faint">
          ⌘↵ / Ctrl↵ to send · Esc to stop a stream
        </p>
      </div>
    </Card>
  );
}

// ---------------------------------------------------------------- styling

const inputClass =
  "h-8 w-full min-w-0 rounded-lg border border-line bg-surface-2 px-2.5 text-[13px] text-fg placeholder:text-faint focus:border-[var(--color-accent)]/60 focus:outline-none";

const textareaClass =
  "w-full resize-y rounded-lg border border-line bg-surface-2 px-2.5 py-2 text-[13px] text-fg placeholder:text-faint focus:border-[var(--color-accent)]/60 focus:outline-none";

const kbdClass =
  "rounded border border-line bg-surface-2 px-1 py-0.5 font-sans text-[10px] text-muted";

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
