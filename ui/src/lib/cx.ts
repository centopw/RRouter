/** Join conditional class names in source order; falsy entries are skipped. */
export function cx(...parts: Array<string | false | null | undefined>): string {
  return parts.filter(Boolean).join(" ");
}
