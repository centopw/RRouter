import { useState } from "react";
import { CaretDown, CaretRight, Wrench } from "@phosphor-icons/react";
import { Badge, ErrorNote, Heading, StatusPill, cx, statusText } from "../../components/ui";
import { ms, splitRef } from "../../lib/format";
import { useUsageDetail } from "../../lib/queries";

export type TurnStatus = "streaming" | "done" | "stopped" | "error";

export type ToolCallPart = {
  id: string;
  name: string;
  arguments: string;
};

export type Turn = {
  id: number;
  role: "user" | "assistant";
  /** The model that produced an assistant turn, frozen so later selects do not rewrite it. */
  model: string | null;
  text: string;
  /** Thinking / reasoning trace streamed by reasoning-capable models. */
  reasoning: string;
  /** Function / tool calls accumulated during the turn. */
  toolCalls: ToolCallPart[];
  status: TurnStatus;
  error: string | null;
  /** Gateway `x-request-id` header for attempt waterfall inspection. */
  requestId: string | null;
  /** Gateway HTTP status code before SSE frame parsing. */
  httpStatus: number | null;
  /** Verbatim bytes from the response, shown behind the Raw toggle. */
  raw: string;
  ttft: number | null;
  latency: number | null;
  promptTokens: number | null;
  completionTokens: number | null;
};

export type ToolCallDelta = { index: number; id?: string; name?: string; argsChunk?: string };

export type FrameDelta = {
  text?: string;
  reasoning?: string;
  toolCalls?: ToolCallDelta[];
  prompt?: number;
  completion?: number;
  done?: boolean;
  error?: string;
};

export function createTurn(
  init: Pick<Turn, "id" | "role" | "model" | "text" | "status">,
): Turn {
  return {
    ...init,
    reasoning: "",
    toolCalls: [],
    error: null,
    requestId: null,
    httpStatus: null,
    raw: "",
    ttft: null,
    latency: null,
    promptTokens: null,
    completionTokens: null,
  };
}

export function mergeToolCalls(current: ToolCallPart[], deltas: ToolCallDelta[]): ToolCallPart[] {
  if (deltas.length === 0) return current;
  const next = current.map((entry) => ({ ...entry }));
  for (const delta of deltas) {
    while (next.length <= delta.index) next.push({ id: "", name: "", arguments: "" });
    const slot = next[delta.index];
    if (!slot) continue;
    if (delta.id) slot.id = delta.id;
    if (delta.name) slot.name = slot.name ? `${slot.name}${delta.name}` : delta.name;
    if (delta.argsChunk) slot.arguments += delta.argsChunk;
  }
  return next;
}

export function parseSseFrame(frame: string): { event: string | null; data: string } | null {
  let event: string | null = null;
  const data: string[] = [];
  for (const line of frame.split("\n")) {
    if (line === "" || line.startsWith(":")) continue;
    if (line.startsWith("event:")) event = line.slice(6).trim() || null;
    else if (line.startsWith("data:")) {
      const rest = line.slice(5);
      data.push(rest.startsWith(" ") ? rest.slice(1) : rest);
    }
  }
  return data.length === 0 ? null : { event, data: data.join("\n") };
}

export function chatDelta(json: unknown): FrameDelta | null {
  const record = asRecord(json);
  if (!record) return null;
  const delta: FrameDelta = {};
  const error = asRecord(record.error);
  if (error) delta.error = str(error.message) ?? "the gateway reported a stream error";

  const choices = Array.isArray(record.choices) ? record.choices : [];
  const piece = asRecord(asRecord(choices[0])?.delta);
  if (piece) {
    const content = str(piece.content);
    if (content) delta.text = content;
    const reasoning = str(piece.reasoning_content) ?? str(piece.reasoning);
    if (reasoning) delta.reasoning = reasoning;
    if (Array.isArray(piece.tool_calls)) {
      const calls: ToolCallDelta[] = [];
      for (const item of piece.tool_calls) {
        const call = asRecord(item);
        const fn = call ? asRecord(call.function) : null;
        const index = call ? num(call.index) : null;
        if (!call || index === null) continue;
        calls.push({
          index,
          id: str(call.id) ?? undefined,
          name: fn ? (str(fn.name) ?? undefined) : undefined,
          argsChunk: fn ? (str(fn.arguments) ?? undefined) : undefined,
        });
      }
      if (calls.length > 0) delta.toolCalls = calls;
    }
  }

  const usage = asRecord(record.usage);
  if (usage) {
    const prompt = num(usage.prompt_tokens);
    const completion = num(usage.completion_tokens);
    if (prompt !== null) delta.prompt = prompt;
    if (completion !== null) delta.completion = completion;
  }
  return delta;
}

export function messagesDelta(
  json: unknown,
  event: string | null,
  toolIndexByBlock: Map<number, number>,
): FrameDelta | null {
  const record = asRecord(json);
  if (!record) return null;
  const type = str(record.type) ?? event;
  const delta: FrameDelta = {};

  switch (type) {
    case "message_start":
      applyAnthropicUsage(asRecord(asRecord(record.message)?.usage), delta);
      break;
    case "content_block_start": {
      const blockIndex = num(record.index);
      const block = asRecord(record.content_block);
      if (blockIndex !== null && block && str(block.type) === "tool_use") {
        const toolIndex = toolIndexByBlock.size;
        toolIndexByBlock.set(blockIndex, toolIndex);
        delta.toolCalls = [
          { index: toolIndex, id: str(block.id) ?? undefined, name: str(block.name) ?? undefined },
        ];
      }
      break;
    }
    case "content_block_delta": {
      const block = asRecord(record.delta);
      const blockType = block ? str(block.type) : null;
      if (block && blockType === "text_delta") {
        const text = str(block.text);
        if (text) delta.text = text;
      } else if (block && blockType === "thinking_delta") {
        const thinking = str(block.thinking);
        if (thinking) delta.reasoning = thinking;
      } else if (block && blockType === "input_json_delta") {
        const blockIndex = num(record.index);
        const toolIndex = blockIndex !== null ? toolIndexByBlock.get(blockIndex) : undefined;
        const partial = str(block.partial_json);
        if (toolIndex !== undefined && partial) {
          delta.toolCalls = [{ index: toolIndex, argsChunk: partial }];
        }
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
  }
  return delta;
}

export function httpErrorMessage(body: string, status: number): string {
  let parsed: unknown = null;
  try {
    parsed = JSON.parse(body);
  } catch {
    parsed = null;
  }
  const record = asRecord(parsed);
  const message =
    (record ? str(asRecord(record.error)?.message) ?? str(record.message) : null) ?? body.trim();
  if (!message) return `${status} ${statusText(status)}`;
  const short = message.length > 400 ? `${message.slice(0, 400)}…` : message;
  return `${status} ${statusText(status)} · ${short}`;
}

type Json = Record<string, unknown>;

function applyAnthropicUsage(usage: Json | null, delta: FrameDelta) {
  if (!usage) return;
  const prompt = num(usage.input_tokens);
  const completion = num(usage.output_tokens);
  if (prompt !== null && prompt > 0) delta.prompt = prompt;
  if (completion !== null && completion > 0) delta.completion = completion;
}

const asRecord = (v: unknown): Json | null =>
  typeof v === "object" && v !== null && !Array.isArray(v) ? (v as Json) : null;
const str = (v: unknown): string | null => (typeof v === "string" ? v : null);
const num = (v: unknown): number | null =>
  typeof v === "number" && Number.isFinite(v) ? v : null;

export const STATUS_LABEL: Record<TurnStatus, string> = {
  streaming: "streaming",
  done: "complete",
  stopped: "stopped",
  error: "failed",
};

const KBD_CLASS =
  "rounded border border-line bg-surface-2 px-1 py-0.5 font-sans text-caption text-muted";

export function TurnCard({ turn }: { turn: Turn }) {
  const [rawOpen, setRawOpen] = useState(false);
  const [reasoningToggled, setReasoningToggled] = useState<boolean | null>(null);
  const [waterfallOpen, setWaterfallOpen] = useState(false);

  const detail = useUsageDetail(
    turn.role === "assistant" && turn.status !== "streaming" ? turn.requestId : null,
  );
  const attempts = detail.data?.attempts ?? [];

  if (turn.role === "user") {
    return (
      <article className="ml-auto max-w-[85%] rounded-[14px] bg-surface-3 px-4 py-2.5">
        <p className="whitespace-pre-wrap break-words text-body leading-relaxed text-fg">
          {turn.text}
        </p>
      </article>
    );
  }

  const isStreaming = turn.status === "streaming";
  const streamingReasoning = isStreaming && turn.reasoning !== "" && !turn.text && turn.toolCalls.length === 0;
  const reasoningOpen = reasoningToggled ?? streamingReasoning;
  const maxAttemptLatency = attempts.reduce((max, attempt) => Math.max(max, attempt.latency_ms), 0);
  const effectiveStatus = turn.httpStatus ?? detail.data?.request?.status ?? null;
  const failedTurn = turn.status === "error" || (effectiveStatus !== null && effectiveStatus >= 400);

  return (
    <article className={cx("flex flex-col gap-2", isStreaming && "transition-none")}>
      <div className="flex flex-wrap items-center gap-2">
        <span aria-hidden className="h-2 w-2 rounded-full bg-[var(--color-accent)]" />
        <span className="font-mono text-caption text-muted">{turn.model ?? "assistant"}</span>

        {attempts.length > 0 ? (
          <button
            type="button"
            onClick={() => setWaterfallOpen((open) => !open)}
            aria-expanded={waterfallOpen}
            title="Toggle attempt waterfall"
            className={cx(
              "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 font-mono text-caption leading-4 tnum transition-colors",
              failedTurn
                ? "border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] text-[var(--color-neg-ink)]"
                : "border-[var(--color-pos-line)] bg-[var(--color-pos-tint)] text-[var(--color-pos-ink)]",
            )}
          >
            <span>
              {effectiveStatus !== null ? `${effectiveStatus} ${statusText(effectiveStatus)}` : STATUS_LABEL[turn.status]}
            </span>
            <span aria-hidden>·</span>
            <span>
              {attempts.length} {attempts.length === 1 ? "attempt" : "attempts"}
            </span>
            {waterfallOpen ? <CaretDown size={11} aria-hidden /> : <CaretRight size={11} aria-hidden />}
          </button>
        ) : effectiveStatus !== null && !isStreaming && !(turn.status === "error" && effectiveStatus < 400) ? (
          <StatusPill status={effectiveStatus} title={turn.error ?? undefined} />
        ) : (
          <Badge tone={turn.status === "error" ? "neg" : isStreaming ? "accent" : "neutral"}>
            <span className="font-mono tnum">{STATUS_LABEL[turn.status]}</span>
          </Badge>
        )}

        {turn.status === "stopped" ? (
          <span className="text-caption text-faint">stopped</span>
        ) : null}

        {turn.latency !== null || turn.ttft !== null ? (
          <span className="ml-auto font-mono text-caption text-faint tnum">
            {turn.ttft !== null ? `first token ${ms(turn.ttft)}` : ""}
            {turn.ttft !== null && turn.latency !== null ? " · " : ""}
            {turn.latency !== null ? ms(turn.latency) : ""}
            {turn.promptTokens !== null || turn.completionTokens !== null
              ? ` · ${turn.promptTokens ?? "—"} in / ${turn.completionTokens ?? "—"} out`
              : ""}
          </span>
        ) : null}
      </div>

      {waterfallOpen && attempts.length > 0 ? (
        <ol
          aria-label="attempt waterfall"
          className="divide-y divide-[var(--color-rule)] rounded-[var(--radius-surface)] border border-line bg-surface-2"
        >
          {attempts.map((attempt, index) => {
            const attemptFailed =
              attempt.status === 0 || attempt.status >= 400 || attempt.error !== null;
            const { provider, id } = splitRef(attempt.model);
            const served = !attemptFailed && index === attempts.length - 1;
            const widthPct =
              maxAttemptLatency > 0
                ? Math.max(4, Math.min(100, (attempt.latency_ms / maxAttemptLatency) * 100))
                : 0;
            return (
              <li key={attempt.attempt} className="space-y-1 px-3 py-2 text-caption">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-mono text-faint tnum">#{attempt.attempt}</span>
                  <span className="min-w-0 flex-1 truncate font-mono text-fg">
                    <span className="text-faint">{provider ?? attempt.provider}/</span>
                    {id}
                  </span>
                  <Badge tone={attemptFailed ? "neg" : "pos"}>
                    <span className="font-mono tnum">{attempt.status || "err"}</span>
                  </Badge>
                  <span className="font-mono text-muted tnum">{ms(attempt.latency_ms)}</span>
                  {served ? <span className="text-faint">served</span> : null}
                </div>
                {maxAttemptLatency > 0 ? (
                  <div aria-hidden className="h-1 overflow-hidden rounded-full bg-[var(--color-rule)]">
                    <div
                      className={cx(
                        "h-full rounded-full",
                        attemptFailed ? "bg-[var(--color-neg-fill)]" : "bg-[var(--color-pos-fill)]",
                      )}
                      style={{ width: `${widthPct}%` }}
                    />
                  </div>
                ) : null}
                {attemptFailed && attempt.error ? (
                  <p className="break-words text-neg">{attempt.error}</p>
                ) : null}
              </li>
            );
          })}
        </ol>
      ) : null}

      {turn.reasoning ? (
        <section
          aria-label="reasoning"
          className="rounded-[var(--radius-surface)] border border-line bg-surface-2"
        >
          <button
            type="button"
            onClick={() => setReasoningToggled(!reasoningOpen)}
            aria-expanded={reasoningOpen}
            className="flex w-full items-center gap-2 px-3 py-2 text-left text-caption font-medium text-muted hover:text-fg"
          >
            {reasoningOpen ? <CaretDown size={12} aria-hidden /> : <CaretRight size={12} aria-hidden />}
            <span>Reasoning</span>
            <span className="ml-auto font-mono text-faint tnum">
              {streamingReasoning ? "thinking…" : `${turn.reasoning.length} chars`}
            </span>
          </button>
          {reasoningOpen ? (
            <p className="border-t border-[var(--color-rule)] px-3 py-2.5 whitespace-pre-wrap break-words font-mono text-small leading-relaxed text-muted">
              {turn.reasoning}
              {streamingReasoning ? <StreamingCaret /> : null}
            </p>
          ) : null}
        </section>
      ) : null}

      {turn.toolCalls.length > 0 ? (
        <section aria-label="tool calls" className="flex flex-col gap-2">
          {turn.toolCalls.map((call, index) => {
            const streamingThisCall =
              isStreaming && !turn.text && index === turn.toolCalls.length - 1;
            return (
              <div
                key={call.id || `${call.name}-${index}`}
                className="rounded-[var(--radius-surface)] border border-line bg-surface-2 px-3 py-2"
              >
                <div className="flex flex-wrap items-center gap-2">
                  <Wrench size={13} aria-hidden className="text-muted" />
                  <span className="font-mono text-small font-medium text-fg">
                    {call.name || "tool_call"}
                  </span>
                  {call.id ? (
                    <span className="ml-auto font-mono text-caption text-faint tnum">{call.id}</span>
                  ) : null}
                </div>
                {call.arguments ? (
                  <pre className="mt-1.5 max-h-48 overflow-auto whitespace-pre-wrap break-words font-mono text-caption leading-relaxed text-muted">
                    {formatToolArguments(call.arguments)}
                    {streamingThisCall ? <StreamingCaret /> : null}
                  </pre>
                ) : null}
              </div>
            );
          })}
        </section>
      ) : null}

      <div
        aria-live={isStreaming ? "polite" : undefined}
        aria-busy={isStreaming ? true : undefined}
        className={cx("text-lead leading-7 text-fg", isStreaming && "transition-none")}
      >
        {turn.text ? (
          <p className="whitespace-pre-wrap break-words">
            {turn.text}
            {isStreaming ? <StreamingCaret /> : null}
          </p>
        ) : turn.status !== "error" && !turn.reasoning && turn.toolCalls.length === 0 ? (
          <p className="text-body text-faint">
            {isStreaming
              ? "Waiting for the first token…"
              : turn.status === "stopped"
                ? "Stopped before any content arrived."
                : "The provider returned no text content."}
            {isStreaming ? <StreamingCaret /> : null}
          </p>
        ) : null}
        {turn.error ? (
          <div className="mt-2">
            <ErrorNote error={turn.error} />
          </div>
        ) : null}
      </div>

      {!isStreaming ? (
        <div className="flex flex-wrap items-center gap-3">
          <button
            type="button"
            onClick={() => setRawOpen((open) => !open)}
            className="text-caption text-faint underline-offset-2 hover:text-fg hover:underline"
          >
            {rawOpen ? "hide raw frames" : "raw frames"}
          </button>
          {turn.requestId ? (
            <span className="font-mono text-caption text-faint tnum" title={turn.requestId}>
              req {turn.requestId.slice(0, 8)}
            </span>
          ) : null}
        </div>
      ) : null}

      {!isStreaming && rawOpen ? (
        <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all rounded-[var(--radius-surface)] border border-line bg-surface-2 p-3 font-mono text-caption leading-relaxed text-muted">
          {turn.raw || "No frames were received."}
        </pre>
      ) : null}
    </article>
  );
}

function StreamingCaret() {
  return (
    <span
      aria-hidden
      className="ml-0.5 inline-block h-4 w-0.5 translate-y-0.5 bg-accent motion-safe:animate-pulse"
    />
  );
}

function formatToolArguments(raw: string): string {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}

/** The calm first screen: say what this place does, then offer real starting prompts. */
export function EmptyPlayground({
  model,
  onPick,
}: {
  model: string;
  onPick: (prompt: string) => void;
}) {
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
            className="rounded-[10px] border border-line bg-surface px-3.5 py-2.5 text-left text-small leading-relaxed text-ink-soft transition-colors hover:border-[var(--color-line-strong)] hover:bg-surface-2"
          >
            {prompt}
          </button>
        ))}
      </div>
      <p className="text-caption text-faint">
        <kbd className={KBD_CLASS}>⌘↵</kbd> send · <kbd className={KBD_CLASS}>Esc</kbd> stop a stream
      </p>
    </div>
  );
}
