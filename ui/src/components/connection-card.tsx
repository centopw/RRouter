import { Link } from "react-router-dom";
import { Card, CopyChip } from "./ui";

/**
 * The two base URLs every client needs. Shown on the models and keys pages so an
 * operator never has to derive the Anthropic/OpenAI split from the endpoint list.
 */
export function ConnectionCard({ showQuickStartLink = true }: { showQuickStartLink?: boolean }) {
  const origin = window.location.origin;
  const openai = `${origin}/v1`;
  return (
    <Card className="mb-4 overflow-hidden">
      <div className="flex flex-wrap items-start justify-between gap-3 border-b border-line px-5 py-3.5">
        <div>
          <h2 className="text-lead font-semibold">Connection details</h2>
          <p className="mt-0.5 text-small text-muted">
            OpenAI-compatible clients use <code>/v1</code>; Anthropic clients use the origin
            and append <code>/v1/messages</code>.
          </p>
        </div>
        {showQuickStartLink ? (
          <Link
            to="/quickstart"
            className="shrink-0 text-small font-medium text-[var(--color-accent-text)] hover:underline"
          >
            Quick start →
          </Link>
        ) : null}
      </div>
      <div className="grid gap-px bg-line sm:grid-cols-2">
        <div className="bg-surface px-5 py-3">
          <div className="text-caption text-faint">OpenAI base URL</div>
          <CopyChip
            value={openai}
            label="OpenAI base URL"
            className="mt-1.5 w-full"
          />
        </div>
        <div className="bg-surface px-5 py-3">
          <div className="text-caption text-faint">Anthropic base URL</div>
          <CopyChip
            value={origin}
            label="Anthropic base URL"
            className="mt-1.5 w-full"
          />
        </div>
      </div>
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t border-line bg-[var(--color-surface-2)]/45 px-5 py-2.5 text-caption text-faint">
        <span>
          auth: <code>Authorization: Bearer &lt;key&gt;</code> or <code>x-api-key</code>
        </span>
        <span className="ml-auto">keys are shown once when created</span>
      </div>
    </Card>
  );
}
