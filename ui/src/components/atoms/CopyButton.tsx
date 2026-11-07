import { Button } from "../atoms/Button";
import { useCopy } from "../atoms/useCopy";

export function CopyButton({
  value,
  label = "Copy",
  className,
}: {
  value: string;
  label?: string;
  className?: string;
}) {
  const [copied, copy] = useCopy();
  const done = copied === value;
  return (
    <Button variant="quiet" onClick={() => copy(value)} className={className}>
      {done ? "Copied" : label}
    </Button>
  );
}
