/**
 * Pure rollups behind the Usage page. The range totals and provider/model rows come
 * from `/api/usage/summary`; account rows come from the same endpoint grouped by
 * connection, which is exact over the whole range. The request-log rollup stays as the
 * fallback for servers that do not report that grouping yet.
 */

import type { UsageEvent, UsageSummaryByConnection, UsageSummaryRow } from "../../lib/api";

export type Totals = {
  requests: number;
  errors: number;
  /** Input + output tokens, matching the Activity page. Cache tokens are excluded. */
  tokens: number;
  /** USD in micros, priced from catalog prices. */
  costMicros: number;
  /** Sum of request-weighted latency means; divide by `requests` for the mean. */
  latencySum: number;
};

export type ModelNode = { model: string; totals: Totals };

export type ProviderNode = { provider: string; totals: Totals; models: ModelNode[] };

export type AccountNode = {
  /** `null` is the "Unattributed" bucket for rows the log could not pin to an account. */
  connectionId: string | null;
  totals: Totals;
  models: ModelNode[];
  /** False for a known connection the source window has no requests for. */
  observed: boolean;
};

export type AccountRef = { id: string; provider: string };

export function emptyTotals(): Totals {
  return { requests: 0, errors: 0, tokens: 0, costMicros: 0, latencySum: 0 };
}

export function addTotals(target: Totals, source: Totals): Totals {
  target.requests += source.requests;
  target.errors += source.errors;
  target.tokens += source.tokens;
  target.costMicros += source.costMicros;
  target.latencySum += source.latencySum;
  return target;
}

function addParts(
  totals: Totals,
  parts: {
    requests: number;
    errors: number;
    input_tokens: number;
    output_tokens: number;
    cost_micros: number;
    latency_ms: number;
  },
): void {
  totals.requests += parts.requests;
  totals.errors += parts.errors;
  totals.tokens += parts.input_tokens + parts.output_tokens;
  totals.costMicros += parts.cost_micros;
  totals.latencySum += parts.latency_ms * parts.requests;
}

function addSummaryRow(totals: Totals, row: UsageSummaryRow): void {
  addParts(totals, { ...row, latency_ms: row.avg_latency_ms });
}

function addEventRow(totals: Totals, row: UsageEvent): void {
  addParts(totals, { ...row, requests: 1, errors: row.status >= 400 || row.error !== null ? 1 : 0 });
}

/** Request-weighted mean latency in milliseconds. */
export function latencyOf(totals: Totals): number {
  return totals.requests > 0 ? totals.latencySum / totals.requests : 0;
}

/** Error rate as a percentage, 0 when nothing was served. */
export function errorRateOf(totals: Totals): number {
  return totals.requests > 0 ? (totals.errors / totals.requests) * 100 : 0;
}

function bySpend(a: { totals: Totals }, b: { totals: Totals }): number {
  return b.totals.costMicros - a.totals.costMicros || b.totals.tokens - a.totals.tokens;
}

/** Summary rows → providers, ranked by spend then tokens, each with ranked models. */
export function buildProviderNodes(rows: UsageSummaryRow[]): ProviderNode[] {
  const providers = new Map<string, Map<string, Totals>>();
  for (const row of rows) {
    let models = providers.get(row.provider);
    if (!models) {
      models = new Map();
      providers.set(row.provider, models);
    }
    let totals = models.get(row.model);
    if (!totals) {
      totals = emptyTotals();
      models.set(row.model, totals);
    }
    addSummaryRow(totals, row);
  }
  const nodes: ProviderNode[] = [];
  for (const [provider, models] of providers) {
    const totals = emptyTotals();
    const modelNodes: ModelNode[] = [];
    for (const [model, modelTotals] of models) {
      addTotals(totals, modelTotals);
      modelNodes.push({ model, totals: modelTotals });
    }
    modelNodes.sort(
      (a, b) => b.totals.tokens - a.totals.tokens || a.model.localeCompare(b.model),
    );
    nodes.push({ provider, totals, models: modelNodes });
  }
  nodes.sort((a, b) => bySpend(a, b) || a.provider.localeCompare(b.provider));
  return nodes;
}

/**
 * True only when the server actually grouped by connection. A build that ignores
 * `group_by` returns ordinary summary rows with no `connection_id`, and treating every
 * one of those as unattributed would invent an account row.
 */
export function isConnectionGrouped(rows: UsageSummaryByConnection[]): boolean {
  return rows.every((row) => "connection_id" in row);
}

/** Connection-grouped summary rows → per-provider account rows with their models. */
export function buildAccountNodes(
  rows: UsageSummaryByConnection[],
): Map<string, AccountNode[]> {
  const byProvider = new Map<string, Map<string, AccountNode>>();
  for (const row of rows) {
    let accounts = byProvider.get(row.provider);
    if (!accounts) {
      accounts = new Map();
      byProvider.set(row.provider, accounts);
    }
    const key = row.connection_id ?? "";
    let account = accounts.get(key);
    if (!account) {
      account = {
        connectionId: row.connection_id,
        totals: emptyTotals(),
        models: [],
        observed: true,
      };
      accounts.set(key, account);
    }
    let model = account.models.find((node) => node.model === row.model);
    if (!model) {
      model = { model: row.model, totals: emptyTotals() };
      account.models.push(model);
    }
    addSummaryRow(model.totals, row);
    addSummaryRow(account.totals, row);
  }
  const out = new Map<string, AccountNode[]>();
  for (const [provider, accounts] of byProvider) {
    const nodes = [...accounts.values()];
    for (const node of nodes) {
      node.models.sort(
        (a, b) => b.totals.tokens - a.totals.tokens || a.model.localeCompare(b.model),
      );
    }
    nodes.sort(bySpend);
    out.set(provider, nodes);
  }
  return out;
}

function pushModel(account: AccountNode, event: UsageEvent): void {
  let model = account.models.find((node) => node.model === event.model);
  if (!model) {
    model = { model: event.model, totals: emptyTotals() };
    account.models.push(model);
  }
  addEventRow(model.totals, event);
  addEventRow(account.totals, event);
}

/** Request-log rows → per-provider account rows, including the null-connection bucket. */
export function buildObservedAccounts(events: UsageEvent[]): Map<string, AccountNode[]> {
  const byProvider = new Map<string, Map<string, AccountNode>>();
  for (const event of events) {
    let accounts = byProvider.get(event.provider);
    if (!accounts) {
      accounts = new Map();
      byProvider.set(event.provider, accounts);
    }
    const key = event.connection_id ?? "";
    let account = accounts.get(key);
    if (!account) {
      account = {
        connectionId: event.connection_id,
        totals: emptyTotals(),
        models: [],
        observed: true,
      };
      accounts.set(key, account);
    }
    pushModel(account, event);
  }
  const out = new Map<string, AccountNode[]>();
  for (const [provider, accounts] of byProvider) {
    const nodes = [...accounts.values()];
    for (const node of nodes) {
      node.models.sort(
        (a, b) => b.totals.tokens - a.totals.tokens || a.model.localeCompare(b.model),
      );
    }
    nodes.sort(bySpend);
    out.set(provider, nodes);
  }
  return out;
}

/**
 * Add every known connection as an account row, even when the sampled log has no
 * requests for it, so a configured-but-idle account still shows its quota.
 */
export function mergeKnownAccounts(
  observed: Map<string, AccountNode[]>,
  known: AccountRef[],
): Map<string, AccountNode[]> {
  const out = new Map<string, AccountNode[]>();
  for (const [provider, nodes] of observed) out.set(provider, [...nodes]);
  for (const ref of known) {
    let nodes = out.get(ref.provider);
    if (!nodes) {
      nodes = [];
      out.set(ref.provider, nodes);
    }
    if (nodes.some((node) => node.connectionId === ref.id)) continue;
    nodes.push({
      connectionId: ref.id,
      totals: emptyTotals(),
      models: [],
      observed: false,
    });
  }
  for (const nodes of out.values()) {
    nodes.sort(
      (a, b) => Number(b.observed) - Number(a.observed) || bySpend(a, b),
    );
  }
  return out;
}

