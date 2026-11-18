import type { ReactNode } from "react";
import { cx } from "../../lib/cx";

export type PageLayoutVariant = "standard" | "focused" | "auth";
export type PageDensity = "compact" | "comfortable";

const WIDTHS: Record<PageLayoutVariant, string> = {
  standard: "max-w-[1160px]",
  focused: "max-w-[880px]",
  auth: "max-w-[440px]",
};

const DENSITIES: Record<PageDensity, string> = {
  compact: "gap-4",
  comfortable: "gap-6",
};

/**
 * Shared page layout wrapper enforcing canonical column widths and vertical spacing.
 * Variants: `standard` (1160px), `focused` (880px), `auth` (440px).
 */
export function PageLayout({
  variant = "standard",
  density,
  className,
  children,
}: {
  variant?: PageLayoutVariant;
  density?: PageDensity;
  className?: string;
  children: ReactNode;
}) {
  return (
    <div
      data-density={density}
      className={cx(
        "mx-auto flex w-full flex-col",
        WIDTHS[variant],
        density ? DENSITIES[density] : "gap-[var(--comp-page-gap)]",
        className,
      )}
    >
      {children}
    </div>
  );
}
