/**
 * How the dashboard reads a connection's `refusal` token — the reason the gateway last took
 * the account out of rotation (`connections.refusal`, migration V43).
 *
 * `refusal` is what separates an account a *provider* refused from one an operator switched
 * off, and the two need opposite human actions: a disabled account just needs enabling, a
 * refused one keeps being refused until its cause is gone. So every token carries its own
 * badge, its own explanation and the action it asks for, ordered by severity — and `NULL`
 * keeps the existing muted “disabled” look, which is the only case a bare toggle is honest.
 *
 * Presentation only: nothing here classifies a refusal, retries, or re-enables anything.
 */

/** Badge/banner colour. These are the shared `BadgeTone`/`AlertTone` names, on purpose. */
export type RefusalTone = "neg" | "warn" | "info";

export type RefusalView = {
  /** Wire token as the gateway stores it (`account_suspended`, …). */
  token: string;
  /** Badge text: short enough for the row's state column. */
  label: string;
  /** Noun phrase for the drawer banner and the re-enable confirmation. */
  title: string;
  tone: RefusalTone;
  /** What happened, in plain English. */
  description: string;
  /** What the operator does instead of re-enabling. */
  fix: string;
  /** Completes “Only re-enable after …”; the condition this build cannot fix by toggling. */
  before: string;
  /** 0 is the most severe. Unknown tokens rank after every token this build names. */
  severity: number;
};

/**
 * Every refusal this build names, most severe first. The order is the severity order: a
 * suspended account may be unrecoverable, an unentitled credential is recoverable by using
 * a different one, and a rejected client version is our own defect against a healthy account.
 */
const KNOWN: ReadonlyArray<Omit<RefusalView, "severity">> = [
  {
    token: "account_suspended",
    label: "suspended",
    title: "Suspended by the provider",
    tone: "neg",
    description:
      "The provider removed this account — Google answers a terminated account with “violation of " +
      "Terms of Service”. It left rotation because the account itself was refused, not because " +
      "anyone switched it off.",
    fix:
      "Appeal the suspension with the provider. If the account is gone for good, remove it here " +
      "and revoke RRouter's OAuth grant so the dead credential stops refreshing.",
    before: "the provider restores the account",
  },
  {
    token: "account_not_entitled",
    label: "not entitled",
    title: "Credential not entitled",
    tone: "warn",
    description:
      "This credential is barred for the client shape RRouter presents — the Claude subscription " +
      "gate refused it, or the account is drawing from third-party extra usage. Re-enabling " +
      "presents the same shape and gets refused again.",
    fix:
      "Attach a credential that is entitled for this provider, or move the account off " +
      "third-party extra usage.",
    before: "the credential is entitled for this client shape",
  },
  {
    token: "client_version_rejected",
    label: "client outdated",
    title: "Client version rejected",
    tone: "info",
    description:
      "The provider refused the client version this build declares: it is below the provider's " +
      "floor. The account itself is fine — nothing on it needs fixing.",
    fix: "Upgrade RRouter. This one is our defect, not the account's.",
    before: "RRouter is upgraded",
  },
];

/** Unknown token: a newer gateway recorded a refusal this build does not name. */
const UNKNOWN_TITLE = "Refused by the provider";
const UNKNOWN_LABEL = "refused";

const BY_TOKEN = new Map(
  KNOWN.map((refusal, severity): [string, RefusalView] => [refusal.token, { ...refusal, severity }]),
);

/**
 * The badge, copy and severity for a `refusal` value, or `null` when there is none.
 *
 * `null`, `undefined` and `""` all mean “not refused”: the field is optional, so a gateway
 * older than V43 omits it and a manual disable stores `NULL` — both keep the plain disabled
 * look. A token this build does not know is still a provider refusal, so it renders as a
 * generic refusal rather than silently reading as an operator disable.
 */
export function refusalView(token?: string | null): RefusalView | null {
  const value = token?.trim() ?? "";
  if (value === "") return null;
  const known = BY_TOKEN.get(value);
  if (known) return known;
  return {
    token: value,
    label: UNKNOWN_LABEL,
    title: UNKNOWN_TITLE,
    tone: "neg",
    severity: KNOWN.length,
    description:
      `The gateway recorded the refusal “${value}”, which this build does not name. The account ` +
      "left rotation for a provider-side reason, not an operator one.",
    fix: "Check the gateway logs for the upstream error before re-enabling.",
    before: "the cause is cleared",
  };
}

/**
 * The sentence a re-enable has to carry with it, or `null` for a plain manual disable where a
 * bare toggle is the whole action and nothing can go wrong.
 */
export function reenableConsequence(view: RefusalView | null): string | null {
  if (view === null) return null;
  return `Only re-enable after ${view.before} — a refused account stays refused.`;
}
