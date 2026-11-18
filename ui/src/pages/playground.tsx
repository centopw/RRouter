import { useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { Button, ErrorNote } from "../components/ui";
import { Heading } from "../components/atoms";
import { findModelRef } from "../lib/model-ref";
import { useModels, usePlaygroundKey, useSystem, useTemplates } from "../lib/queries";
import {
  Composer,
  ENDPOINTS,
  buildWireRequest,
  describeTemperature,
  parseTemperature,
  type Dialect,
} from "./playground/composer";
import {
  EmptyPlayground,
  TurnCard,
  chatDelta,
  createTurn,
  httpErrorMessage,
  mergeToolCalls,
  messagesDelta,
  parseSseFrame,
  type ToolCallDelta,
  type Turn,
} from "./playground/turn-card";

/**
 * A live conversation against the gateway, so an operator can prove a model answers
 * before pointing a client at it. One request per send: the same `/v1` endpoints a
 * client uses, the same auth header, the same SSE frames. Nothing is simulated.
 */

const NO_LEVELS: string[] = [];

export default function Playground() {
  const models = useModels();
  const system = useSystem();
  const templates = useTemplates();
  const key = usePlaygroundKey();

  const [searchParams, setSearchParams] = useSearchParams();
  const modelParam = searchParams.get("model");
  const templateParam = searchParams.get("template");

  const [dialect, setDialect] = useState<Dialect>("chat");
  const [model, setModel] = useState(() => searchParams.get("model")?.trim() ?? "");
  const [systemPrompt, setSystemPrompt] = useState("");
  const [activeTemplateId, setActiveTemplateId] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [temp, setTemp] = useState("1");
  const [effort, setEffort] = useState("");
  const [draft, setDraft] = useState("");
  const [turns, setTurns] = useState<Turn[]>([]);
  const [scrolledUp, setScrolledUp] = useState(false);

  const nextId = useRef(0);
  const abortRef = useRef<AbortController | null>(null);
  const streamRafRef = useRef<number | null>(null);
  const transcriptRef = useRef<HTMLDivElement | null>(null);
  const pinnedToBottomRef = useRef(true);
  const effortFor = useRef<string | null>(null);

  const apiKey = key.data?.key ?? "";
  const streaming = turns.some((turn) => turn.status === "streaming");
  const needsKey = system.data?.require_api_key === true;
  const keyMissing = needsKey && apiKey.trim() === "";

  // Deep-linked `?model=<ref>` from Models, Combos, Aliases, or Logs pre-selects the target.
  useEffect(() => {
    if (!modelParam || models.isLoading) return;
    const trimmed = modelParam.trim();
    if (trimmed) {
      const match = findModelRef(models.data, trimmed);
      setModel(match ? match.ref : trimmed);
    }
    const next = new URLSearchParams(searchParams);
    next.delete("model");
    setSearchParams(next, { replace: true });
  }, [modelParam, models.isLoading, models.data, searchParams, setSearchParams]);

  // First usable model is the useful default: the page should send on first open.
  useEffect(() => {
    if (model !== "" || modelParam || !models.data?.length) return;
    const usable = models.data.filter((m) => m.enabled && m.available);
    const pick = usable.find((m) => m.source === "upstream") ?? usable[0] ?? models.data[0];
    if (pick) setModel(pick.ref);
  }, [model, modelParam, models.data]);

  // "Open in Playground" on the Templates page hands the body over as a query param.
  useEffect(() => {
    if (!templateParam || !templates.data) return;
    const row = templates.data.find((template) => template.id === templateParam);
    if (row) {
      setSystemPrompt(row.body);
      setActiveTemplateId(row.id);
      setSettingsOpen(true);
    }
    const next = new URLSearchParams(searchParams);
    next.delete("template");
    setSearchParams(next, { replace: true });
  }, [templateParam, templates.data, searchParams, setSearchParams]);

  const selected = models.data?.find((entry) => entry.ref === model);
  const levels = selected?.reasoning?.supported_efforts ?? NO_LEVELS;
  const mandatory = selected?.reasoning?.mandatory ?? false;
  const tempInfo = describeTemperature(selected, dialect, effort !== "" && effort !== "none");

  useEffect(() => {
    const support = selected?.reasoning;
    if (!support || levels.length === 0) {
      effortFor.current = model;
      setEffort("");
      return;
    }
    const initial =
      support.default_enabled === false ? "none" : (support.default_effort ?? levels[0] ?? "");
    if (effortFor.current !== model) {
      effortFor.current = model;
      setEffort(initial);
      return;
    }
    setEffort((current) => {
      if (current === "none" && !support.mandatory) return current;
      if (current !== "" && levels.includes(current)) return current;
      return initial;
    });
  }, [levels, model, selected]);

  useEffect(() => {
    if (Number.parseFloat(temp) > tempInfo.max) setTemp(String(tempInfo.max));
  }, [temp, tempInfo.max]);

  // Escape stops a stream even when focus left the composer.
  useEffect(() => {
    if (!streaming) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") abortRef.current?.abort();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [streaming]);

  // A route change must not leave an SSE connection or scheduled RAF running in the background.
  useEffect(
    () => () => {
      abortRef.current?.abort();
      if (streamRafRef.current !== null) {
        window.cancelAnimationFrame(streamRafRef.current);
        streamRafRef.current = null;
      }
    },
    [],
  );

  // 48px scroll-anchor hysteresis: keep pinned during streaming unless the user scrolls up.
  useEffect(() => {
    const el = transcriptRef.current;
    if (!el || !pinnedToBottomRef.current) return;
    el.scrollTop = el.scrollHeight;
  }, [turns]);

  const handleTranscriptScroll = () => {
    const el = transcriptRef.current;
    if (!el) return;
    const isNearBottom = el.scrollHeight - el.scrollTop - el.clientHeight <= 48;
    pinnedToBottomRef.current = isNearBottom;
    setScrolledUp(!isNearBottom);
  };

  const jumpToBottom = () => {
    const el = transcriptRef.current;
    if (!el) return;
    pinnedToBottomRef.current = true;
    setScrolledUp(false);
    el.scrollTop = el.scrollHeight;
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
    const { wire, headers } = buildWireRequest({
      dialect,
      model,
      text,
      turns,
      systemText: systemPrompt.trim(),
      temperature: tempInfo.supported ? parseTemperature(temp, tempInfo.max) : undefined,
      effort,
      authKey: apiKey.trim(),
    });

    pinnedToBottomRef.current = true;
    setScrolledUp(false);
    setTurns((prev) => [
      ...prev,
      createTurn({ id: userId, role: "user", model: null, text, status: "done" }),
      createTurn({ id: assistantId, role: "assistant", model, text: "", status: "streaming" }),
    ]);
    setDraft("");

    let ttft: number | null = null;
    let raw = "";
    let pendingText = "";
    let pendingReasoning = "";
    let pendingTools: ToolCallDelta[] = [];
    let promptTokens: number | null = null;
    let completionTokens: number | null = null;
    let requestId: string | null = null;
    let httpStatus: number | null = null;
    let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
    const toolIndexByBlock = new Map<number, number>();

    const cancelScheduledFlush = () => {
      if (streamRafRef.current !== null) {
        window.cancelAnimationFrame(streamRafRef.current);
        streamRafRef.current = null;
      }
    };

    const applyBuffered = (patch?: Partial<Turn>) => {
      const chunk = pendingText;
      const reasoningChunk = pendingReasoning;
      const toolDeltas = pendingTools;
      pendingText = "";
      pendingReasoning = "";
      pendingTools = [];
      if (!patch && !chunk && !reasoningChunk && toolDeltas.length === 0) return;
      const firstTokenAt = ttft;
      setTurns((prev) =>
        prev.map((turn) =>
          turn.id === assistantId
            ? {
                ...turn,
                ...patch,
                text: chunk ? turn.text + chunk : turn.text,
                reasoning: reasoningChunk ? turn.reasoning + reasoningChunk : turn.reasoning,
                toolCalls: mergeToolCalls(turn.toolCalls, toolDeltas),
                ttft: firstTokenAt ?? turn.ttft,
                requestId: requestId ?? turn.requestId,
                httpStatus: httpStatus ?? turn.httpStatus,
              }
            : turn,
        ),
      );
    };

    const flushPending = () => {
      streamRafRef.current = null;
      applyBuffered();
    };

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
        return null;
      }
      const delta =
        dialect === "chat" ? chatDelta(json) : messagesDelta(json, parsed.event, toolIndexByBlock);
      if (!delta) return null;
      if (delta.error) throw new Error(delta.error);
      if (delta.text || delta.reasoning || (delta.toolCalls && delta.toolCalls.length > 0)) {
        if (ttft === null) ttft = Math.round(performance.now() - startedAt);
        if (delta.text) pendingText += delta.text;
        if (delta.reasoning) pendingReasoning += delta.reasoning;
        if (delta.toolCalls) pendingTools.push(...delta.toolCalls);
        if (streamRafRef.current === null) {
          streamRafRef.current = window.requestAnimationFrame(flushPending);
        }
      }
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
        requestId = response.headers.get("x-request-id");
        httpStatus = response.status;
        if (!response.ok) throw new Error(httpErrorMessage(await response.text(), response.status));
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
        if (!finished && buffer.trim() !== "") applyFrame(buffer);
        cancelScheduledFlush();
        applyBuffered({
          status: "done",
          latency: Math.round(performance.now() - startedAt),
          raw,
          promptTokens,
          completionTokens,
        });
      } catch (error) {
        cancelScheduledFlush();
        const aborted = controller.signal.aborted;
        applyBuffered({
          status: aborted ? "stopped" : "error",
          error: aborted ? null : error instanceof Error ? error.message : String(error),
          latency: Math.round(performance.now() - startedAt),
          raw,
          promptTokens,
          completionTokens,
        });
      } finally {
        cancelScheduledFlush();
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
  const modelLabel = models.data?.find((m) => m.ref === model)?.display ?? model;

  return (
    <div className="mx-auto flex h-[calc(100dvh-10rem)] min-h-[420px] w-full max-w-[880px] flex-col gap-4 pb-2 min-[820px]:h-[calc(100dvh-6rem)]">
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
          {turns.length > 0 && !streaming ? (
            <Button
              size="sm"
              variant="quiet"
              onClick={() => {
                pinnedToBottomRef.current = true;
                setScrolledUp(false);
                setTurns([]);
              }}
            >
              Clear
            </Button>
          ) : null}
        </div>
      </header>

      {models.isError ? <ErrorNote error={models.error} onRetry={() => void models.refetch()} /> : null}
      {key.isError ? (
        <ErrorNote error={key.error} onRetry={() => void key.refetch()} />
      ) : keyMissing && !key.isLoading ? (
        <div className="rounded-[var(--radius-card)] border border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] px-3 py-2 text-small text-[var(--color-warn-ink)]">
          This gateway requires a key and the playground's own key was not issued.
        </div>
      ) : null}

      <div className="relative flex min-h-[260px] flex-1 flex-col overflow-hidden rounded-[var(--radius-overlay)] border border-line bg-[var(--color-bg)]">
        <div
          ref={transcriptRef}
          onScroll={handleTranscriptScroll}
          className="flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto overscroll-contain px-4 py-5 min-[700px]:px-6"
        >
          {turns.length === 0 ? (
            <EmptyPlayground model={modelLabel} onPick={setDraft} />
          ) : (
            turns.map((turn) => <TurnCard key={turn.id} turn={turn} />)
          )}
        </div>
        {scrolledUp && turns.length > 0 ? (
          <div className="pointer-events-none absolute inset-x-0 bottom-3 z-10 flex justify-center">
            <button
              type="button"
              onClick={jumpToBottom}
              className="pointer-events-auto inline-flex items-center gap-1.5 rounded-full border border-[var(--color-line-strong)] bg-surface px-3 py-1.5 text-small font-medium text-fg shadow-[var(--shadow-popover)] transition-opacity hover:bg-surface-2"
            >
              <span aria-hidden>↓</span>
              Jump to bottom
            </button>
          </div>
        ) : null}
      </div>

      <Composer
        draft={draft}
        onDraftChange={setDraft}
        model={model}
        onModelChange={setModel}
        modelLabel={modelLabel}
        modelHint={modelHint}
        effort={effort}
        onEffortChange={setEffort}
        levels={levels}
        mandatory={mandatory}
        dialect={dialect}
        onDialectChange={setDialect}
        temp={temp}
        onTempChange={setTemp}
        temperatureMax={tempInfo.max}
        temperatureSupported={tempInfo.supported}
        temperatureHint={tempInfo.hint}
        systemPrompt={systemPrompt}
        onSystemPromptChange={setSystemPrompt}
        activeTemplateId={activeTemplateId}
        onActiveTemplateIdChange={setActiveTemplateId}
        apiKey={apiKey}
        keyError={key.isError}
        needsKey={needsKey}
        streaming={streaming}
        settingsOpen={settingsOpen}
        onSettingsOpenChange={setSettingsOpen}
        onSend={send}
        onAbortStream={() => abortRef.current?.abort()}
      />
    </div>
  );
}
