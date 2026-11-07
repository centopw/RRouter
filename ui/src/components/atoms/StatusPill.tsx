import { Badge } from "../atoms/Badge";

const STATUS_TEXT: Record<number, string> = {
  200: "OK",
  201: "Created",
  400: "Bad Request",
  401: "Unauthorized",
  402: "Payment Required",
  403: "Forbidden",
  404: "Not Found",
  408: "Timeout",
  413: "Payload Too Large",
  422: "Unprocessable",
  429: "Too Many Requests",
  499: "Client Closed",
  500: "Internal Error",
  502: "Bad Gateway",
  503: "Unavailable",
  504: "Gateway Timeout",
};

export function statusText(status: number): string {
  return STATUS_TEXT[status] ?? (status >= 500 ? "Server Error" : status >= 400 ? "Client Error" : "OK");
}

/** HTTP outcome as a pill: tinted by class, and always carrying the code in text. */
export function StatusPill({ status, title }: { status: number; title?: string }) {
  const ok = status >= 200 && status < 400;
  return (
    <Badge tone={ok ? "pos" : "neg"} title={title}>
      <span className="font-mono">
        {status} {statusText(status)}
      </span>
    </Badge>
  );
}
