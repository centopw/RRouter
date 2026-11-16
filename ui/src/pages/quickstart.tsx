import { useRef, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { ArrowRight } from "@phosphor-icons/react";
import { useQueryClient } from "@tanstack/react-query";
import {
  Badge,
  Button,
  Card,
  CopyButton,
  CopyChip,
  Input,
  PageHeader,
  cx,
} from "../components/ui";
import { ConnectionCard } from "../components/connection-card";
import { CreateKeyModal } from "../components/create-key-modal";
import { qk, useIsAdmin, useModels } from "../lib/queries";

type Protocol = "openai" | "anthropic";
type OS = "macos" | "linux" | "windows";
type ToolId =
  | "omp"
  | "claude-code"
  | "codex"
  | "opencode"
  | "cursor"
  | "cline"
  | "openai-sdk"
  | "anthropic-sdk"
  | "generic";

type Ctx = { origin: string; openai: string; key: string; model: string };

type RecipeContent = {
  file?: Record<OS, string>;
  /** Shown when the tool has no file to edit. */
  steps: string[];
  code: Partial<Record<Protocol, string>>;
  /** A shell/powershell step when the config references an environment variable. */
  env?: Record<OS, string>;
  lang: string;
  note?: string;
};

type Recipe = {
  id: ToolId;
  name: string;
  summary: string;
  protocols: Protocol[];
  build: (ctx: Ctx) => RecipeContent;
};

const OSES: OS[] = ["macos", "linux", "windows"];
const OS_LABELS: Record<OS, string> = {
  macos: "macOS",
  linux: "Linux",
  windows: "Windows",
};
const PROTOCOL_LABELS: Record<Protocol, string> = {
  openai: "OpenAI-compatible",
  anthropic: "Anthropic Messages",
};

const lines = (...v: string[]) => v.join("\n");

/** The path a tool reads on each desktop OS. `undefined` means it is edited in the app. */
const OMP_FILE: Record<OS, string> = {
  macos: "~/.omp/agent/models.yml",
  linux: "~/.omp/agent/models.yml",
  windows: "%USERPROFILE%\\.omp\\agent\\models.yml",
};
const CLAUDE_FILE: Record<OS, string> = {
  macos: "~/.claude/settings.json",
  linux: "~/.claude/settings.json",
  windows: "%USERPROFILE%\\.claude\\settings.json",
};
const CODEX_FILE: Record<OS, string> = {
  macos: "~/.codex/config.toml",
  linux: "~/.codex/config.toml",
  windows: "%USERPROFILE%\\.codex\\config.toml",
};
const OPENCODE_FILE: Record<OS, string> = {
  macos: "~/.config/opencode/opencode.json",
  linux: "~/.config/opencode/opencode.json",
  windows: "%USERPROFILE%\\.config\\opencode\\opencode.json",
};

const RECIPES: Recipe[] = [
  {
    id: "omp",
    name: "OMP",
    summary: "Coding agent, provider file",
    protocols: ["openai", "anthropic"],
    build: ({ origin, openai, key, model }) => ({
      file: OMP_FILE,
      lang: "models.yml",
      code: {
        openai: lines(
          "providers:",
          "  rrouter:",
          `    baseUrl: ${openai}`,
          `    apiKey: ${key}`,
          "    api: openai-completions",
          "    authHeader: true",
          "    disableStrictTools: true",
          "    discovery:",
          "      type: proxy",
        ),
        anthropic: lines(
          "providers:",
          "  rrouter-anthropic:",
          `    baseUrl: ${origin}`,
          `    apiKey: ${key}`,
          "    api: anthropic-messages",
          "    authHeader: true",
          "    disableStrictTools: true",
          "    discovery:",
          "      type: proxy",
        ),
      },
      steps: [
        "Open the file. If the agent directory does not exist, run `omp` once to create it, then edit `models.yml`.",
        "Merge the provider under the existing `providers:` key; keep your other providers.",
        "Start a new omp session. The OpenAI entry serves `rrouter/<model>`, the Anthropic entry serves `rrouter-anthropic/<model>`.",
      ],
      note: `Use \"${model}\" as the model id. The two entries are separate on purpose: OpenAI Chat and Anthropic Messages are different wire formats.`,
    }),
  },
  {
    id: "claude-code",
    name: "Claude Code",
    summary: "Anthropic Messages CLI",
    protocols: ["anthropic"],
    build: ({ origin, key, model }) => ({
      file: CLAUDE_FILE,
      lang: "settings.json",
      code: {
        anthropic: JSON.stringify(
          {
            $schema: "https://json.schemastore.org/claude-code-settings.json",
            env: {
              ANTHROPIC_BASE_URL: origin,
              ANTHROPIC_AUTH_TOKEN: key,
              ANTHROPIC_MODEL: model,
            },
          },
          null,
          2,
        ),
      },
      steps: [
        "Open the user settings file. Create it if it does not exist.",
        "Merge the `env` block with any existing settings; do not replace your permissions or hooks.",
        "Start `claude`, then run `/status` and confirm the gateway endpoint is active.",
      ],
      note: "Claude Code appends `/v1/messages`; the base URL is the origin, without `/v1`.",
    }),
  },
  {
    id: "codex",
    name: "Codex CLI",
    summary: "OpenAI Responses CLI",
    protocols: ["openai"],
    build: ({ openai, key, model }) => ({
      file: CODEX_FILE,
      lang: "config.toml",
      code: {
        openai: lines(
          `model = "${model}"`,
          'model_provider = "rrouter"',
          "",
          "[model_providers.rrouter]",
          'name = "RRouter"',
          `base_url = "${openai}"`,
          'env_key = "RROUTER_API_KEY"',
          'wire_api = "responses"',
        ),
      },
      env: {
        macos: `export RROUTER_API_KEY=${key}`,
        linux: `export RROUTER_API_KEY=${key}`,
        windows: `$env:RROUTER_API_KEY = "${key}"`,
      },
      steps: [
        "Open the Codex config file. Create it if it does not exist.",
        "Merge the provider table and the top-level `model`/`model_provider` keys.",
        "Set `RROUTER_API_KEY` in the same shell, then start `codex`.",
      ],
      note: "`base_url` is the `/v1` root; Codex appends `/responses`.",
    }),
  },
  {
    id: "opencode",
    name: "OpenCode",
    summary: "Editor agent, JSON config",
    protocols: ["openai", "anthropic"],
    build: ({ origin, openai, key, model }) => ({
      file: OPENCODE_FILE,
      lang: "opencode.json",
      code: {
        openai: JSON.stringify(
          {
            $schema: "https://opencode.ai/config.json",
            model: "rrouter/rrouter-model",
            providers: {
              rrouter: {
                name: "RRouter",
                env: ["RROUTER_API_KEY"],
                package: "@opencode/ai/providers/openai-compatible",
                settings: { baseURL: openai },
                models: {
                  "rrouter-model": { name: "RRouter model", modelID: model },
                },
              },
            },
          },
          null,
          2,
        ),
        anthropic: JSON.stringify(
          {
            $schema: "https://opencode.ai/config.json",
            model: "rrouter/rrouter-model",
            providers: {
              rrouter: {
                name: "RRouter",
                env: ["RROUTER_API_KEY"],
                package: "@opencode/ai/providers/anthropic-compatible",
                settings: { baseURL: origin },
                models: {
                  "rrouter-model": { name: "RRouter model", modelID: model },
                },
              },
            },
          },
          null,
          2,
        ),
      },
      env: {
        macos: `export RROUTER_API_KEY=${key}`,
        linux: `export RROUTER_API_KEY=${key}`,
        windows: `$env:RROUTER_API_KEY = "${key}"`,
      },
      steps: [
        "Open the global config; edit the project config instead if you only want this repository.",
        "Merge `providers` and `model` with the existing config.",
        "Set `RROUTER_API_KEY`, restart OpenCode, then choose `rrouter/rrouter-model` in `/models`.",
      ],
      note: "The OpenAI package needs the `/v1` root; the Anthropic package needs the origin.",
    }),
  },
  {
    id: "cursor",
    name: "Cursor",
    summary: "OpenAI-compatible settings",
    protocols: ["openai"],
    build: ({ openai, key, model }) => ({
      lang: "values",
      code: {
        openai: lines(
          `Base URL: ${openai}`,
          `API key: ${key}`,
          `Model: ${model}`,
        ),
      },
      steps: [
        "Open Cursor Settings → Models.",
        "Enable the OpenAI API key, then set Override OpenAI Base URL to the value below.",
        "Paste the gateway key. Add the model id if Cursor does not discover it automatically.",
      ],
      note: "Cursor handles the OpenAI wire format only; use the `/v1` base URL.",
    }),
  },
  {
    id: "cline",
    name: "Cline",
    summary: "OpenAI-compatible settings",
    protocols: ["openai"],
    build: ({ openai, key, model }) => ({
      lang: "values",
      code: {
        openai: lines(
          `Base URL: ${openai}`,
          `API key: ${key}`,
          `Model ID: ${model}`,
        ),
      },
      steps: [
        "Open Cline → Settings → API Configuration.",
        "Choose OpenAI Compatible.",
        "Paste the base URL, key, and model id below.",
      ],
      note: "Cline handles the OpenAI wire format only; use the `/v1` base URL.",
    }),
  },
  {
    id: "openai-sdk",
    name: "OpenAI SDK",
    summary: "Python or Node script",
    protocols: ["openai"],
    build: ({ openai, key, model }) => ({
      file: { macos: "main.py", linux: "main.py", windows: "main.py" },
      lang: "python",
      code: {
        openai: lines(
          "from openai import OpenAI",
          "",
          "client = OpenAI(",
          `    base_url=\"${openai}\",`,
          `    api_key=\"${key}\",`,
          ")",
          "",
          "reply = client.chat.completions.create(",
          `    model=\"${model}\",`,
          "    messages=[{\"role\": \"user\", \"content\": \"Hello\"}],",
          ")",
          "print(reply.choices[0].message.content)",
        ),
      },
      steps: [
        "Create the script in your project and install the SDK (`pip install openai`).",
        "Run it. The key is the gateway key, not an upstream provider key.",
      ],
      note: "The OpenAI SDK appends `/chat/completions`; `base_url` includes `/v1`.",
    }),
  },
  {
    id: "anthropic-sdk",
    name: "Anthropic SDK",
    summary: "Python or Node script",
    protocols: ["anthropic"],
    build: ({ origin, key, model }) => ({
      file: { macos: "main.py", linux: "main.py", windows: "main.py" },
      lang: "python",
      code: {
        anthropic: lines(
          "from anthropic import Anthropic",
          "",
          "client = Anthropic(",
          `    base_url=\"${origin}\",`,
          `    api_key=\"${key}\",`,
          ")",
          "",
          "message = client.messages.create(",
          `    model=\"${model}\",`,
          "    max_tokens=1024,",
          "    messages=[{\"role\": \"user\", \"content\": \"Hello\"}],",
          ")",
          "print(message.content[0].text)",
        ),
      },
      steps: [
        "Create the script in your project and install the SDK (`pip install anthropic`).",
        "Run it. The key is the gateway key; no Anthropic account is required in the client.",
      ],
      note: "The Anthropic SDK appends `/v1/messages`; `base_url` is the origin.",
    }),
  },
  {
    id: "generic",
    name: "Any other tool",
    summary: "Environment variables",
    protocols: ["openai", "anthropic"],
    build: ({ origin, openai, key, model }) => ({
      lang: "environment",
      code: {
        openai: lines(
          `OPENAI_BASE_URL=${openai}`,
          `OPENAI_API_KEY=${key}`,
          `OPENAI_MODEL=${model}`,
        ),
        anthropic: lines(
          `ANTHROPIC_BASE_URL=${origin}`,
          `ANTHROPIC_AUTH_TOKEN=${key}`,
          `ANTHROPIC_MODEL=${model}`,
        ),
      },
      steps: [
        "Put the values in the tool's environment, `.env`, or provider settings.",
        "Restart the tool and check `GET /v1/models` with the same key.",
      ],
      note: "OpenAI-compatible tools use the `/v1` base URL; Anthropic tools use the origin.",
    }),
  },
];

function defaultOs(): OS {
  if (typeof navigator === "undefined") return "macos";
  if (/windows/i.test(navigator.userAgent)) return "windows";
  if (/mac/i.test(navigator.userAgent)) return "macos";
  return "linux";
}

/** Preserve inline code in guide steps instead of showing literal backticks. */
function InlineCode({ text }: { text: string }) {
  return (
    <>
      {text.split(/(`[^`]+`)/g).map((part, index) =>
        part.startsWith("`") && part.endsWith("`") ? (
          <code
            key={index}
            className="rounded bg-surface-3 px-1 py-0.5 font-mono text-[0.95em] text-fg"
          >
            {part.slice(1, -1)}
          </code>
        ) : (
          <span key={index}>{part}</span>
        ),
      )}
    </>
  );
}

function Sample({ code, label }: { code: string; label: string }) {
  return (
    <div className="rounded-lg border border-line bg-[var(--color-surface-2)]">
      <pre className="overflow-x-auto px-3 py-2.5 text-caption leading-relaxed text-muted">
        <code>{code}</code>
      </pre>
      <div className="flex items-center justify-between border-t border-line px-3 py-1.5">
        <span className="text-caption text-faint">{label}</span>
        <CopyButton value={code} label="Copy" className="h-7 px-2 text-caption" />
      </div>
    </div>
  );
}

function Segmented<T extends string>({
  options,
  value,
  onChange,
  label,
}: {
  options: ReadonlyArray<{ id: T; label: string }>;
  value: T;
  onChange: (value: T) => void;
  label: string;
}) {
  return (
    <div
      role="group"
      aria-label={label}
      className="inline-flex flex-wrap gap-0.5 rounded-full border border-line bg-surface-3 p-0.5"
    >
      {options.map((option) => (
        <button
          key={option.id}
          type="button"
          aria-pressed={value === option.id}
          onClick={() => onChange(option.id)}
          className={cx(
            "h-7 rounded-full px-2.5 text-small transition-colors max-[820px]:h-9",
            value === option.id
              ? "bg-accent font-medium text-[var(--color-accent-ink)]"
              : "text-muted hover:text-fg",
          )}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

function Step({
  number,
  title,
  body,
  action,
  onClick,
}: {
  number: number;
  title: string;
  body: string;
  action: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="group flex h-full flex-col rounded-[var(--radius-card)] border border-line bg-surface p-4 text-left transition-colors hover:border-line-strong hover:bg-[var(--color-selected)]"
    >
      <span className="flex h-7 w-7 items-center justify-center rounded-full border border-line-strong bg-surface-2 text-caption font-semibold text-fg tnum">
        {number}
      </span>
      <span className="mt-3 text-body font-semibold text-fg">{title}</span>
      <span className="mt-1 flex-1 text-small leading-relaxed text-muted">{body}</span>
      <span className="mt-3 inline-flex items-center gap-1 text-small font-medium text-[var(--color-accent-text)]">
        {action} <ArrowRight size={13} aria-hidden className="transition-transform group-hover:translate-x-0.5" />
      </span>
    </button>
  );
}

export default function QuickStart() {
  const isAdmin = useIsAdmin();
  const navigate = useNavigate();
  const client = useQueryClient();
  const models = useModels();
  const [params, setParams] = useSearchParams();
  const detailRef = useRef<HTMLDivElement | null>(null);
  const keyRef = useRef<HTMLDivElement | null>(null);
  const [initialOs] = useState(defaultOs);
  const [apiKey, setApiKey] = useState("");
  const [minted, setMinted] = useState<string | null>(null);
  const [creatingKey, setCreatingKey] = useState(false);
  const origin = window.location.origin;
  const openai = `${origin}/v1`;
  const key = apiKey.trim() || "sk-rr-...";

  const requestedTool = params.get("tool");
  const guide = RECIPES.find((recipe) => recipe.id === requestedTool) ?? RECIPES[0]!;
  const requestedProtocol = params.get("api") as Protocol | null;
  const protocol = guide.protocols.includes(requestedProtocol as Protocol)
    ? (requestedProtocol as Protocol)
    : guide.protocols[0]!;
  const requestedOs = params.get("os") as OS | null;
  const os = requestedOs && OSES.includes(requestedOs) ? requestedOs : initialOs;

  const preferred =
    protocol === "anthropic"
      ? (ref: string) => ref.toLowerCase().includes("claude")
      : (ref: string) => /^openai\//i.test(ref) && /gpt|codex/i.test(ref);
  const example =
    models.data?.find(
      (model) => model.enabled && model.available && preferred(model.ref),
    ) ?? models.data?.find((model) => model.enabled && model.available);
  const modelRef = example?.ref ?? "provider/model";
  const content = guide.build({ origin, openai, key, model: modelRef });
  const code = content.code[protocol] ?? "";
  const env = content.env?.[os];
  const path = content.file?.[os];
  const showOs = content.file ? new Set(Object.values(content.file)).size > 1 : false;

  const update = (patch: Record<string, string | null>) =>
    setParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        for (const [name, value] of Object.entries(patch)) {
          if (value === null) next.delete(name);
          else next.set(name, value);
        }
        return next;
      },
      { replace: true },
    );

  const selectTool = (next: Recipe) => {
    const api = next.protocols.includes(protocol) ? protocol : next.protocols[0]!;
    update({ tool: next.id, api });
    if (window.matchMedia("(max-width: 1023px)").matches) {
      requestAnimationFrame(() => {
        detailRef.current?.scrollIntoView({ behavior: "smooth", block: "start" });
      });
    }
  };

  return (
    <>
      <PageHeader
        title="Quick start"
        subtitle="Create a key, pick a model, then open the guide for the tool you actually use."
        actions={
          isAdmin ? (
            <Button
              variant="primary"
              onClick={() =>
                keyRef.current?.scrollIntoView({ behavior: "smooth", block: "center" })
              }
            >
              create API key
            </Button>
          ) : (
            <Badge>ask an administrator for a key</Badge>
          )
        }
      />

      <Card className="scroll-mt-20">
        <div ref={keyRef} className="px-5 py-4">
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-2">
                <h2 className="text-lead font-semibold">Your API key</h2>
                {minted !== null && minted === apiKey ? <Badge tone="pos">created</Badge> : null}
              </div>
              <p className="mt-0.5 text-small text-muted">
                Create one here, or paste an existing key. Every guide below fills it in.
              </p>
            </div>
            <div className="flex min-w-0 flex-1 flex-wrap items-center justify-end gap-2 sm:flex-none">
              <Input
                value={apiKey}
                onChange={(value) => {
                  setApiKey(value);
                  setMinted(null);
                }}
                placeholder="sk-rr-... (paste an existing key)"
                ariaLabel="API key"
                className="w-full font-mono sm:w-[21rem]"
              />
              {apiKey.trim() ? <CopyButton value={apiKey.trim()} label="Copy key" /> : null}
              {isAdmin ? (
                <Button variant="primary" onClick={() => setCreatingKey(true)}>
                  {apiKey.trim() ? "create another" : "create key"}
                </Button>
              ) : (
                <Badge>ask an administrator for a key</Badge>
              )}
            </div>
          </div>
          <p className="mt-2 text-caption leading-relaxed text-faint">
            {minted !== null && minted === apiKey
              ? "Copy it now: the plaintext is shown only on this page. Configure limits and models in the create dialog."
              : apiKey.trim()
                ? "This key is used in every config below. Make sure it has access to the model you chose."
                : "Until you create or paste a key, the configs show sk-rr-... as a placeholder."}
          </p>
        </div>
      </Card>

      {creatingKey ? (
        <CreateKeyModal
          onClose={() => setCreatingKey(false)}
          onCreated={(created) => {
            setApiKey(created.key);
            setMinted(created.key);
            setCreatingKey(false);
            void client.invalidateQueries({ queryKey: qk.keys });
            void client.invalidateQueries({ queryKey: qk.system });
          }}
        />
      ) : null}

      <div className="grid gap-3 sm:grid-cols-3">
        <Step
          number={1}
          title="Create an API key"
          body="The plaintext is shown once. Store it in the tool's secret field, not in a repository."
          action={isAdmin ? "Create below" : "Paste your key"}
          onClick={() =>
            keyRef.current?.scrollIntoView({ behavior: "smooth", block: "center" })
          }
        />
        <Step
          number={2}
          title="Choose a model"
          body="Use the provider-qualified id from the Models page, for example provider/model."
          action="Browse models"
          onClick={() => navigate("/models")}
        />
        <Step
          number={3}
          title="Configure a tool"
          body="Pick a platform below. Each guide names the file, the OS-specific path, and the wire format."
          action="See tool guides"
          onClick={() => document.getElementById("tool-guide")?.scrollIntoView({ behavior: "smooth" })}
        />
      </div>

      <ConnectionCard showQuickStartLink={false} />

      <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
        <Card className="overflow-hidden">
          <div className="border-b border-line px-5 py-3.5">
            <h2 className="text-lead font-semibold">Example model</h2>
            <p className="mt-0.5 text-small text-muted">
              Model IDs are provider-qualified. Copy the one you want to call.
            </p>
          </div>
          <div className="px-5 py-4">
            <CopyChip value={modelRef} label="example model" className="w-full" />
            <p className="mt-3 text-caption leading-relaxed text-faint">
              A bare model id may still route when it is unambiguous, but the qualified form
              is what the Models page and <code>/v1/models</code> advertise.
            </p>
          </div>
        </Card>

        <Card className="overflow-hidden">
          <div className="border-b border-line px-5 py-3.5">
            <h2 className="text-lead font-semibold">Supported endpoints</h2>
            <p className="mt-0.5 text-small text-muted">Same gateway, same key, three wire shapes.</p>
          </div>
          <div className="divide-y divide-[var(--color-rule)]">
            {[
              ["POST", "/v1/chat/completions", "OpenAI Chat Completions"],
              ["POST", "/v1/responses", "OpenAI Responses · Codex"],
              ["POST", "/v1/messages", "Anthropic Messages · Claude Code"],
              ["POST", "/v1/responses/compact", "OpenAI server compaction"],
              ["GET", "/v1/models", "Models available to this key"],
            ].map(([method, path, note]) => (
              <div key={path} className="flex items-baseline gap-3 px-5 py-2.5">
                <span
                  className={cx(
                    "w-9 shrink-0 font-mono text-caption",
                    method === "GET" ? "text-[var(--color-pos)]" : "text-[var(--color-accent-2)]",
                  )}
                >
                  {method}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-mono text-small text-fg">{path}</span>
                  <span className="block text-caption text-faint">{note}</span>
                </span>
              </div>
            ))}
          </div>
        </Card>
      </div>

      <section id="tool-guide" className="mt-6 scroll-mt-4">
        <div className="mb-3 flex flex-wrap items-baseline justify-between gap-2">
          <div>
            <h2 className="text-heading font-semibold text-fg">Connect a tool</h2>
            <p className="mt-0.5 text-small text-muted">
              Pick one platform. The guide opens with its file, OS-specific path, and both
              wire protocols where the tool supports them.
            </p>
          </div>
          <Link
            to="/providers"
            className="text-small font-medium text-[var(--color-accent-text)] hover:underline"
          >
            Manage provider accounts →
          </Link>
        </div>

        <div className="grid gap-3 lg:grid-cols-[230px_minmax(0,1fr)]">
          <div className="grid content-start gap-2 sm:grid-cols-2 lg:grid-cols-1">
            {RECIPES.map((recipe) => {
              const selected = recipe.id === guide.id;
              return (
                <button
                  key={recipe.id}
                  type="button"
                  aria-pressed={selected}
                  onClick={() => selectTool(recipe)}
                  className={cx(
                    "rounded-xl border p-3 text-left transition-colors",
                    selected
                      ? "border-[var(--color-accent)] bg-[var(--color-selected)]"
                      : "border-line bg-surface hover:border-line-strong hover:bg-[var(--color-selected)]",
                  )}
                >
                  <span className="flex items-center justify-between gap-2">
                    <span className="text-body font-semibold text-fg">{recipe.name}</span>
                    <span className="shrink-0 text-caption text-faint">
                      {recipe.protocols.length === 2
                        ? "2 formats"
                        : PROTOCOL_LABELS[recipe.protocols[0]!].split(" ")[0]}
                    </span>
                  </span>
                  <span className="mt-0.5 block text-caption text-muted">{recipe.summary}</span>
                </button>
              );
            })}
          </div>

          <div ref={detailRef} className="scroll-mt-4">
            <Card className="overflow-hidden">
              <div className="border-b border-line px-5 py-4">
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div>
                    <h3 className="text-lead font-semibold text-fg">{guide.name}</h3>
                    <p className="mt-0.5 text-small text-muted">{guide.summary}</p>
                  </div>
                  {guide.protocols.length > 1 ? (
                    <Segmented
                      label="Wire format"
                      value={protocol}
                      onChange={(next) => update({ api: next })}
                      options={guide.protocols.map((id) => ({ id, label: PROTOCOL_LABELS[id] }))}
                    />
                  ) : (
                    <Badge>{PROTOCOL_LABELS[guide.protocols[0]!]}</Badge>
                  )}
                </div>
              </div>

              <div className="space-y-5 px-5 py-4">
                {content.file ? (
                  <div>
                    <div className="flex flex-wrap items-center justify-between gap-2">
                      <span className="text-caption font-medium tracking-wide text-faint uppercase">
                        File to edit
                      </span>
                      {showOs ? (
                        <Segmented
                          label="Operating system"
                          value={os}
                          onChange={(next) => update({ os: next })}
                          options={OSES.map((id) => ({ id, label: OS_LABELS[id] }))}
                        />
                      ) : null}
                    </div>
                    <div className="mt-2 flex items-center gap-2 rounded-lg border border-line bg-[var(--color-surface-2)] px-3 py-2">
                      <code className="min-w-0 flex-1 break-all font-mono text-small text-fg">
                        {path}
                      </code>
                      <CopyButton value={path ?? ""} label="Copy path" className="h-7 px-2 text-caption" />
                    </div>
                  </div>
                ) : (
                  <div className="rounded-lg border border-line bg-[var(--color-surface-2)] px-3 py-2.5">
                    <div className="text-caption font-medium tracking-wide text-faint uppercase">
                      Where to configure
                    </div>
                    <p className="mt-1 text-small text-muted">
                      No file: this tool stores the endpoint in its own settings.
                    </p>
                  </div>
                )}

                <ol className="space-y-2">
                  {content.steps.map((step, index) => (
                    <li key={step} className="flex gap-2 text-small leading-relaxed text-muted">
                      <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-surface-3 text-caption font-semibold text-fg tnum">
                        {index + 1}
                      </span>
                      <span><InlineCode text={step} /></span>
                    </li>
                  ))}
                </ol>

                <div>
                  <div className="mb-2 flex items-center gap-2">
                    <span className="text-caption font-medium tracking-wide text-faint uppercase">
                      {content.file ? "Configuration" : "Values to paste"}
                    </span>
                    {content.env ? (
                      <span className="text-caption text-faint">· set the key separately</span>
                    ) : null}
                  </div>
                  <Sample code={code} label={content.lang} />
                </div>

                {env ? (
                  <div>
                    <div className="mb-2 text-caption font-medium tracking-wide text-faint uppercase">
                      Set the key
                    </div>
                    <Sample code={env} label={os === "windows" ? "powershell" : "shell"} />
                  </div>
                ) : null}

                {content.note ? (
                  <p className="text-caption leading-relaxed text-faint"><InlineCode text={content.note} /></p>
                ) : null}
              </div>
            </Card>
          </div>
        </div>
      </section>

      <Card className="mt-4 flex flex-wrap items-center justify-between gap-3 px-5 py-4">
        <div>
          <h2 className="text-body font-semibold text-fg">Server-side compaction</h2>
          <p className="mt-0.5 max-w-[70ch] text-small text-muted">
            OpenAI and Anthropic can compact the conversation on the provider. RRouter
            forwards the native compaction fields and beta headers; no local summary is
            written.
          </p>
        </div>
        <Badge tone="accent">server compact available</Badge>
      </Card>
    </>
  );
}
