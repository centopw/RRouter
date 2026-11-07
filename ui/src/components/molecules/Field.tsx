import { type ReactNode } from "react";
import * as Label from "@radix-ui/react-label";
import { Text } from "../atoms";

export function Field({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <Label.Root className="flex min-w-0 flex-col gap-1.5">
      <Text as="span" variant="small" className="font-semibold text-ink-soft">{label}</Text>
      {children}
      {hint ? <Text as="span" variant="caption" tone="faint">{hint}</Text> : null}
    </Label.Root>
  );
}
