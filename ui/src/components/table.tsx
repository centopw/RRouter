import type { ReactNode } from "react";
import { EmptyState, cx } from "./ui";

/** One table for every page: fixed header, hover row, optional row actions. */

export type Column<T> = {
  key: string;
  title: string;
  align?: "left" | "right";
  width?: string;
  render: (row: T) => ReactNode;
};

export function DataTable<T>({
  columns,
  rows,
  rowKey,
  empty,
  loading,
}: {
  columns: Array<Column<T>>;
  rows: T[] | undefined;
  rowKey: (row: T) => string;
  empty?: string;
  loading?: boolean;
}) {
  if (!rows) {
    return <div className="px-5 py-10 text-center text-body text-faint">{loading ? "loading…" : "—"}</div>;
  }
  if (rows.length === 0) return <EmptyState>{empty ?? "nothing here yet"}</EmptyState>;

  return (
    <div className="overflow-x-auto">
      <table className="w-full border-collapse text-body">
        <thead>
          <tr className="border-b border-line text-left">
            {columns.map((c) => (
              <th
                key={c.key}
                scope="col"
                style={c.width ? { width: c.width } : undefined}
                className={cx(
                  "px-4 py-2.5 text-small font-semibold text-ink-soft",
                  c.align === "right" && "text-right",
                )}
              >
                {c.title}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={rowKey(row)} className="border-b border-[var(--color-rule)] last:border-0 hover:bg-[var(--color-row-hover)]">
              {columns.map((c) => (
                <td
                  key={c.key}
                  className={cx("px-4 py-2.5 align-middle text-fg", c.align === "right" && "text-right tnum")}
                >
                  {c.render(row)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
