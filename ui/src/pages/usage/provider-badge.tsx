/**
 * Provider icon wrapper for `/usage`: guards against empty provider ids (`""` from
 * requests that failed before dispatch) so the surface never renders a blank dark square.
 */

import { WarningCircle } from "@phosphor-icons/react";
import { ProviderIcon } from "../../components/provider-icon";
import { cx } from "../../components/ui";

export function UsageProviderIcon({
  provider,
  size = 16,
  className,
}: {
  provider: string;
  size?: number;
  className?: string;
}) {
  if (provider.trim() === "") {
    return (
      <WarningCircle
        size={size}
        weight="fill"
        aria-hidden
        className={cx("shrink-0 text-[var(--color-neg)]", className)}
      />
    );
  }
  return <ProviderIcon provider={provider} size={size} className={className} />;
}
