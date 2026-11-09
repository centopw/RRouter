import { useEffect, useRef, useState, type ReactNode } from "react";
import { EmptyState, cx } from "./ui";

/**
 * One table for every page.
 *
 * `thead` sticks, the right-hand actions column sticks, and the scroll container is a
 * named, focusable region so a keyboard or screen-reader user can reach the overflow
 * without hunting for a scrollbar.
 *
 * Pagination: controlled wins. A caller that passes `pagination` slices its own rows
 * (server-side or ahead of time) and this only paints the footer. Without the prop the
 * table paginates itself at 25 rows a page, so no page can render rows without controls.
 */

export type Column<T> = {
  key: string;
  title: string;
  align?: "left" | "right";
  width?: string;
  render: (row: T) => ReactNode;
  /** Render the header text for screen readers only (an actions column). */
  srOnlyTitle?: boolean;
  /**
   * Extra classes for the header and every cell; a page hides a secondary column at a
   * breakpoint with `hidden xl:table-cell` instead of duplicating the markup.
   */
  className?: string;
};

export type Pagination = {
  /** 1-based. */
  page: number;
  size: number;
  /** Rows after filtering, not rows on this page. */
  total: number;
  /** Offered page sizes; the first is the default. */
  sizes?: number[];
  /** What is being counted, for "1–50 of 9,243 models". */
  label?: string;
  onPage: (page: number) => void;
  onSize: (size: number) => void;
};

const DEFAULT_SIZES = [25, 50, 100, 200];
/** Page size an unpaginated table starts with; the footer offers the others. */
const AUTO_PAGE_SIZE = 25;

export function DataTable<T>({
  columns,
  rows,
  rowKey,
  empty,
  loading,
  pagination,
  stickyHeader = true,
  maxBodyHeight,
  scrollLabel,
  caption,
  rowHeaderKey,
  stickyRight,
  tableLayout,
}: {
  columns: Array<Column<T>>;
  rows: T[] | undefined;
  rowKey: (row: T) => string;
  /** A node, not just a sentence: a filtered-empty state wants a "clear" action. */
  empty?: ReactNode;
  loading?: boolean;
  pagination?: Pagination;
  stickyHeader?: boolean;
  /** Constrains the scroll region; the header needs a scrollport to stick to. */
  maxBodyHeight?: string;
  /** Accessible name of the scroll region and the table. */
  scrollLabel?: string;
  caption?: string;
  /** The column whose cell becomes the row header (`<th scope="row">`). */
  rowHeaderKey?: string;
  /** The column that stays pinned to the right edge while the table scrolls. */
  stickyRight?: string;
  /**
   * `"fixed"` keeps the table at the container width instead of growing to the intrinsic
   * width of its widest cell (a long model id, a nowrap price). Columns without a `width`
   * share what is left. Default is the browser's `auto`.
   */
  tableLayout?: "auto" | "fixed";
}) {
  // Controlled pagination wins; otherwise the table owns page and size.
  const controlled = pagination !== undefined;
  const [autoPage, setAutoPage] = useState(1);
  const [autoSize, setAutoSize] = useState(AUTO_PAGE_SIZE);
  const total = rows?.length ?? 0;
  const autoLast = Math.max(1, Math.ceil(total / autoSize));
  const page = controlled ? pagination.page : Math.min(autoPage, autoLast);

  // A smaller dataset starts at the top; growth keeps the current page when it is still
  // valid. A size change also starts at the top, like every other table footer.
  const previousTotal = useRef(total);
  useEffect(() => {
    if (controlled) return;
    if (total < previousTotal.current) {
      setAutoPage(1);
    } else {
      setAutoPage((current) => Math.min(current, autoLast));
    }
    previousTotal.current = total;
  }, [controlled, total, autoLast]);
  useEffect(() => {
    if (!controlled) setAutoPage(1);
  }, [controlled, autoSize]);

  if (!rows) {
    return (
      <div className="px-5 py-10 text-center text-body text-faint">
        {loading ? "loading…" : "—"}
      </div>
    );
  }
  if (rows.length === 0) {
    // An empty page in controlled mode keeps its footer: the reader may need to page
    // back. An auto table with no rows has nothing to paginate and stays a plain empty
    // state.
    const footer = controlled && pagination.total > 0
      ? <PaginationBar pagination={pagination} />
      : null;
    return (
      <>
        <EmptyState>{empty ?? "nothing here yet"}</EmptyState>
        {footer}
      </>
    );
  }

  const label = scrollLabel ?? caption ?? "table";
  const pageRows = controlled
    ? rows
    : rows.slice((page - 1) * autoSize, page * autoSize);
  const footer: Pagination = pagination ?? {
    page,
    size: autoSize,
    total,
    label: scrollLabel ?? caption ?? "rows",
    onPage: setAutoPage,
    onSize: (next) => {
      setAutoSize(next);
      setAutoPage(1);
    },
  };

  // A bounded table is its own scrollport; an unbounded one must not be, or a page ends
  // up with two vertical scrollbars (the table's and the shell's).
  const bounded = Boolean(maxBodyHeight);

  return (
    <div>
      <div
        className={cx("table-scroll", bounded && "overflow-auto")}
        tabIndex={bounded ? 0 : undefined}
        role="region"
        aria-label={label}
        style={bounded ? { maxHeight: maxBodyHeight } : undefined}
      >
        <table
          className="w-full border-separate border-spacing-0 text-body"
          style={tableLayout ? { tableLayout } : undefined}
        >
          <caption className="sr-only">{caption ?? label}</caption>
          <thead className={stickyHeader ? "sticky top-0 z-20" : undefined}>
            <tr>
              {columns.map((column) => (
                <th
                  key={column.key}
                  scope="col"
                  style={column.width ? { width: column.width } : undefined}
                  className={cx(
                    "border-b border-line bg-surface px-4 py-2.5 text-small font-semibold text-ink-soft",
                    column.align === "right" && "text-right",
                    stickyHeader && "sticky top-0 z-20",
                    column.key === stickyRight &&
                      "sticky right-0 z-30 border-l border-line bg-surface shadow-[-8px_0_8px_-8px_rgba(28,43,66,0.16)]",
                    column.className,
                  )}
                >
                  {column.srOnlyTitle ? (
                    <span className="sr-only">{column.title}</span>
                  ) : (
                    column.title
                  )}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {pageRows.map((row) => (
              <tr key={rowKey(row)} className="group">
                {columns.map((column) => {
                  const content = column.render(row);
                  const shared = cx(
                    "border-b border-[var(--color-rule)] px-4 py-2.5 align-middle text-fg",
                    column.align === "right" && "text-right tnum",
                    column.key === stickyRight &&
                      "sticky right-0 z-10 border-l border-line bg-surface shadow-[-8px_0_8px_-8px_rgba(28,43,66,0.16)] group-hover:bg-[var(--color-row-hover)]",
                    column.className,
                  );
                  if (column.key === rowHeaderKey) {
                    return (
                      <th key={column.key} scope="row" className={cx(shared, "text-left font-normal")}>
                        {content}
                      </th>
                    );
                  }
                  return (
                    <td
                      key={column.key}
                      style={column.width ? { width: column.width } : undefined}
                      className={cx(shared, "group-hover:bg-[var(--color-row-hover)]")}
                    >
                      {content}
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {pagination !== undefined || total > 0 ? <PaginationBar pagination={footer} /> : null}
    </div>
  );
}

/** `1, 2, …, 8, 9, 10`: at most seven targets, gaps named rather than numbered. */
function pageWindow(page: number, last: number): Array<number | "gap"> {
  if (last <= 7) return Array.from({ length: last }, (_, index) => index + 1);
  const wanted = new Set<number>([1, last, page - 1, page, page + 1]);
  if (page <= 3) [2, 3, 4].forEach((n) => wanted.add(n));
  if (page >= last - 2) [last - 1, last - 2, last - 3].forEach((n) => wanted.add(n));
  const pages = [...wanted].filter((n) => n >= 1 && n <= last).sort((a, b) => a - b);
  const out: Array<number | "gap"> = [];
  for (const [index, number] of pages.entries()) {
    const previous = pages[index - 1];
    if (previous !== undefined && number - previous > 1) out.push("gap");
    out.push(number);
  }
  return out;
}

/** The footer under a table. Exported for pages that swap the table for cards on a phone
 *  and still owe the reader the same page controls. */
export function PaginationBar({ pagination }: { pagination: Pagination }) {
  const { page, size, total, label, onPage, onSize } = pagination;
  const sizes = pagination.sizes ?? DEFAULT_SIZES;
  const last = Math.max(1, Math.ceil(total / size));
  const first = total === 0 ? 0 : (page - 1) * size + 1;
  const shown = Math.min(page * size, total);
  const noun = label ?? "rows";

  return (
    <div className="flex flex-wrap items-center justify-between gap-3 border-t border-line px-4 py-2.5">
      <p className="text-caption text-muted tnum" aria-live="polite">
        {total === 0 ? `0 ${noun}` : `${first.toLocaleString()}–${shown.toLocaleString()} of ${total.toLocaleString()} ${noun}`}
      </p>
      <div className="flex flex-wrap items-center gap-3">
        <label className="flex items-center gap-1.5 text-caption text-muted">
          <span>Rows</span>
          <select
            value={size}
            onChange={(event) => onSize(Number(event.target.value))}
            aria-label={`${noun} per page`}
            className="h-7 rounded-[var(--radius-input)] border border-line bg-surface px-1.5 text-caption text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
          >
            {sizes.map((option) => (
              <option key={option} value={option}>
                {option}
              </option>
            ))}
          </select>
        </label>
        {last > 1 ? (
          <nav className="flex items-center gap-0.5" aria-label={`${noun} pages`}>
            <PageButton
              label="Previous page"
              disabled={page <= 1}
              onClick={() => onPage(page - 1)}
            >
              ‹
            </PageButton>
            {pageWindow(page, last).map((entry, index) =>
              entry === "gap" ? (
                <span key={`gap-${index}`} className="px-1 text-caption text-faint" aria-hidden>
                  …
                </span>
              ) : (
                <PageButton
                  key={entry}
                  label={`Page ${entry}`}
                  current={entry === page}
                  onClick={() => onPage(entry)}
                >
                  {entry}
                </PageButton>
              ),
            )}
            <PageButton
              label="Next page"
              disabled={page >= last}
              onClick={() => onPage(page + 1)}
            >
              ›
            </PageButton>
          </nav>
        ) : null}
      </div>
    </div>
  );
}

function PageButton({
  label,
  current = false,
  disabled = false,
  onClick,
  children,
}: {
  label: string;
  current?: boolean;
  disabled?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      aria-label={label}
      aria-current={current ? "page" : undefined}
      className={cx(
        "flex h-7 min-w-7 items-center justify-center rounded-[var(--radius-input)] px-1.5 text-caption tnum transition-colors",
        current
          ? "bg-accent font-semibold text-[var(--color-accent-ink)]"
          : "text-muted hover:bg-surface-3 hover:text-fg disabled:text-faint",
        disabled && "cursor-not-allowed",
      )}
    >
      {children}
    </button>
  );
}
