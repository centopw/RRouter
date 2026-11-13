import { Link } from "react-router-dom";
import type { UseQueryResult } from "@tanstack/react-query";
import {
  Badge,
  Card,
  CardHeader,
  CopyIconButton,
  EmptyState,
  ErrorNote,
  Select,
} from "../../components/ui";
import { Toggle } from "../../components/atoms";
import type { Alias, Combo } from "../../lib/api";
import type { DraftEntry, DraftGroup } from "../../lib/layout-draft";

const linkClass =
  "inline-flex h-8 items-center rounded-lg border border-line bg-surface-2 px-3 text-body text-fg hover:border-line-strong";

/**
 * NAMES — the routable names a preset advertises: aliases and combos, with the group and
 * `/v1/models` visibility the layout stores. Editing the alias target or a combo's model
 * list stays on their own pages; this view never duplicates those editors.
 */
export function NamesView({
  isAdmin,
  aliases,
  combos,
  groups,
  entries,
  presetId,
  onAssign,
  onVisible,
}: {
  isAdmin: boolean;
  aliases: UseQueryResult<Alias[]>;
  combos: UseQueryResult<Combo[]>;
  groups: DraftGroup[];
  entries: DraftEntry[];
  presetId: string | null;
  onAssign: (ref: string, group: string | null) => void;
  onVisible: (ref: string, visible: boolean) => void;
}) {
  const error = aliases.error ?? combos.error;
  const custom = groups.filter((group) => !group.provider);
  const rows = [
    ...(aliases.data ?? []).map((alias) => ({
      ref: `rrouter/${alias.alias}`,
      name: alias.alias,
      detail: `alias → ${alias.target}`,
      kind: "alias" as const,
    })),
    ...(combos.data ?? []).map((combo) => ({
      ref: `rrouter/${combo.name}`,
      name: combo.name,
      detail: `combo · ${combo.models.length} models`,
      kind: "combo" as const,
    })),
  ];
  // Group and visibility are layout fields, so a preset must be loaded to change them.
  const editable = isAdmin && presetId !== null;

  return (
    <>
      {error ? (
        <div className="mb-3">
          <ErrorNote
            error={error}
            onRetry={() => {
              void aliases.refetch();
              void combos.refetch();
            }}
          />
        </div>
      ) : null}

      <Card>
        <CardHeader
          title="Aliases & combos"
          action={
            <span className="flex flex-wrap items-center justify-end gap-2">
              <span className="text-caption text-faint tnum">
                {rows.length.toLocaleString()} routable names · group and /v1/models visibility
              </span>
              <Link to="/routing/aliases" className={linkClass}>
                edit aliases
              </Link>
              <Link to="/routing/combos" className={linkClass}>
                edit combos
              </Link>
            </span>
          }
        />
        {rows.length === 0 ? (
          <EmptyState>
            No aliases or combos yet. Create one under{" "}
            <Link to="/routing/aliases" className="underline underline-offset-2">
              Aliases
            </Link>{" "}
            or{" "}
            <Link to="/routing/combos" className="underline underline-offset-2">
              Combos
            </Link>
            ; its group and visibility appear here.
          </EmptyState>
        ) : (
          <ul className="flex flex-col">
            {rows.map((row) => {
              const entry = entries.find((candidate) => candidate.ref === row.ref);
              const visible = entry?.visible !== false;
              const group = entry?.group ?? "";
              return (
                <li
                  key={row.ref}
                  className="flex flex-wrap items-center gap-2 border-b border-[var(--color-rule)] px-4 py-2 last:border-b-0"
                >
                  <Badge tone="neutral">{row.kind}</Badge>
                  <span className="min-w-0 flex-[1_1_12rem]">
                    <span className="block truncate font-mono text-small text-fg">{row.name}</span>
                    <span className="block truncate text-caption text-faint">{row.detail}</span>
                  </span>
                  <CopyIconButton value={row.name} label={`Copy ${row.kind} ${row.name}`} />
                  {editable ? (
                    <>
                      <div className="w-40 shrink-0">
                        <Select
                          value={group}
                          ariaLabel={`Group for ${row.ref}`}
                          onChange={(next) => onAssign(row.ref, next || null)}
                        >
                          <option value="">Ungrouped</option>
                          {custom.map((option) => (
                            <option key={option.id} value={option.id}>
                              {option.name}
                            </option>
                          ))}
                        </Select>
                      </div>
                      <Toggle
                        size="sm"
                        checked={visible}
                        label={`List ${row.ref} on /v1/models`}
                        onChange={(next) => onVisible(row.ref, next)}
                      />
                    </>
                  ) : (
                    <Badge tone={visible ? "neutral" : "warn"}>
                      {visible ? "listed" : "hidden"}
                    </Badge>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </Card>

      {!editable && isAdmin ? (
        <p className="mt-3 text-caption text-faint">
          Group and visibility are stored on a preset; no preset is loaded right now.
        </p>
      ) : null}
      {!editable && !isAdmin ? (
        <p className="mt-3 text-caption text-faint">
          Editing aliases, combos and groups is an admin action.
        </p>
      ) : null}
    </>
  );
}
