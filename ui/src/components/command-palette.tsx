import { useCallback, useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Graph, Moon, Plugs, Plus, Sun } from "@phosphor-icons/react";
import { useNavigate } from "react-router-dom";
import { cx } from "../lib/cx";
import { useFeatureFlag } from "../lib/flags";
import { useModels } from "../lib/queries";

export type PaletteTheme = "light" | "dark";

export type PaletteNavItem = {
  to: string;
  label: string;
  icon: ReactNode;
  group: string;
  /** Hidden from members and viewers; the matching route is guarded too. */
  adminOnly?: boolean;
};

export type PaletteEntry = {
  id: string;
  section: string;
  label: string;
  hint?: string;
  icon?: ReactNode;
  run: () => void;
};

const iconGlyph = (Icon: typeof Plus) => <Icon size={17} weight="regular" aria-hidden />;

/**
 * Global `⌘K` / `Ctrl+K` command palette with WAI-ARIA combobox + listbox keyboard
 * navigation (`ArrowDown`, `ArrowUp`, `Home`, `End`, `Enter`, `aria-activedescendant`).
 */
export function CommandPalette({
  open,
  onClose,
  nav,
  isAdmin,
  theme,
  onToggleTheme,
}: {
  open: boolean;
  onClose: () => void;
  nav: PaletteNavItem[];
  isAdmin: boolean;
  theme: PaletteTheme;
  onToggleTheme: () => void;
}) {
  const [query, setQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);
  const navigate = useNavigate();
  const models = useModels();
  const showQuickActions = useFeatureFlag("commandPaletteActions");
  const listboxId = useId();
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (open) {
      setQuery("");
      setActiveIndex(0);
    }
  }, [open]);

  const go = useCallback(
    (path: string) => {
      navigate(path);
      onClose();
    },
    [navigate, onClose],
  );

  const results = useMemo(() => {
    const q = query.trim().toLowerCase();
    const actionSpecs: Array<{
      id: string;
      label: string;
      hint: string;
      keywords: string;
      icon: ReactNode;
      adminOnly?: boolean;
      run: () => void;
    }> = [
      {
        id: "action-create-key",
        label: "Create API key",
        hint: "/access/keys",
        keywords: "new key token secret access",
        icon: iconGlyph(Plus),
        adminOnly: true,
        run: () => go("/access/keys"),
      },
      {
        id: "action-add-provider",
        label: "Add provider connection",
        hint: "/providers",
        keywords: "connect upstream account oauth",
        icon: iconGlyph(Plugs),
        run: () => go("/providers"),
      },
      {
        id: "action-topology",
        label: "Live routing topology",
        hint: "/routing/topology",
        keywords: "map graph live upstream traffic",
        icon: iconGlyph(Graph),
        run: () => go("/routing/topology"),
      },
      {
        id: "action-theme",
        label: "Toggle theme",
        hint: theme === "dark" ? "Switch to light" : "Switch to dark",
        keywords: "dark light mode appearance color",
        icon: theme === "dark" ? <Sun size={17} aria-hidden /> : <Moon size={17} aria-hidden />,
        run: () => {
          onToggleTheme();
          onClose();
        },
      },
    ];

    const actions: PaletteEntry[] = showQuickActions
      ? actionSpecs
          .filter((spec) => isAdmin || !spec.adminOnly)
          .filter(
            (spec) =>
              !q ||
              spec.label.toLowerCase().includes(q) ||
              spec.keywords.toLowerCase().includes(q) ||
              spec.hint.toLowerCase().includes(q),
          )
          .map(({ id, label, hint, icon, run }) => ({
            id,
            section: "Quick Actions",
            label,
            hint,
            icon,
            run,
          }))
      : [];

    const pageHits = nav
      .filter(
        (n) =>
          (isAdmin || !n.adminOnly) &&
          (!q ||
            n.label.toLowerCase().includes(q) ||
            n.group.toLowerCase().includes(q) ||
            n.to.toLowerCase().includes(q)),
      )
      .slice(0, 8);
    // Group headings are the nav's own group labels: one source, no second taxonomy.
    const pageGroups: Array<{ title: string; items: PaletteEntry[] }> = [];
    for (const p of pageHits) {
      const entry: PaletteEntry = {
        id: `page-${p.to}`,
        section: p.group,
        label: p.label,
        hint: p.to,
        icon: p.icon,
        run: () => go(p.to),
      };
      const group = pageGroups.find((candidate) => candidate.title === p.group);
      if (group) group.items.push(entry);
      else pageGroups.push({ title: p.group, items: [entry] });
    }
    const pages = pageGroups.flatMap((group) => group.items);

    const modelHits: PaletteEntry[] = (models.data ?? [])
      .filter(
        (m) =>
          q &&
          (m.ref.toLowerCase().includes(q) || (m.display?.toLowerCase().includes(q) ?? false)),
      )
      .slice(0, 6)
      .map((m) => ({
        id: `model-${m.ref}`,
        section: "Models",
        label: m.ref,
        hint: m.provider,
        run: () => go(`/models?q=${encodeURIComponent(m.ref)}`),
      }));

    const all = [...actions, ...pages, ...modelHits];
    return { actions, pageGroups, pages, modelHits, all };
  }, [nav, query, models.data, isAdmin, theme, showQuickActions, go, onToggleTheme, onClose]);

  const clampedIndex =
    results.all.length === 0 ? -1 : Math.min(Math.max(0, activeIndex), results.all.length - 1);
  const activeEntry = clampedIndex >= 0 ? results.all[clampedIndex] : undefined;
  const optionId = useCallback(
    (id: string) => `${listboxId}-opt-${id.replace(/[^a-zA-Z0-9_-]/g, "-")}`,
    [listboxId],
  );

  useEffect(() => {
    if (!open || !activeEntry) return;
    const el = listRef.current?.querySelector<HTMLElement>(`#${CSS.escape(optionId(activeEntry.id))}`);
    el?.scrollIntoView({ block: "nearest" });
  }, [open, clampedIndex, activeEntry, optionId]);

  const onInputKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    const count = results.all.length;
    if (count === 0) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActiveIndex((prev) => Math.min(count - 1, prev + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActiveIndex((prev) => Math.max(0, prev - 1));
    } else if (e.key === "Home") {
      e.preventDefault();
      setActiveIndex(0);
    } else if (e.key === "End") {
      e.preventDefault();
      setActiveIndex(count - 1);
    } else if (e.key === "Enter") {
      if (activeEntry) {
        e.preventDefault();
        activeEntry.run();
      }
    }
  };

  let runningIndex = 0;

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
    >
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-[var(--z-command)] bg-[var(--color-scrim-modal)]" />
        <Dialog.Content className="dialog-panel fixed left-1/2 top-[14vh] z-[var(--z-command)] w-full max-w-xl -translate-x-1/2 overflow-hidden rounded-[var(--radius-overlay)] border border-line-strong bg-surface shadow-[var(--shadow-modal)] focus:outline-none">
          <Dialog.Title className="sr-only">Command palette</Dialog.Title>
          <Dialog.Description className="sr-only">
            Run quick actions or jump to a dashboard page or model.
          </Dialog.Description>
          <input
            autoFocus
            role="combobox"
            aria-expanded={open}
            aria-controls={listboxId}
            aria-autocomplete="list"
            aria-activedescendant={activeEntry ? optionId(activeEntry.id) : undefined}
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setActiveIndex(0);
            }}
            onKeyDown={onInputKeyDown}
            placeholder="Run an action, or jump to a page or model…"
            aria-label="Search actions, pages and models"
            className="h-11 w-full border-b border-line bg-transparent px-4 text-lead text-fg placeholder:text-faint focus:outline-none"
          />
          <div
            ref={listRef}
            id={listboxId}
            role="listbox"
            aria-label="Command results"
            className="max-h-[50vh] overflow-y-auto p-2"
          >
            {results.actions.length > 0 ? (
              <PaletteSection title="Quick Actions">
                {results.actions.map((item) => {
                  const idx = runningIndex++;
                  return (
                    <PaletteResultItem
                      key={item.id}
                      id={optionId(item.id)}
                      item={item}
                      active={idx === clampedIndex}
                      monoHint
                      onHover={() => setActiveIndex(idx)}
                      onSelect={item.run}
                    />
                  );
                })}
              </PaletteSection>
            ) : null}
            {results.pageGroups.map((group) => (
              <PaletteSection key={group.title} title={group.title}>
                {group.items.map((item) => {
                  const idx = runningIndex++;
                  return (
                    <PaletteResultItem
                      key={item.id}
                      id={optionId(item.id)}
                      item={item}
                      active={idx === clampedIndex}
                      monoHint
                      onHover={() => setActiveIndex(idx)}
                      onSelect={item.run}
                    />
                  );
                })}
              </PaletteSection>
            ))}
            {results.modelHits.length > 0 ? (
              <PaletteSection title="Models">
                {results.modelHits.map((item) => {
                  const idx = runningIndex++;
                  return (
                    <PaletteResultItem
                      key={item.id}
                      id={optionId(item.id)}
                      item={item}
                      active={idx === clampedIndex}
                      monoLabel
                      onHover={() => setActiveIndex(idx)}
                      onSelect={item.run}
                    />
                  );
                })}
              </PaletteSection>
            ) : null}
            {results.all.length === 0 ? (
              <div className="px-3 py-6 text-center text-body text-faint">no matches</div>
            ) : null}
          </div>
          <div className="flex items-center justify-between border-t border-line bg-surface-2 px-3 py-1.5 text-caption text-faint">
            <span>↑↓ navigate · Home/End jump · ↵ run</span>
            <span>Esc close</span>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

/** Grouped section heading inside the command palette listbox. */
export function PaletteSection({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="mb-1">
      <div className="px-2 py-1 text-caption uppercase tracking-[0.1em] text-faint">{title}</div>
      {children}
    </div>
  );
}

/** Single selectable option row inside a `PaletteSection`. */
export function PaletteResultItem({
  id,
  item,
  active,
  monoLabel = false,
  monoHint = false,
  onHover,
  onSelect,
}: {
  id: string;
  item: PaletteEntry;
  active: boolean;
  monoLabel?: boolean;
  monoHint?: boolean;
  onHover: () => void;
  onSelect: () => void;
}) {
  return (
    <button
      id={id}
      type="button"
      role="option"
      aria-selected={active}
      onMouseEnter={onHover}
      onClick={onSelect}
      className={cx(
        "flex w-full items-center justify-between gap-2 rounded-md px-2.5 py-2 text-left text-body transition-colors",
        active ? "bg-[var(--color-selected)] font-medium text-fg" : "text-muted hover:bg-surface-2 hover:text-fg",
      )}
    >
      {monoLabel ? (
        <span className="truncate font-mono text-small">{item.label}</span>
      ) : (
        <span className="flex min-w-0 items-center gap-2.5">
          {item.icon ? <span className="shrink-0">{item.icon}</span> : null}
          <span className="truncate">{item.label}</span>
        </span>
      )}
      {item.hint ? (
        <span className={cx("shrink-0 text-caption text-faint", monoHint && "font-mono")}>
          {item.hint}
        </span>
      ) : null}
    </button>
  );
}
