import { Link, useNavigate } from "react-router-dom";
import { ArrowRight } from "@phosphor-icons/react";
import {
  Badge,
  Button,
  Card,
  CopyButton,
  CopyChip,
  PageHeader,
  cx,
} from "../components/ui";
import { ConnectionCard } from "../components/connection-card";
import { useIsAdmin, useModels } from "../lib/queries";

/** A copyable code sample; the clipboard button is the only interactive part. */
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
  const models = useModels();
  const origin = window.location.origin;
  const openai = `${origin}/v1`;
  const example =
    models.data?.find((model) => model.enabled && model.available && model.ref.includes("claude")) ??
    models.data?.find((model) => model.enabled && model.available);
  const modelRef = example?.ref ?? "provider/model";
  const key = "sk-rr-...";

  return (
    <>
      <PageHeader
        title="Quick start"
        subtitle="Create a key, pick a model, and point any OpenAI- or Anthropic-compatible tool at this gateway."
        actions={
          isAdmin ? (
            <Button variant="primary" onClick={() => navigate("/access/keys")}>
              create API key
            </Button>
          ) : (
            <Badge>ask an administrator for a key</Badge>
          )
        }
      />

      <div className="grid gap-3 sm:grid-cols-3">
        <Step
          number={1}
          title="Create an API key"
          body="The plaintext is shown once. Store it in the tool's secret field, not in a repository."
          action="Open API keys"
          onClick={() => navigate(isAdmin ? "/access/keys" : "/account")}
        />
        <Step
          number={2}
          title="Choose a model"
          body="Use the model ID exactly as the registry shows it, for example provider/model."
          action="Browse models"
          onClick={() => navigate("/models")}
        />
        <Step
          number={3}
          title="Configure a tool"
          body="Use the base URL for the protocol the tool speaks, then paste the key."
          action="See tool guides"
          onClick={() => document.getElementById("tool-guides")?.scrollIntoView({ behavior: "smooth" })}
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

      <section id="tool-guides" className="mt-6 scroll-mt-4">
        <div className="mb-3 flex flex-wrap items-baseline justify-between gap-2">
          <div>
            <h2 className="text-heading font-semibold text-fg">Connect a tool</h2>
            <p className="mt-0.5 text-small text-muted">
              Choose the protocol the tool speaks; no gateway-side SDK is required.
            </p>
          </div>
          <Link
            to="/providers"
            className="text-small font-medium text-[var(--color-accent-text)] hover:underline"
          >
            Manage provider accounts →
          </Link>
        </div>
        <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
          <Card className="flex flex-col gap-3 p-4">
            <div>
              <h3 className="text-body font-semibold">Claude Code</h3>
              <p className="mt-0.5 text-small text-muted">
                Anthropic base URL; the CLI appends <code>/v1/messages</code>.
              </p>
            </div>
            <Sample
              label="shell"
              code={`ANTHROPIC_BASE_URL=${origin}\nANTHROPIC_AUTH_TOKEN=${key}\nclaude`}
            />
          </Card>

          <Card className="flex flex-col gap-3 p-4">
            <div>
              <h3 className="text-body font-semibold">OpenAI SDK</h3>
              <p className="mt-0.5 text-small text-muted">
                Python or Node: set the client base URL and the gateway key.
              </p>
            </div>
            <Sample
              label="python"
              code={`from openai import OpenAI\n\nclient = OpenAI(\n    base_url="${openai}",\n    api_key="${key}",\n)`}
            />
          </Card>

          <Card className="flex flex-col gap-3 p-4">
            <div>
              <h3 className="text-body font-semibold">OMP</h3>
              <p className="mt-0.5 text-small text-muted">
                Add an OpenAI-compatible provider with this base URL.
              </p>
            </div>
            <Sample
              label="provider"
              code={`base_url = "${openai}"\napi_key = "${key}"\nmodel = "${modelRef}"`}
            />
          </Card>

          <Card className="flex flex-col gap-3 p-4">
            <div>
              <h3 className="text-body font-semibold">DeepSeek harness</h3>
              <p className="mt-0.5 text-small text-muted">
                Point its OpenAI-compatible client at the gateway.
              </p>
            </div>
            <Sample
              label="environment"
              code={`OPENAI_BASE_URL=${openai}\nOPENAI_API_KEY=${key}\nOPENAI_MODEL=${modelRef}`}
            />
          </Card>

          <Card className="flex flex-col gap-3 p-4">
            <div>
              <h3 className="text-body font-semibold">Codex CLI</h3>
              <p className="mt-0.5 text-small text-muted">
                Declare a custom OpenAI-compatible provider with this base URL.
              </p>
            </div>
            <Sample
              label="config.toml"
              code={`base_url = "${openai}"\napi_key = "${key}"`}
            />
          </Card>

          <Card className="flex flex-col gap-3 p-4">
            <div>
              <h3 className="text-body font-semibold">Cursor / Cline</h3>
              <p className="mt-0.5 text-small text-muted">
                Choose OpenAI Compatible, then paste the base URL and key.
              </p>
            </div>
            <Sample
              label="provider"
              code={`Base URL: ${openai}\nAPI key: ${key}`}
            />
          </Card>

          <Card className="flex flex-col gap-3 p-4">
            <div>
              <h3 className="text-body font-semibold">OpenCode</h3>
              <p className="mt-0.5 text-small text-muted">
                Add an OpenAI-compatible provider; all requests carry the gateway key.
              </p>
            </div>
            <Sample
              label="opencode.json"
              code={`"baseURL": "${openai}",\n"apiKey": "${key}"`}
            />
          </Card>

          <Card className="flex flex-col gap-3 p-4">
            <div>
              <h3 className="text-body font-semibold">Anthropic SDK</h3>
              <p className="mt-0.5 text-small text-muted">
                Use the origin as the base URL; the SDK appends the Messages path.
              </p>
            </div>
            <Sample
              label="python"
              code={`from anthropic import Anthropic\n\nclient = Anthropic(\n    base_url="${origin}",\n    api_key="${key}",\n)`}
            />
          </Card>

          <Card className="flex flex-col gap-3 p-4">
            <div>
              <h3 className="text-body font-semibold">Any OpenAI-compatible app</h3>
              <p className="mt-0.5 text-small text-muted">
                If it asks for a base URL, this is the one. The API key is the gateway key,
                not the upstream provider key.
              </p>
            </div>
            <CopyChip value={openai} label="OpenAI base URL" className="w-full" />
          </Card>
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
