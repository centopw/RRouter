import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { CaretDown, CaretUp, CaretUpDown } from "@phosphor-icons/react";
import { Checkbox } from "./atoms/Checkbox";
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

export type DataTableSort = {
  key: string;
  direction: "asc" | "desc";
};

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
  /** Enables clickable sorting on this column header (`aria-sort`). */
  sortable?: boolean;
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
const SKELETON_ROWS = 5;
const SKELETON_WIDTHS = ["w-32", "w-24", "w-40", "w-20", "w-28"];
const STICKY_SHADOW = "shadow-[var(--shadow-sticky-col)]";

function useDocumentDensity(): "compact" | "comfortable" {
  const read = (): "compact" | "comfortable" =>
    document.documentElement.dataset.density === "compact" ? "compact" : "comfortable";
  const [density, setDensity] = useState<"compact" | "comfortable">(read);
  useEffect(() => {
    const observer = new MutationObserver(() => setDensity(read()));
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-density"],
    });
    return () => observer.disconnect();
  }, []);
  return density;
}

function compareSortValues(a: unknown, b: unknown): number {
  const aEmpty = a === null || a === undefined || a === "";
  const bEmpty = b === null || b === undefined || b === "";
  if (aEmpty && bEmpty) return 0;
  if (aEmpty) return 1;
  if (bEmpty) return -1;
  if (typeof a === "number" && typeof b === "number") return a - b;
  if (typeof a === "boolean" && typeof b === "boolean") return Number(a) - Number(b);
  return String(a).localeCompare(String(b), undefined, { numeric: true, sensitivity: "base" });
}

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
  density,
  sort,
  onSort,
  sortValue,
  selectedKey,
  selectable = false,
  selectedKeys,
  onSelectionChange,
  bulkActions,
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
  /** Overrides the global density preference for this table. */
  density?: "compact" | "comfortable";
  /** Controlled sort state; omit for built-in client-side sorting. */
  sort?: DataTableSort | null;
  onSort?: (next: DataTableSort | null) => void;
  /** Optional sort value extractor for uncontrolled client-side sorting. */
  sortValue?: (row: T, columnKey: string) => string | number | boolean | null | undefined;
  /** Highlights the matching row with `--color-selected`. */
  selectedKey?: string | number;
  /** Adds a leading checkbox column and floating bulk action bar when rows are selected. */
  selectable?: boolean;
  selectedKeys?: ReadonlySet<string> | string[];
  onSelectionChange?: (keys: string[]) => void;
  /** Rendered inside the floating bulk action bar when one or more rows are selected. */
  bulkActions?: ReactNode | ((selectedKeys: string[], clear: () => void) => ReactNode);
}) {
  const docDensity = useDocumentDensity();
  const effectiveDensity = density ?? docDensity;
  const headerCellPad = effectiveDensity === "compact" ? "px-3 py-1.5 text-caption" : "px-4 py-2.5 text-small";
  const bodyCellPad = effectiveDensity === "compact" ? "px-3 py-1.5 text-small" : "px-4 py-2.5 text-body";

  // Sort state: controlled wins, otherwise built-in client-side sorting on `sortable` columns.
  const sortControlled = sort !== undefined || onSort !== undefined;
  const [autoSort, setAutoSort] = useState<DataTableSort | null>(null);
  const activeSort = sortControlled ? (sort ?? null) : autoSort;

  const toggleSort = (key: string) => {
    const next: DataTableSort | null =
      activeSort?.key !== key
        ? { key, direction: "asc" }
        : activeSort.direction === "asc"
          ? { key, direction: "desc" }
          : null;
    if (onSort) onSort(next);
    else setAutoSort(next);
  };

  // Selection state: controlled wins when `selectedKeys` is provided.
  const selectionControlled = selectedKeys !== undefined;
  const [autoSelected, setAutoSelected] = useState<string[]>([]);
  const selectedSet = useMemo(() => {
    if (!selectionControlled) return new Set(autoSelected);
    return selectedKeys instanceof Set ? new Set(selectedKeys) : new Set(selectedKeys);
  }, [selectionControlled, selectedKeys, autoSelected]);

  const commitSelection = (nextSet: Set<string>) => {
    const next = [...nextSet];
    if (!selectionControlled) setAutoSelected(next);
    onSelectionChange?.(next);
  };

  const clearSelection = () => commitSelection(new Set());

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

  const sortedRows = useMemo(() => {
    if (!rows || sortControlled || !activeSort) return rows;
    const column = columns.find((col) => col.key === activeSort.key);
    if (!column) return rows;
    const factor = activeSort.direction === "asc" ? 1 : -1;
    return rows
      .map((row, index) => ({ row, index }))
      .sort((left, right) => {
        const aVal = sortValue
          ? sortValue(left.row, column.key)
          : (left.row as Record<string, unknown>)[column.key];
        const bVal = sortValue
          ? sortValue(right.row, column.key)
          : (right.row as Record<string, unknown>)[column.key];
        const cmp = compareSortValues(aVal, bVal);
        return cmp !== 0 ? cmp * factor : left.index - right.index;
      })
      .map((entry) => entry.row);
  }, [rows, sortControlled, activeSort, columns, sortValue]);

  const label = scrollLabel ?? caption ?? "table";
  const bounded = Boolean(maxBodyHeight);

  if (loading && (!rows || rows.length === 0)) {
    return (
      <div aria-busy="true">
        <span className="sr-only">Loading {label}…</span>
        <div
          className={cx("table-scroll overflow-x-auto", bounded && "overflow-y-auto")}
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
                {selectable ? (
                  <th
                    scope="col"
                    className={cx(
                      "w-10 border-b border-line bg-surface",
                      headerCellPad,
                      stickyHeader && "sticky top-0 z-20",
                    )}
                  >
                    <span className="sr-only">Select</span>
                  </th>
                ) : null}
                {columns.map((column) => (
                  <th
                    key={column.key}
                    scope="col"
                    style={column.width ? { width: column.width } : undefined}
                    className={cx(
                      "border-b border-line bg-surface font-semibold text-ink-soft",
                      headerCellPad,
                      column.align === "right" && "text-right tnum",
                      stickyHeader && "sticky top-0 z-20",
                      column.key === stickyRight &&
                        cx("sticky right-0 z-30 border-l border-line bg-surface", STICKY_SHADOW),
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
              {Array.from({ length: SKELETON_ROWS }, (_, rowIdx) => (
                <tr key={rowIdx} aria-hidden="true">
                  {selectable ? (
                    <td className={cx("w-10 border-b border-[var(--color-rule)] align-middle", bodyCellPad)}>
                      <div className="h-4 w-4 animate-pulse rounded-[3px] bg-surface-3" />
                    </td>
                  ) : null}
                  {columns.map((column, colIdx) => {
                    const widthClass =
                      column.key === stickyRight || column.key === "actions"
                        ? "w-14"
                        : column.align === "right"
                          ? "w-16"
                          : SKELETON_WIDTHS[(rowIdx + colIdx) % SKELETON_WIDTHS.length];
                    return (
                      <td
                        key={column.key}
                        style={column.width ? { width: column.width } : undefined}
                        className={cx(
                          "border-b border-[var(--color-rule)] align-middle",
                          bodyCellPad,
                          column.align === "right" && "text-right tnum",
                          column.key === stickyRight &&
                            cx("sticky right-0 z-10 border-l border-line bg-surface", STICKY_SHADOW),
                          column.className,
                        )}
                      >
                        <div
                          className={cx(
                            "h-4 max-w-full animate-pulse rounded bg-surface-3",
                            widthClass,
                            column.align === "right" && "ml-auto",
                          )}
                        />
                      </td>
                    );
                  })}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
    );
  }

  if (!sortedRows) {
    return (
      <div className="px-5 py-10 text-center text-body text-faint">
        —
      </div>
    );
  }
  if (sortedRows.length === 0) {
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

  const pageRows = controlled
    ? sortedRows
    : sortedRows.slice((page - 1) * autoSize, page * autoSize);
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

  const pageRowKeys = pageRows.map((row) => rowKey(row));
  const selectedOnPageCount = selectable
    ? pageRowKeys.filter((key) => selectedSet.has(key)).length
    : 0;
  const allOnPageSelected = selectable && pageRowKeys.length > 0 && selectedOnPageCount === pageRowKeys.length;
  const someOnPageSelected = selectable && selectedOnPageCount > 0 && !allOnPageSelected;
  const selectedList = [...selectedSet];

  const toggleAllOnPage = (next: boolean) => {
    const updated = new Set(selectedSet);
    for (const key of pageRowKeys) {
      if (next) updated.add(key);
      else updated.delete(key);
    }
    commitSelection(updated);
  };

  const toggleOne = (key: string, next: boolean) => {
    const updated = new Set(selectedSet);
    if (next) updated.add(key);
    else updated.delete(key);
    commitSelection(updated);
  };

  const selectedKeyString =
    selectedKey !== undefined && selectedKey !== null ? String(selectedKey) : null;

  return (
    <div className="relative">
      <div
        className={cx("table-scroll overflow-x-auto", bounded && "overflow-y-auto")}
        tabIndex={0}
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
              {selectable ? (
                <th
                  scope="col"
                  className={cx(
                    "w-10 border-b border-line bg-surface",
                    headerCellPad,
                    stickyHeader && "sticky top-0 z-20",
                  )}
                >
                  <Checkbox
                    checked={allOnPageSelected}
                    indeterminate={someOnPageSelected}
                    onChange={toggleAllOnPage}
                    ariaLabel="Select all rows on page"
                  />
                </th>
              ) : null}
              {columns.map((column) => {
                const isSorted = column.sortable && activeSort?.key === column.key;
                const ariaSort = column.sortable
                  ? isSorted
                    ? activeSort.direction === "asc"
                      ? "ascending"
                      : "descending"
                    : "none"
                  : undefined;
                return (
                  <th
                    key={column.key}
                    scope="col"
                    aria-sort={ariaSort}
                    style={column.width ? { width: column.width } : undefined}
                    className={cx(
                      "border-b border-line bg-surface font-semibold text-ink-soft",
                      headerCellPad,
                      column.align === "right" && "text-right tnum",
                      stickyHeader && "sticky top-0 z-20",
                      column.key === stickyRight &&
                        cx("sticky right-0 z-30 border-l border-line bg-surface", STICKY_SHADOW),
                      column.className,
                    )}
                  >
                    {column.srOnlyTitle ? (
                      <span className="sr-only">{column.title}</span>
                    ) : column.sortable ? (
                      <button
                        type="button"
                        onClick={() => toggleSort(column.key)}
                        className={cx(
                          "inline-flex items-center gap-1 rounded-[var(--radius-input)] font-semibold text-ink-soft transition-colors hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text",
                          column.align === "right" && "ml-auto flex-row-reverse",
                        )}
                      >
                        <span>{column.title}</span>
                        {isSorted ? (
                          activeSort.direction === "asc" ? (
                            <CaretUp size={12} weight="bold" aria-hidden className="text-accent-text" />
                          ) : (
                            <CaretDown size={12} weight="bold" aria-hidden className="text-accent-text" />
                          )
                        ) : (
                          <CaretUpDown size={12} aria-hidden className="text-faint" />
                        )}
                      </button>
                    ) : (
                      column.title
                    )}
                  </th>
                );
              })}
            </tr>
          </thead>
          <tbody>
            {pageRows.map((row) => {
              const key = rowKey(row);
              const isHighlighted =
                (selectedKeyString !== null && key === selectedKeyString) ||
                (selectable && selectedSet.has(key));
              return (
                <tr
                  key={key}
                  data-selected={isHighlighted || undefined}
                  aria-selected={isHighlighted || undefined}
                  className="group"
                >
                  {selectable ? (
                    <td
                      className={cx(
                        "w-10 border-b border-[var(--color-rule)] align-middle",
                        bodyCellPad,
                        isHighlighted
                          ? "bg-[var(--color-selected)]"
                          : "bg-surface group-hover:bg-[var(--color-row-hover)]",
                      )}
                    >
                      <Checkbox
                        checked={selectedSet.has(key)}
                        onChange={(next) => toggleOne(key, next)}
                        ariaLabel={`Select row ${key}`}
                      />
                    </td>
                  ) : null}
                  {columns.map((column) => {
                    const content = column.render(row);
                    const cellBg = isHighlighted
                      ? "bg-[var(--color-selected)]"
                      : column.key === stickyRight
                        ? "bg-surface group-hover:bg-[var(--color-row-hover)]"
                        : "group-hover:bg-[var(--color-row-hover)]";
                    const shared = cx(
                      "border-b border-[var(--color-rule)] align-middle text-fg",
                      bodyCellPad,
                      column.align === "right" && "text-right tnum",
                      column.key === stickyRight &&
                        cx("sticky right-0 z-10 border-l border-line", STICKY_SHADOW),
                      cellBg,
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
                        className={shared}
                      >
                        {content}
                      </td>
                    );
                  })}
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      {selectable && selectedList.length > 0 ? (
        <div
          role="region"
          aria-label="Bulk actions"
          className="sticky bottom-3 z-[var(--z-popover)] mx-auto mt-3 flex w-fit max-w-full flex-wrap items-center gap-2.5 rounded-full border border-line-strong bg-surface px-4 py-1.5 shadow-[var(--shadow-popover)]"
        >
          <span className="text-small font-semibold text-fg tnum">
            {selectedList.length} selected
          </span>
          {bulkActions ? (
            <div className="flex flex-wrap items-center gap-1.5 border-l border-line pl-2.5">
              {typeof bulkActions === "function"
                ? bulkActions(selectedList, clearSelection)
                : bulkActions}
            </div>
          ) : null}
          <button
            type="button"
            onClick={clearSelection}
            className="rounded-full border border-line bg-surface-2 px-2.5 py-1 text-caption font-medium text-muted transition-colors hover:border-line-strong hover:bg-surface-3 hover:text-fg"
          >
            Clear
          </button>
        </div>
      ) : null}
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
